#!/usr/bin/env python3
"""Measures pisa on a library the size agent-built ones reach: 1,500
module-system snapshots on Lean's core, against a running stack
(`PROJECT=lean-core docker compose up`).

The library is generated: layers of `--width` modules, each module importing
one to three of the layer below and up to two of the eight layers below that,
declaring an exposed definition and a theorem about it. Every 50th module's
theorem is a `sorry`, and every 300th module declares an axiom a theorem
uses, so audits have something to report. A layer's creates run at once.

It times:
  import    each create, with the size of its closure
  top       one create that imports the top layer, warm and after the
            workers restart
  reads     a get, two listing pages of 1000, the dependents of the bottom
            module, a label audit, an audit against a spec with its review
            list, and a rebuild plan from the bottom module to a replacement
  storage   with PostgreSQL, the size of the catalog's tables and of the
            module rows (`pg_total_relation_size`, `pg_column_size`)

usage: python3 scripts/bench_library.py [--url URL] [--modules N] [--out DIR]
"""

import argparse
import concurrent.futures
import json
import random
import statistics
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def api(url, method, path, body=None, timeout=3600):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(url + path, data=data, method=method,
                                 headers={"content-type": "application/json"})
    t = time.time()
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            raw = r.read()
            status = r.status
    except urllib.error.HTTPError as e:
        raw, status = e.read(), e.code
    return status, json.loads(raw or b"null"), time.time() - t, len(raw)


def module_code(i, imports):
    """Module `i`'s code: it imports the snapshot modules `imports`."""
    lines = ["module", ""]
    lines += [f"public import {m}" for m in imports]
    value = f"{i} + 1"
    lines += ["", f"@[expose] public def f{i} : Nat := {value}", ""]
    if i % 50 == 49:
        lines.append(f"public theorem t{i} : f{i} = {i} + 1 := sorry")
    elif i % 300 == 299:
        lines.append(f"public axiom a{i} : f{i} = {i} + 1")
        lines.append(f"public theorem t{i} : f{i} = {i} + 1 := a{i}")
    else:
        lines.append(f"public theorem t{i} : f{i} = {i} + 1 := rfl")
    return "\n".join(lines) + "\n"


def create(url, code, labels, **extra):
    body = {"code": code, "labels": labels, "timeout": 600, **extra}
    status, r, dt, _ = api(url, "POST", "/snapshots", body)
    if status != 200 or "snapshot" not in r:
        sys.exit(f"create failed ({status}): {json.dumps(r)[:2000]}")
    return r, dt


def wait_for_workers(url, n):
    for _ in range(300):
        try:
            with urllib.request.urlopen(url + "/health") as r:
                if json.load(r)["workers"] >= n:
                    return
        except OSError:
            pass
        time.sleep(1)
    sys.exit(f"timed out waiting for {n} workers")


def storage(project):
    """The catalog's table sizes and the module rows' sizes, from the
    stack's PostgreSQL."""
    query = ("SELECT json_build_object("
             "'modules', pg_total_relation_size('modules'), "
             "'module_imports', pg_total_relation_size('module_imports'), "
             "'entries', pg_total_relation_size('entries'), "
             "'links', coalesce(pg_total_relation_size(to_regclass('links')), 0), "
             "'row_bytes', (SELECT json_agg(pg_column_size(m.*) ORDER BY pg_column_size(m.*)) FROM modules m))")
    cmd = ["docker", "compose"] + (["-p", project] if project else []) + \
        ["exec", "-T", "postgres", "psql", "-U", "pisa", "-d", "pisa", "-tAc", query]
    out = subprocess.run(cmd, capture_output=True, text=True)
    if out.returncode != 0:
        return {"error": out.stderr[-500:]}
    sizes = json.loads(out.stdout)
    rows = sizes.pop("row_bytes") or []
    sizes["module_rows"] = {"n": len(rows), "median": statistics.median(rows) if rows else 0,
                            "max": max(rows, default=0), "total": sum(rows)}
    return sizes


