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

The `api` phase measures the snapshots pisa built from these results, through
a running pisa (`--url`, e.g. `docker compose up`): each chunk becomes a
snapshot that imports the snapshots of the modules it imports, and checks
follow each one, as a formalizer's next compiles would. Then, against the
same compose stack:

  restart      checks on the whole library after restarting both workers,
               and the memory of each REPL process
  determinism  the largest chunks rebuilt in each worker container, compared
               with the module the store served

usage: python3 scripts/bench_snapshots.py [--out DIR] [--limit N] [--phases ...]
"""

import argparse
import hashlib
import io
import json
import re
import statistics
import subprocess
import sys
import tarfile
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CONTAINER = "pisa-bench"
PROJECT = "/opt/project"
HEADER = "import Mathlib\nimport Pisa"
TOOLING = "Physlib.Meta."
HIT = "example : (2 : ℝ) + 2 = 4 := by norm_num"
# A module-system chunk need not import ℝ.
MODULE_HIT = "example : 2 + 2 = 4 := rfl"
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


def plain_lines(text):
    """A module's source without its imports, and with the module system's
    `public` markers made plain Lean."""
    for line in text.splitlines():
        if line.strip() == "module" or IMPORT.match(line):
            continue
        line = re.sub(r"^(\s*)(?:@\[expose\]\s*)?public\s+(noncomputable\s+)?section\b", r"\1\2section", line)
        yield re.sub(r"^(\s*)public\s+(?=meta\b|def\b|theorem\b|lemma\b|abbrev\b|instance\b|structure\b|class\b|inductive\b|noncomputable\b|opaque\b|@\[)", r"\1", line)


def as_command(text):
    """A module's source as one REPL command. Imports go; the module system's
    `public` markers become plain Lean; the chunk is wrapped in a section, with
    any scopes it leaves open closed, so its `open`s and options end with it as
    they would at the end of a file."""
    out, stack = [], []
    for line in plain_lines(text):
        s = line.strip()
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


def api(url, method, path, body=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(url + path, data=data, method=method,
                                 headers={"content-type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=3600) as r:
            return r.status, json.loads(r.read())
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"null")


def timed_check(url, code):
    t = time.time()
    status, r = api(url, "POST", "/check", {"code": code, "timeout": 600})
    assert status == 200, r
    return r, time.time() - t


def chunk_code(m, src, snaps, chunked, as_modules):
    """Module `m` as the code of a snapshot. As plain Lean, it imports the
    snapshots of its dependencies (or, without any, the header) and drops the
    module system. As a module-system file it stays verbatim, with imports of
    other chunks pointing at their snapshots."""
    if as_modules:
        def to_snapshot(i):
            name = i.group(1)
            return i.group(0).replace(name, snaps[name]["module"]) if name in chunked else i.group(0)
        return IMPORT.sub(to_snapshot, src[m])
    deps = [d for d in IMPORT.findall(src[m]) if d in chunked]
    header = "\n".join(f"import {snaps[d]['module']}" for d in deps) if deps else HEADER
    return header + "\n\n" + "\n".join(plain_lines(src[m]))


def suffix(as_modules):
    return "-modules" if as_modules else ""


def phase_api(src, order, url, checks, out, as_modules=False):
    """Each chunk as a snapshot through pisa's API, importing the snapshots of
    the modules it imports, followed by `checks` checks on its header."""
    chunked = set(order)
    hit = MODULE_HIT if as_modules else HIT
    raw_file = out / f"raw-api{suffix(as_modules)}.json"
    snaps, res = {}, {"api": {"creates": [], "checks": []}}
    for i, m in enumerate(order):
        deps = [d for d in IMPORT.findall(src[m]) if d in chunked]
        code = chunk_code(m, src, snaps, chunked, as_modules)
        t = time.time()
        status, r = api(url, "POST", "/snapshots", {"code": code, "timeout": 1200})
        dt = time.time() - t
        assert status == 200, r
        result = r["check"]
        row = {"module": m, "seconds": dt, "build_seconds": result["time"], "imports": len(deps),
               "kept": "snapshot" in r, "status": r.get("snapshot", {}).get("status"), "lines": lines_of(code)}
        if "snapshot" not in r:
            row["error"] = result.get("error") or [x["text"][:300] for x in result["messages"] if x["severity"] == "error"][:3]
            res["api"]["creates"].append(row)
            print(f"API create {i + 1}/{len(order)} {m}: NOT KEPT {row['error']}", flush=True)
            return res
        snaps[m] = r["snapshot"]
        hits = [timed_check(url, f"{snaps[m]['header']}\n\n{hit}") for _ in range(checks)]
        assert all(h["ok"] for h, _ in hits), hits
        row["checks"] = [dt for _, dt in hits]
        res["api"]["creates"].append(row)
        print(f"API create {i + 1}/{len(order)} {m}: {dt:.1f}s (build {result['time']:.1f}s), "
              f"checks {', '.join(f'{x:.2f}' for x in row['checks'])}s", flush=True)
        raw_file.write_text(json.dumps(res, indent=1))

    # One snapshot that imports every leaf holds the whole library.
    imported = {d for m in order for d in IMPORT.findall(src[m])}
    leaves = [m for m in order if m not in imported]
    imports = "\n".join(f"{'public ' if as_modules else ''}import {snaps[m]['module']}" for m in leaves)
    code = ("module\n" if as_modules else "") + imports + \
        f"\n\n{'public ' if as_modules else ''}theorem pisa_bench_library : True := trivial"
    t = time.time()
    status, r = api(url, "POST", "/snapshots", {"code": code, "timeout": 1200})
    assert status == 200 and "snapshot" in r, r
    final = r["snapshot"]
    res["api"]["library"] = {"snapshot_id": final["id"], "module": final["module"], "leaves": len(leaves),
                             "seconds": time.time() - t, "build_seconds": r["check"]["time"]}
    print(f"API library of {len(leaves)} leaves: {res['api']['library']['seconds']:.1f}s", flush=True)
    hits = [timed_check(url, f"{final['header']}\n\n{hit}") for _ in range(10)]
    res["api"]["hits"] = [dt for _, dt in hits]
    # Other headers on the same snapshot are misses: a fresh import, wherever
    # they land (the builder or the other worker).
    misses = []
    for extra in ["Mathlib.Tactic.Ring", "Mathlib.Tactic.Linarith", "Mathlib.Tactic.Positivity",
                  "Mathlib.Tactic.NormNum", "Mathlib.Tactic.FieldSimp", "Mathlib.Tactic.Abel"]:
        r, dt = timed_check(url, f"{final['header']}\nimport {extra}\n\n{hit}")
        assert r["ok"], r
        misses.append(dt)
    res["api"]["misses"] = misses
    print(f"API final {final['module']}: hits {statistics.median(res['api']['hits']):.2f}s median, "
          f"misses {', '.join(f'{x:.1f}' for x in misses)}s", flush=True)
    return res


def workers():
    """The compose stack's worker containers."""
    return sh("docker", "compose", "ps", "-q", "worker").decode().split()


