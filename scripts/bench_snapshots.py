#!/usr/bin/env python3
"""Measures the two candidate designs for environment snapshots at Mathlib
scale, against what pisa does today.

A Physlib subtree stands in for a library a formalizer has built: the import
closure of --target, one module per chunk, on an `import Mathlib` header.
Physlib's `Meta` tooling is imported compiled, as part of the header: it
registers environment extensions with `initialize`, which Lean cannot run in
the module that defines them, so it cannot be replayed as chunks. In a
container of the worker image, driving the REPL directly:

  M0  importing the header
  M1  today: elaborating the library prefix as one command, at four sizes
  M2  chunk replay: each chunk a command on the previous chunk's env
  M3  hits: a new lemma on an env the process already holds
  M4  pisa's axiom report on the chained env, whole and scoped to one chunk
  M5  compiled modules: each chunk built as a module, then imported

usage: python3 scripts/bench_snapshots.py [--out DIR] [--limit N] [--phases ...]
"""

import argparse
import io
import json
import re
import subprocess
import sys
import tarfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CONTAINER = "pisa-bench"
PROJECT = "/opt/project"
HEADER = "import Mathlib\nimport Lean.Elab.Command"
TOOLING = "Physlib.Meta."
HIT = "example : (2 : ℝ) + 2 = 4 := by norm_num"
# The image has no procps; find REPL processes through /proc.
REPL_PIDS = 'for p in /proc/[0-9]*; do [ "$(cat $p/comm 2>/dev/null)" = repl ] && echo ${p#/proc/}; done; true'
IMPORT = re.compile(r"^(?:public\s+|private\s+)?(?:meta\s+)?import\s+(?:all\s+)?(\S+)", re.M)


def sh(*args, input=None, check=True):
    r = subprocess.run(args, input=input, capture_output=True, check=False)
    if check and r.returncode != 0:
        sys.exit(f"{' '.join(args[:4])}…: {r.stderr.decode()[-2000:]}")
    return r.stdout


def dexec(cmd, input=None, check=True):
    return sh("docker", "exec", "-i", CONTAINER, "bash", "-c", cmd, input=input, check=check)


