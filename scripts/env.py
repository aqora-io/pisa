#!/usr/bin/env python3
"""Builds the worker image of an environment: a Lean toolchain and the
libraries its workers build, which requests may import.

    scripts/env.py build environments/lean-4-35.toml [--repository R] [--push]
    scripts/env.py label docker/lean-project

An environment is a TOML file:

    toolchain = "leanprover/lean4:v4.35.0"
    imports = ["Mathlib"]   # the modules its workers build
    repl = "v4.35.0"        # the REPL's tag; by default the toolchain's

    [[require]]             # as in a lakefile.toml
    name = "mathlib"
    git = "https://github.com/leanprover-community/mathlib4.git"
    rev = "v4.35.0"

`build` writes its Lake project beside it, in a directory of its name: a
wrapper like docker/lean-project, whose lake-manifest.json Lake resolves in
Docker. It then builds docker/Dockerfile's worker on that project, tagged
`<repository>:<release>-<version>`, and prints the version and the image.
The version is the label the image's workers register (`<toolchain>-<hash of
the project's files>`), which a chart pool or a request names; the release
is pisa's, from Cargo.toml, as the image's worker is this checkout's.

Commit the file and the directory: the manifest pins what the version hashes.
A manifest is kept while the file says what it said, and resolved again with
`--update`, or when the file changes.
"""

import argparse
import hashlib
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def release():
    """pisa's version, which the workspace's crates share."""
    with open(ROOT / "Cargo.toml", "rb") as f:
        return tomllib.load(f)["workspace"]["package"]["version"]

# What a wrapper project is called, as docker/lean-project is.
PACKAGE, LIBRARY = "pisa_project", "PisaProject"


def label(project):
    """The version of the Lake project at `project`, as a worker of it
    computes it (`project_version` in crates/pisa-protocol/src/version.rs)."""
    toolchain = (project / "lean-toolchain").read_text().strip()
    hashed = hashlib.sha256()
    names = [n for n in ("lake-manifest.json", "lakefile.toml", "lakefile.lean") if (project / n).is_file()]
    names += sorted(
        p.name for p in project.iterdir() if p.is_file() and p.name.endswith(".lean") and p.name != "lakefile.lean"
    )
    for name in names:
        hashed.update(name.encode())
        hashed.update((project / name).read_bytes())
    return f"{toolchain.rsplit(':', 1)[-1]}-{hashed.hexdigest()[:8]}"


def quoted(value):
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'


def read_spec(path):
    spec = tomllib.loads(path.read_text())
    unknown = set(spec) - {"toolchain", "imports", "repl", "require"}
    if unknown:
        sys.exit(f"{path}: unknown keys: {', '.join(sorted(unknown))}")
    toolchain = spec.get("toolchain")
    if not isinstance(toolchain, str) or ":" not in toolchain:
        sys.exit(f'{path}: toolchain is required, as in a lean-toolchain file: "leanprover/lean4:v4.35.0"')
    imports = spec.get("imports", [])
    requires = spec.get("require", [])
    for require in requires:
        if "name" not in require or not all(isinstance(v, str) for v in require.values()):
            sys.exit(f"{path}: a [[require]] has a name, and strings for values: {require}")
    return {
        "toolchain": toolchain,
        "repl": spec.get("repl", toolchain.rsplit(":", 1)[-1]),
        "imports": imports,
        "requires": requires,
    }


def project_files(spec):
    """The files of the wrapper project, but for its manifest. They don't say
    which file they are from: environments that say the same are one."""
    lakefile = [
        "# Written by scripts/env.py from the environment's file: change that, not this.",
        f"name = {quoted(PACKAGE)}",
        f"defaultTargets = [{quoted(LIBRARY)}]",
    ]
    for require in spec["requires"]:
        lakefile += ["", "[[require]]"] + [f"{key} = {quoted(value)}" for key, value in require.items()]
    lakefile += ["", "# Importing this builds everything a request may import."]
    lakefile += ["[[lean_lib]]", f"name = {quoted(LIBRARY)}"]
    return {
        "lakefile.toml": "\n".join(lakefile) + "\n",
        "lean-toolchain": spec["toolchain"] + "\n",
        f"{LIBRARY}.lean": "".join(f"import {module}\n" for module in spec["imports"]),
    }