def summary(xs):
    xs = sorted(xs)
    if not xs:
        return {}
    return {"n": len(xs), "p50": statistics.median(xs), "p95": xs[int(0.95 * (len(xs) - 1))],
            "max": xs[-1], "total": sum(xs)}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", default="http://localhost:8000")
    ap.add_argument("--modules", type=int, default=1500)
    ap.add_argument("--width", type=int, default=8)
    ap.add_argument("--seed", type=int, default=7)
    ap.add_argument("--project", help="the compose project, for storage and restarts")
    ap.add_argument("--out", type=Path, default=ROOT / "bench/library" / time.strftime("%Y-%m-%d"))
    args = ap.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    rng = random.Random(args.seed)
    run = f"lib{args.seed}-{int(time.time())}"
    labels = {"bench": run}
    url = args.url

    # The library, a layer at a time.
    layers = []
    made = []  # (i, response, seconds, closure size)
    closures = []
    t0 = time.time()
    with concurrent.futures.ThreadPoolExecutor(args.width) as pool:
        i = 0
        while i < args.modules:
            layer = []
            jobs = []
            for _ in range(min(args.width, args.modules - i)):
                below = layers[-1] if layers else []
                parents = rng.sample(below, min(len(below), rng.randint(1, 3))) if below else []
                older = [m for l in layers[-9:-1] for m in l]
                parents += rng.sample(older, min(len(older), rng.randint(0, 2)))
                parents = sorted(set(parents))
                closure = set(parents)
                for p in parents:
                    closure |= closures[p]
                closures.append(closure)
                code = module_code(i, [made[p][1]["snapshot"]["module"] for p in parents])
                jobs.append((i, pool.submit(create, url, code, labels)))
                layer.append(i)
                i += 1
            for j, job in jobs:
                r, dt = job.result()
                made.append((j, r, dt, len(closures[j])))
            layers.append(layer)
            if len(layers) % 10 == 0:
                done = len(made)
                print(f"{done}/{args.modules} modules in {time.time() - t0:.0f}s; "
                      f"last layer p50 {statistics.median(d for _, _, d, _ in made[-args.width:]):.2f}s",
                      flush=True)
    imported = time.time() - t0
    creates = [{"i": i, "seconds": dt, "closure": c, "verified": r["snapshot"]["verified"]}
               for i, r, dt, c in made]
    by_size = {}
    for lo, hi in [(0, 100), (100, 500), (500, 1000), (1000, 10 ** 9)]:
        ds = [c["seconds"] for c in creates if lo <= c["closure"] < hi]
        if ds:
            by_size[f"closure {lo}-{hi if hi < 10 ** 9 else ''}"] = summary(ds)
    res = {"modules": args.modules, "width": args.width, "seed": args.seed, "run": run,
           "import": {"seconds": imported, "creates": summary([c["seconds"] for c in creates]),
                      "by_closure": by_size}}
    print(f"import: {imported:.0f}s, create p50 {res['import']['creates']['p50']:.2f}s", flush=True)
    (args.out / "creates.json").write_text(json.dumps(creates))

    # One create on the whole library: its top layer.
    top = [made[i][1]["snapshot"]["module"] for i in layers[-1]]
    top_code = "module\n\n" + "\n".join(f"public import {m}" for m in top) + "\n\npublic theorem top : True := trivial\n"
    r, warm = create(url, top_code, labels)
    res["top"] = {"closure": len(set().union(*(closures[i] for i in layers[-1])) | set(layers[-1])),
                  "warm_seconds": warm}
    if args.project:
        with urllib.request.urlopen(url + "/health") as h:
            workers = json.load(h)["workers"]
        # Workers deregister as they drain, so the count comes back only once
        # the restarted ones serve.
        cmd = ["docker", "compose", "-p", args.project, "restart", "worker"]
        subprocess.run(cmd, check=True, capture_output=True)
        wait_for_workers(url, workers)
        again = top_code.replace("theorem top", "theorem top'")
        r2, cold = create(url, again, labels)
        res["top"]["cold_seconds"] = cold
    print(f"top: {res['top']}", flush=True)

    # Reads.
    bottom = made[0][1]["snapshot"]["id"]
    leaf = r["snapshot"]["id"]
    reads = {}

    def read(name, method, path, body=None):
        status, out, dt, size = api(url, method, path, body)
        reads[name] = {"status": status, "seconds": dt, "bytes": size}
        if status != 200:
            reads[name]["error"] = json.dumps(out)[:500]
        print(f"{name}: {status} {dt:.2f}s {size} bytes", flush=True)
        return out

    version = made[0][1]["snapshot"]["version"]
    read("get", "GET", f"/snapshots/{leaf}")
    page = read("list page 1", "GET", f"/snapshots?label=bench:{run}&limit=1000&version={version}")
    if page and page.get("next"):
        read("list page 2", "GET", f"/snapshots?label=bench:{run}&limit=1000&after={page['next']}&version={version}")
    read("dependents of the bottom", "GET", f"/snapshots/{bottom}/dependents")
    read("label audit", "GET", f"/snapshots/audit?label=bench:{run}&version={version}")
    # A spec on top with a hole, and a head that fills it.
    spec_code = (f"module\n\npublic import {top[0]}\n\n@[expose] public def h : Nat := sorry\n\n"
                 f"public theorem goal : h = h := sorry\n")
    spec, _ = create(url, spec_code, {}, holes=["h"])
    head_code = spec_code.replace("def h : Nat := sorry", "def h : Nat := 1").replace("goal : h = h := sorry", "goal : h = h := rfl")
    status, head, _, _ = api(url, "POST", "/snapshots", {"code": head_code, "timeout": 600,
                                                         "replaces": {"snapshot_id": spec["snapshot"]["id"]}})
    if status == 200 and "snapshot" in head:
        read("audit against a spec, with review", "GET",
             f"/snapshots/audit?against={spec['snapshot']['id']}&review=true")
    # A replacement of the bottom module with the same statements.
    bottom_code = module_code(0, []).replace(":= rfl", ":= by rfl")
    replacement, _ = create(url, bottom_code, {})
    read("rebuild plan from the bottom", "POST", "/snapshots/rebuild",
         {"from": bottom, "to": replacement["snapshot"]["id"], "labels": {"bench": run}, "dry_run": True})
    res["reads"] = reads
    res["storage"] = storage(args.project)
    print(f"storage: {res['storage']}", flush=True)
    (args.out / "results.json").write_text(json.dumps(res, indent=1))


if __name__ == "__main__":
    main()