class Repl:
    """One REPL process in the container, speaking JSON separated by blank lines."""

    def __init__(self):
        before = set(dexec(REPL_PIDS).split())
        self.proc = subprocess.Popen(
            ["docker", "exec", "-i", CONTAINER, "bash", "-c", f"cd {PROJECT} && exec lake env repl"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1,
        )
        for _ in range(100):
            pids = set(dexec(REPL_PIDS).split()) - before
            if pids:
                self.pid = pids.pop().decode()
                break
            time.sleep(0.1)
        else:
            sys.exit("the REPL did not start")

    def send(self, command):
        self.proc.stdin.write(json.dumps(command) + "\n\n")
        self.proc.stdin.flush()
        lines = []
        while True:
            line = self.proc.stdout.readline()
            if not line:
                sys.exit("the REPL exited")
            if not line.strip() and lines:
                return json.loads("".join(lines))
            if line.strip():
                lines.append(line)

    def timed(self, command):
        t = time.time()
        r = self.send(command)
        return r, time.time() - t

    def memory(self):
        """Resident and peak memory of the REPL process, in MB."""
        status = dexec(f"cat /proc/{self.pid}/status").decode()
        kb = lambda k: int(re.search(rf"^{k}:\s+(\d+)", status, re.M).group(1))
        return {"rss_mb": kb("VmRSS") // 1024, "peak_mb": kb("VmHWM") // 1024}

    def close(self):
        self.proc.stdin.close()
        self.proc.wait()


def errors(r):
    return [m["data"] for m in r.get("messages", []) if m.get("severity") == "error"]


def sources():
    """Physlib's .lean files by module name, read from the image."""
    pkg = dexec(f"ls -d {PROJECT}/.lake/packages/*/ | grep -i physlib | head -1").decode().strip()
    tar = tarfile.open(fileobj=io.BytesIO(dexec(f"tar c -C {pkg} Physlib")))
    return {
        m.name[:-5].replace("/", "."): tar.extractfile(m).read().decode()
        for m in tar.getmembers()
        if m.isfile() and m.name.endswith(".lean")
    }


def closure(src, target):
    """Modules under `target` and everything they import from Physlib, dependencies first."""
    seen, order = set(), []

    def visit(m):
        if m in seen or m not in src:
            return
        seen.add(m)
        for i in IMPORT.findall(src[m]):
            visit(i)
        order.append(m)

    for m in sorted(src):
        if m.startswith(target + "."):
            visit(m)
    return order


SCOPE = re.compile(r"^\s*(?:(?:@\[[^\]]*\]\s*)?(?:public\s+)?(?:noncomputable\s+)?section\b\s*(\S*)|namespace\s+(\S+)|end\b\s*(\S*))")


def as_command(text):
    """A module's source as one REPL command. Imports go; the module system's
    `public` markers become plain Lean; the chunk is wrapped in a section, with
    any scopes it leaves open closed, so its `open`s and options end with it as
    they would at the end of a file."""
    out, stack = [], []
    for line in text.splitlines():
        s = line.strip()
        if s == "module" or IMPORT.match(line):
            continue
        line = re.sub(r"^(\s*)(?:@\[expose\]\s*)?public\s+(noncomputable\s+)?section\b", r"\1\2section", line)
        line = re.sub(r"^(\s*)public\s+(?=meta\b|def\b|theorem\b|lemma\b|abbrev\b|instance\b|structure\b|class\b|inductive\b|noncomputable\b|opaque\b|@\[)", r"\1", line)
        m = SCOPE.match(line)
        if m:
            if m.group(3) is not None and s.startswith("end"):
                if stack:
                    stack.pop()
            elif m.group(2):
                stack.append(m.group(2))
            else:
                stack.append(m.group(1) or "")
        out.append(line)
    closers = [f"end {n}".rstrip() for n in reversed(stack)]
    return "section\n" + "\n".join(out) + "\n" + "\n".join(closers) + "\nend\n"


def lines_of(text):
    return text.count("\n") + 1


def phase_chain(chunks, out):
    """M0, M2, M3 and M4 in one process."""
    repl = Repl()
    r, t = repl.timed({"cmd": HEADER})
    assert not errors(r), errors(r)
    header_env = r["env"]
    res = {"m0": {"seconds": t, **repl.memory()}, "m2": [], "replayed": 0}
    print(f"M0 header: {t:.1f}s {res['m0']}", flush=True)

    env, envs = header_env, []
    for i, (name, cmd) in enumerate(chunks):
        r, t = repl.timed({"cmd": cmd, "env": env})
        errs = errors(r)
        row = {"module": name, "lines": lines_of(cmd), "seconds": t, "error_count": len(errs), "errors": errs[:3], **repl.memory()}
        res["m2"].append(row)
        print(f"M2 {i + 1}/{len(chunks)} {name}: {t:.1f}s {row['rss_mb']} MB {len(errs)} errors", flush=True)
        # Keep going past errors: M1 resends the same text, so it meets the
        # same errors and the comparison stays fair. The report counts them.
        env = r["env"]
        envs.append(env)
    res["replayed"] = len(envs)
    if not envs:
        return res

    hits = []
    for _ in range(10):
        r, t = repl.timed({"cmd": HIT, "env": envs[-1]})
        hits.append({"seconds": t, "ok": not errors(r)})
    mid = envs[len(envs) // 2]
    r, t_mid = repl.timed({"cmd": HIT, "env": mid})
    r0, t_base = repl.timed({"cmd": HIT, "env": header_env})
    res["m3"] = {"final": hits, "mid": {"seconds": t_mid, "ok": not errors(r)},
                 "header_only": {"seconds": t_base, "ok": not errors(r0)}, **repl.memory()}
    print(f"M3 hits: {[round(h['seconds'], 2) for h in hits]}, mid {t_mid:.2f}s, header {t_base:.2f}s", flush=True)

    report = report_text(ROOT)
    whole, t_whole = repl.timed({"cmd": report, "env": envs[-1]})
    names_before = decl_names(repl.send({"cmd": report, "env": envs[-2]})) if len(envs) > 1 else []
    scoped = scoped_report(report, names_before)
    part, t_part = repl.timed({"cmd": scoped, "env": envs[-1]})
    res["m4"] = {
        "whole": {"seconds": t_whole, "decls": len(decl_names(whole))},
        "scoped": {"seconds": t_part, "decls": len(decl_names(part)), "excluded": len(names_before)},
    }
    print(f"M4 report: whole {t_whole:.1f}s/{res['m4']['whole']['decls']}, scoped {t_part:.1f}s/{res['m4']['scoped']['decls']}", flush=True)
    repl.close()
    return res


def report_text(root):
    """pisa's axiom report, read from the worker's source."""
    src = (root / "crates/pisa-worker/src/policy.rs").read_text()
    return re.search(r'pub const AXIOM_REPORT: &str = r#"(.*?)"#;', src, re.S).group(1)


def scoped_report(report, exclude):
    """The report, skipping the given names before computing their axioms."""
    names = "\\n".join(exclude)
    report = report.replace(
        "  let mut out : Array _root_.Lean.Json := #[]",
        "  let mut out : Array _root_.Lean.Json := #[]\n"
        f'  let skip : _root_.Std.HashSet _root_.Lean.Name := ("{names}".splitOn "\\n").foldl (fun s n => s.insert n.toName) {{}}',
    )
    return report.replace(
        "    if n.isInternal && !(_root_.Lean.isPrivateName n) then continue",
        "    if n.isInternal && !(_root_.Lean.isPrivateName n) then continue\n    if skip.contains n then continue",
    )


def decl_names(r):
    for m in r.get("messages", []):
        if m["data"].startswith("PISA_DECLS "):
            return [d["name"] for d in json.loads(m["data"][len("PISA_DECLS "):])]
    return []


def phase_resend(chunks, out):
    """M1 in a fresh process: the prefix as one command, from the header env."""
    repl = Repl()
    r, t = repl.timed({"cmd": HEADER})
    header_env = r["env"]
    total = sum(lines_of(c) for _, c in chunks)
    res = {"m0_again": {"seconds": t}, "m1": []}
    for frac in (0.25, 0.5, 0.75, 1.0):
        k, acc = 0, 0
        while k < len(chunks) and acc < frac * total:
            acc += lines_of(chunks[k][1])
            k += 1
        r, t = repl.timed({"cmd": "\n".join(c for _, c in chunks[:k]), "env": header_env})
        row = {"fraction": frac, "chunks": k, "lines": acc, "seconds": t, "errors": len(errors(r)), **repl.memory()}
        res["m1"].append(row)
        print(f"M1 {frac:.0%}: {k} chunks, {acc} lines, {t:.1f}s, {row['errors']} errors", flush=True)
    repl.close()
    return res


def phase_modules(src, order, replayed, out):
    """M5: the replayed prefix as package-local modules, built one by one, then imported."""
    mods = order[:replayed]
    chunked = set(mods)
    # Only chunk modules are renamed; tooling stays the compiled Physlib module.
    rename = lambda m: "Bench." + m[len("Physlib."):] if m in chunked else m
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w") as tar:
        for m in mods:
            text = IMPORT.sub(lambda x: x.group(0).replace(x.group(1), rename(x.group(1))), src[m])
            data = text.encode()
            info = tarfile.TarInfo(rename(m).replace(".", "/") + ".lean")
            info.size = len(data)
            tar.addfile(info, io.BytesIO(data))
    dexec(f"tar x -C {PROJECT}", input=buf.getvalue())
    dexec(f"cd {PROJECT} && grep -q 'name = \"Bench\"' lakefile.toml || printf '\\n[[lean_lib]]\\nname = \"Bench\"\\n' >> lakefile.toml")

    res = {"m5": {"builds": []}}
    for i, m in enumerate(mods):
        t = time.time()
        r = subprocess.run(["docker", "exec", CONTAINER, "bash", "-c", f"cd {PROJECT} && lake build {rename(m)}"],
                           capture_output=True)
        dt = time.time() - t
        res["m5"]["builds"].append({"module": m, "seconds": dt, "ok": r.returncode == 0})
        print(f"M5 build {i + 1}/{len(mods)} {m}: {dt:.1f}s {'ok' if r.returncode == 0 else 'FAILED'}", flush=True)
        if r.returncode != 0:
            res["m5"]["error"] = r.stdout.decode()[-1500:] + r.stderr.decode()[-1500:]
            return res
    t = time.time()
    dexec(f"cd {PROJECT} && lake build {rename(mods[-1])}")
    res["m5"]["lake_noop_seconds"] = time.time() - t

    imported = {i for m in mods for i in IMPORT.findall(src[m])}
    leaves = [rename(m) for m in mods if m not in imported]
    repl = Repl()
    header = HEADER + "\n" + "\n".join(f"import {m}" for m in leaves)
    r, t = repl.timed({"cmd": header})
    res["m5"]["import"] = {"seconds": t, "leaves": len(leaves), "errors": errors(r)[:3], **repl.memory()}
    hits = [repl.timed({"cmd": HIT, "env": r["env"]}) for _ in range(5)]
    res["m5"]["hits"] = [{"seconds": dt, "ok": not errors(h)} for h, dt in hits]
    print(f"M5 import: {t:.1f}s {res['m5']['import']}", flush=True)
    repl.close()
    return res


def render(raw):
    """The report's tables, from raw.json."""
    m2 = raw.get("m2", [])
    ok = m2
    with_errors = sum(1 for r in m2 if r["errors"])
    replay = sum(r["seconds"] for r in ok)
    lines = sum(r["lines"] for r in ok)
    per = sorted(r["seconds"] for r in ok)
    q = lambda xs, f: xs[min(len(xs) - 1, int(f * len(xs)))] if xs else 0
    out = [f"# Environment snapshots at Mathlib scale ({time.strftime('%Y-%m-%d')})", ""]
    out += [
        f"Library: the import closure of `{raw['target']}`, {raw['modules']} modules and {raw.get('lines', lines)} lines, "
        f"one module per chunk. Header: `{' / '.join(raw['header'].splitlines())}`. "
        "Measured with `scripts/bench_snapshots.py` in the worker image, driving the REPL directly; raw numbers in `raw.json`.",
        "",
        "| Measurement | Result |",
        "| --- | --- |",
        f"| M0 header import | {raw['m0']['seconds']:.1f} s, {raw['m0']['rss_mb']} MB |",
    ]
    for r in raw.get("m1", []):
        out.append(f"| M1 resend {r['fraction']:.0%} ({r['lines']} lines) as one command | {r['seconds']:.1f} s |")
    out.append(f"| M2 replay, {len(m2)} chunks chained ({lines} lines; {with_errors} with Lean errors) | {replay:.1f} s in total |")
    out.append(f"| M2 one chunk on its base (a statement-sized hit) | median {q(per, .5):.1f} s, p90 {q(per, .9):.1f} s, max {q(per, 1):.1f} s |")
    if "m3" in raw:
        h = sorted(x["seconds"] for x in raw["m3"]["final"])
        out.append(f"| M3 trivial lemma on the final env (10×) | median {q(h, .5) * 1000:.0f} ms, all ok: {all(x['ok'] for x in raw['m3']['final'])}; mid-chain env ok: {raw['m3']['mid']['ok']} |")
    if ok:
        out.append(f"| Memory after the chain | {ok[-1]['rss_mb']} MB (+{ok[-1]['rss_mb'] - raw['m0']['rss_mb']} MB over the header, {1000 * (ok[-1]['rss_mb'] - raw['m0']['rss_mb']) / max(lines, 1):.0f} MB per 1k lines) |")
    if "m4" in raw:
        w, p = raw["m4"]["whole"], raw["m4"]["scoped"]
        out.append(f"| M4 axiom report on the final env | whole: {w['seconds']:.1f} s, {w['decls']} declarations; scoped to the last chunk: {p['seconds']:.1f} s, {p['decls']} |")
    m5 = raw.get("m5")
    if m5:
        built = [b for b in m5["builds"] if b["ok"]]
        overhead = m5.get("lake_noop_seconds", 0)
        out.append(f"| M5 module builds, {len(built)} of {len(m5['builds'])} | {sum(b['seconds'] for b in built):.1f} s in total, including about {overhead:.1f} s of Lake overhead per build |")
        if "import" in m5:
            i = m5["import"]
            hs = sorted(x["seconds"] for x in m5["hits"])
            out.append(f"| M5 import header + {i['leaves']} leaf modules | {i['seconds']:.1f} s, {i['rss_mb']} MB, errors: {len(i['errors'])}; trivial lemma median {q(hs, .5) * 1000:.0f} ms |")
    out += ["", "## Per-chunk replay", "", "| # | Module | Lines | Seconds | RSS (MB) | Errors |", "| ---: | --- | ---: | ---: | ---: | ---: |"]
    for i, r in enumerate(m2, 1):
        out.append(f"| {i} | `{r['module']}` | {r['lines']} | {r['seconds']:.1f} | {r['rss_mb']} | {r.get('error_count', len(r['errors']))} |")
    return "\n".join(out) + "\n"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--image", default="pisa-worker:lean-project")
    ap.add_argument("--target", default="Physlib.QuantumMechanics")
    ap.add_argument("--out", type=Path, default=ROOT / "bench/snapshots" / time.strftime("%Y-%m-%d"))
    ap.add_argument("--limit", type=int, help="use only the first N chunks")
    ap.add_argument("--phases", default="chain,resend,modules")
    ap.add_argument("--keep", action="store_true", help="leave the container running")
    ap.add_argument("--report-only", action="store_true", help="render tables from an existing raw.json")
    args = ap.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    if args.report_only:
        (args.out / "tables.md").write_text(render(json.loads((args.out / "raw.json").read_text())))
        return

    sh("docker", "rm", "-f", CONTAINER, check=False)
    sh("docker", "run", "-d", "--name", CONTAINER, "--entrypoint", "sleep", args.image, "infinity")
    try:
        src = sources()
        order = closure(src, args.target)
        # Tooling, and whatever it imports, comes compiled through the header.
        tooling = set(closure(src, TOOLING.rstrip(".")))
        global HEADER
        HEADER += "".join(f"\nimport {m}" for m in order if m in tooling and m.startswith(TOOLING))
        order = [m for m in order if m not in tooling][: args.limit]
        chunks = [(m, as_command(src[m])) for m in order]
        print(f"{len(order)} modules, {sum(lines_of(c) for _, c in chunks)} lines", flush=True)
        raw = {"target": args.target, "modules": len(order), "lines": sum(lines_of(c) for _, c in chunks), "header": HEADER}
        phases = args.phases.split(",")
        if "chain" in phases:
            raw.update(phase_chain(chunks, args.out))
            (args.out / "raw.json").write_text(json.dumps(raw, indent=1))
        replayed = raw.get("replayed", len(chunks))
        if "resend" in phases:
            raw.update(phase_resend(chunks[:replayed], args.out))
            (args.out / "raw.json").write_text(json.dumps(raw, indent=1))
        if "modules" in phases:
            raw.update(phase_modules(src, order, replayed, args.out))
            (args.out / "raw.json").write_text(json.dumps(raw, indent=1))
        (args.out / "tables.md").write_text(render(raw))
    finally:
        if not args.keep:
            sh("docker", "rm", "-f", CONTAINER, check=False)


if __name__ == "__main__":
    main()