def docker(*args):
    command = ["docker", "build", "-f", "docker/Dockerfile", *args, "."]
    print("+ " + " ".join(str(a) for a in command), file=sys.stderr)
    if subprocess.run(command, cwd=ROOT, stdout=sys.stderr).returncode != 0:
        sys.exit("docker build failed")


def resolve(project, toolchain):
    """Writes the project's lake-manifest.json, as Lake resolves it."""
    with tempfile.TemporaryDirectory() as out:
        docker(
            "--target", "manifest", "--build-context", f"project={project}",
            "--build-arg", f"LEAN_TOOLCHAIN={toolchain}", "--output", out,
        )  # fmt: skip
        left = (Path(out) / "lean-toolchain").read_text().strip()
        if left != toolchain:
            sys.exit(f"what it requires is on {left}, not {toolchain}: make that its toolchain")
        (project / "lake-manifest.json").write_bytes((Path(out) / "lake-manifest.json").read_bytes())


def write_project(spec, project, update):
    files = project_files(spec)
    current = all((project / name).is_file() and (project / name).read_text() == text for name, text in files.items())
    if current and (project / "lake-manifest.json").is_file() and not update:
        return
    project.mkdir(parents=True, exist_ok=True)
    for stale in project.glob("*.lean"):
        stale.unlink()
    for name, text in files.items():
        (project / name).write_text(text)
    (project / "lake-manifest.json").unlink(missing_ok=True)
    resolve(project, spec["toolchain"])


def build(args):
    source = Path(args.spec)
    spec = read_spec(source)
    project = Path(args.out) if args.out else source.with_suffix("")
    write_project(spec, project, args.update)
    version = label(project)
    image = f"{args.repository}:{release()}-{version}"
    if not args.project_only:
        revision = args.revision or subprocess.run(
            ["git", "rev-parse", "HEAD"], cwd=ROOT, capture_output=True, text=True
        ).stdout.strip()
        docker(
            "--target", "worker", "--build-context", f"project={project.resolve()}",
            "--build-arg", f"LEAN_TOOLCHAIN={spec['toolchain']}", "--build-arg", f"REPL_REV={spec['repl']}",
            "--build-arg", f"REVISION={revision or 'unknown'}", "-t", image,
        )  # fmt: skip
        if args.push:
            subprocess.run(["docker", "push", image], check=True, stdout=sys.stderr)
    print(f"version={version}")
    if not args.project_only:
        print(f"image={image}")


def print_label(args):
    project = Path(args.project)
    if not (project / "lake-manifest.json").is_file():
        # As docker/lean-core: its image's workers say which, in GET /environments.
        note = "has no lake-manifest.json: its image resolves one, which makes its version another"
        print(f"{project} {note}", file=sys.stderr)
    print(label(project))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    b = commands.add_parser("build", help="write an environment's project and build its worker image")
    b.add_argument("spec", help="the environment's TOML file")
    b.add_argument("--out", help="where its project goes (by default beside the file, named as it is)")
    b.add_argument("--update", action="store_true", help="resolve its dependencies again")
    b.add_argument("--project-only", action="store_true", help="write the project and print its version, no image")
    b.add_argument("--repository", default="pisa-worker", help="the image's name, without its tag")
    b.add_argument("--revision", help="pisa's commit, which the image reports (by default HEAD)")
    b.add_argument("--push", action="store_true", help="push the image")
    b.set_defaults(run=build)
    v = commands.add_parser("label", help="print the version of a Lake project")
    v.add_argument("project")
    v.set_defaults(run=print_label)
    args = parser.parse_args()
    args.run(args)