def phase_restart(url, out, as_modules=False):
    """Restarts both workers, so their snapshot caches are empty, then checks
    on the whole-library snapshot of the api phase; and reads the memory of
    every REPL process."""
    raw = json.loads((out / f"raw-api{suffix(as_modules)}.json").read_text())
    lib = raw["api"]["library"]["snapshot_id"]
    hit = MODULE_HIT if as_modules else HIT
    status, info = api(url, "GET", f"/snapshots/{lib}")
    assert status == 200, info
    sh("docker", "compose", "restart", "worker")
    for _ in range(120):
        try:
            with urllib.request.urlopen(url + "/health") as r:
                if json.load(r)["workers"] == 2:
                    break
        except OSError:
            pass
        time.sleep(1)
    checks = [timed_check(url, f"{info['header']}\n\n{hit}") for _ in range(11)]
    assert all(r["ok"] for r, _ in checks), checks
    res = {"cold": [checks[0][1]], "hits": [dt for _, dt in checks[1:]], "memory_mb": []}
    script = ('for p in /proc/[0-9]*; do [ "$(cat $p/comm 2>/dev/null)" = repl ] && echo "$p $(grep -E '
              '"^(Rss|Pss|Shared_Clean|Shared_Dirty|Private_Clean|Private_Dirty):" $p/smaps_rollup | tr -s " " | tr "\\n" " ")"; done; true')
    for w in workers():
        for line in sh("docker", "exec", w, "bash", "-c", script).decode().splitlines():
            parts = line.split()
            fields = {parts[i].rstrip(":"): int(parts[i + 1]) // 1024 for i in range(1, len(parts) - 1, 3)}
            res["memory_mb"].append({"worker": w, "pid": parts[0], **fields})
    print(f"restart: cold {res['cold'][0]:.1f}s, hits {statistics.median(res['hits']):.3f}s", flush=True)
    return res


def phase_determinism(src, out, as_modules=False, n=3):
    """Rebuilds the largest chunks without snapshot imports in each worker
    container and compares their files with those the store served."""
    raw = json.loads((out / f"raw-api{suffix(as_modules)}.json").read_text())
    creates = raw["api"]["creates"]
    roots = sorted((c for c in creates if c["imports"] == 0), key=lambda c: -c["lines"])[:n]
    res = []
    for c in roots:
        code = chunk_code(c["module"], src, {}, set(), as_modules)
        sid = hashlib.sha256(code.encode()).hexdigest()[:32]  # pisa_protocol::snapshot::id
        # One digest over all of the module's files (one, or five for module-system code).
        digest = f"ls s{sid}.* | grep -v '[.]lean$' | sort | xargs sha256sum | sha256sum | cut -c1-64"
        fresh, cached = set(), set()
        for w in workers():
            script = (f"mkdir -p /tmp/det/PisaSnap && cat > /tmp/det/PisaSnap/s{sid}.lean && cd {PROJECT} && "
                      f"LEAN_PATH=$(lake env printenv LEAN_PATH) lean -R /tmp/det -o /tmp/det/PisaSnap/s{sid}.olean "
                      f"/tmp/det/PisaSnap/s{sid}.lean >/dev/null && (cd /tmp/det/PisaSnap && {digest}); "
                      f"for d in $(find /tmp -path '*snapshots*' -name 's{sid}.olean' -printf '%h\\n'); do (cd $d && {digest}); done; "
                      f"rm -rf /tmp/det")
            shas = subprocess.run(["docker", "exec", "-i", w, "bash", "-c", script], input=code.encode(),
                                  capture_output=True).stdout.decode().split()
            fresh.add(shas[0] if shas else None)
            cached.update(shas[1:])
        res.append({"module": c["module"], "lines": c["lines"], "fresh": sorted(map(str, fresh)),
                    "cached": sorted(cached), "identical": len(fresh) == 1 and None not in fresh and cached <= fresh})
        print(f"determinism {c['module']}: {'identical' if res[-1]['identical'] else 'DIFFERENT'}", flush=True)
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
    ap.add_argument("--url", default="http://localhost:8000", help="pisa, for the api phase")
    ap.add_argument("--checks", type=int, default=3, help="checks after each snapshot in the api phase")
    ap.add_argument("--as-modules", action="store_true",
                    help="api, restart, determinism: keep Physlib's module-system files as they are")
    ap.add_argument("--skip-replay-failures", action="store_true",
                    help="api phase: leave out modules that failed in the chain phase (raw.json), and their dependents")
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
        if "api" in phases:
            if args.skip_replay_failures:
                m2 = json.loads((args.out / "raw.json").read_text())["m2"]
                bad = {c["module"] for c in m2 if c.get("error_count")}
                for m in order:
                    if any(d in bad for d in IMPORT.findall(src[m])):
                        bad.add(m)
                order = [m for m in order if m not in bad]
            api_raw = {"target": args.target, "modules": len(order), "as_modules": args.as_modules,
                       "header": None if args.as_modules else HEADER}
            api_raw.update(phase_api(src, order, args.url, args.checks, args.out, args.as_modules))
            (args.out / f"raw-api{suffix(args.as_modules)}.json").write_text(json.dumps(api_raw, indent=1))
        if "restart" in phases:
            cold = phase_restart(args.url, args.out, args.as_modules)
            (args.out / f"cold{suffix(args.as_modules)}.json").write_text(json.dumps(cold, indent=1))
        if "determinism" in phases:
            det = phase_determinism(src, args.out, args.as_modules)
            (args.out / f"determinism{suffix(args.as_modules)}.json").write_text(json.dumps(det, indent=1))
        if not {"chain", "resend", "modules"} & set(phases):
            return
        (args.out / "tables.md").write_text(render(raw))
    finally:
        if not args.keep:
            sh("docker", "rm", "-f", CONTAINER, check=False)


if __name__ == "__main__":
    main()
