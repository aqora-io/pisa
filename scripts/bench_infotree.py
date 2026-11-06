#!/usr/bin/env python3
"""What the REPL's info trees cost, in time and response size, on the bench
library's largest chunks: no info tree, `"original"` (the tactic nodes that
`detail: ["tactics"]` asks for) and `"full"` (every node, which expected types
need). Reuses bench_snapshots.py's container, chunks and REPL driver: the chain
of Physlib.QuantumMechanics' modules on `import Mathlib`. Each target is run in
every mode twice, the second round in reverse order.

usage: python3 scripts/bench_infotree.py OUT.json"""

import json
import sys
import time
from pathlib import Path

import bench_snapshots as b

IMAGE = "pisa-worker:lean-project"


class Repl(b.Repl):
    """Also keeps the size of each raw response and the time to parse it."""

    def send(self, command):
        self.proc.stdin.write(json.dumps(command) + "\n\n")
        self.proc.stdin.flush()
        lines = []
        while True:
            line = self.proc.stdout.readline()
            if not line:
                sys.exit("the REPL exited")
            if not line.strip() and lines:
                raw = "".join(lines)
                self.last_bytes = len(raw.encode())
                t = time.time()
                r = json.loads(raw)
                self.last_parse = time.time() - t
                return r
            if line.strip():
                lines.append(line)


def walk(trees):
    """Node counts by kind, and the original (non-synthetic) term and tactic
    nodes."""
    counts, terms, tactics = {}, [], []
    stack = list(trees or [])
    while stack:
        t = stack.pop()
        if not isinstance(t, dict):
            continue
        kind = t.get("kind", "?")
        node = t.get("node") or {}
        rng = (node.get("stx") or {}).get("range") or {}
        synthetic = rng.get("synthetic")
        key = f"{kind}{' (synthetic)' if synthetic else ''}"
        counts[key] = counts.get(key, 0) + 1
        if kind == "TermInfo" and not synthetic and rng:
            terms.append(node)
        if kind == "TacticInfo" and not synthetic and rng:
            tactics.append(node)
        stack.extend(t.get("children") or [])
    return counts, terms, tactics


def contains(rng, pos):
    s, f = rng["start"], rng["finish"]
    return (s["line"], s["column"]) <= (pos["line"], pos["column"]) <= (f["line"], f["column"])


def width(rng):
    s, f = rng["start"], rng["finish"]
    return (f["line"] - s["line"], f["column"] - s["column"])


def chained(repl, src, root="Physlib.QuantumMechanics", until=()):
    """Replays the library as a chain, stopping after the last of `until` if
    given; returns each chunk's code, the env it ran on, and the chain's
    timings."""
    order = b.closure(src, root)
    tooling = set(b.closure(src, b.TOOLING.rstrip(".")))
    header = b.HEADER + "".join(f"\nimport {m}" for m in order if m in tooling and m.startswith(b.TOOLING))
    order = [m for m in order if m not in tooling]
    if until:
        order = order[: max(order.index(m) for m in until) + 1]
    chunks = [(m, b.as_command(src[m])) for m in order]
    r, t = repl.timed({"cmd": header})
    assert not b.errors(r), b.errors(r)
    print(f"header {t:.1f}s", flush=True)
    env, before, chain = r["env"], {}, []
    for i, (name, cmd) in enumerate(chunks):
        before[name] = env
        r, t = repl.timed({"cmd": cmd, "env": env})
        chain.append({"module": name, "lines": b.lines_of(cmd), "seconds": t, "errors": len(b.errors(r))})
        print(f"chain {i + 1}/{len(chunks)} {name} {t:.1f}s", flush=True)
        env = r["env"]
    return header, dict(chunks), before, chain


def main():
    out = Path(sys.argv[1])
    b.sh("docker", "rm", "-f", b.CONTAINER, check=False)
    b.sh("docker", "run", "-d", "--name", b.CONTAINER, "--entrypoint", "sleep", IMAGE, "infinity")
    try:
        repl = Repl()
        header, code, before, chain = chained(repl, b.sources())
        clean = [c for c in chain if c["errors"] == 0]
        by_size = sorted(clean, key=lambda c: -c["lines"])
        median = sorted(clean, key=lambda c: c["lines"])[len(clean) // 2]
        targets = by_size[:6] + [median, by_size[len(by_size) // 4]]
        results = {"header": header, "chain": chain, "runs": []}
        modes = [None, "original", "full"]
        for c in targets:
            name = c["module"]
            row = {"module": name, "lines": c["lines"], "modes": {}}
            for round_ in range(2):
                for mode in modes if round_ == 0 else list(reversed(modes)):
                    cmd = {"cmd": code[name], "env": before[name]}
                    if mode:
                        cmd["infotree"] = mode
                    r, t = repl.timed(cmd)
                    entry = row["modes"].setdefault(mode or "none", {"seconds": [], "bytes": repl.last_bytes})
                    entry["seconds"].append(t)
                    entry["parse_seconds"] = repl.last_parse
                    entry.update(repl.memory())
                    if mode and round_ == 0:
                        entry["nodes"] = walk(r.get("infotree"))[0]
                    print(f"{name} {mode or 'none'}: {t:.1f}s {repl.last_bytes / 1e6:.1f} MB", flush=True)
            results["runs"].append(row)
            out.write_text(json.dumps(results, indent=1))
        repl.close()
    finally:
        b.sh("docker", "rm", "-f", b.CONTAINER, check=False)


if __name__ == "__main__":
    main()
