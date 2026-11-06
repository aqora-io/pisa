#!/usr/bin/env python3
"""What `infotree: "full"` answers at positions: for 200 random code positions
in two bench chunks, how often there are goals (an original tactic node
contains the position) and an expected type (an original term node with one
contains it), and how much a worker would keep of the tree to answer them.

usage: python3 scripts/bench_positions.py OUT.json"""

import json
import random
import sys
from pathlib import Path

import bench_snapshots as b
from bench_infotree import IMAGE, Repl, chained, contains, walk, width

TARGETS = ["Physlib.SpaceAndTime.Space.Basic", "Physlib.Mathematics.Distribution.TemperedDistribution"]
SKIP = ("--", "/-", "import", "open", "namespace", "end", "section", "variable", "@[")


def main():
    out_path = Path(sys.argv[1])
    b.sh("docker", "rm", "-f", b.CONTAINER, check=False)
    b.sh("docker", "run", "-d", "--name", b.CONTAINER, "--entrypoint", "sleep", IMAGE, "infinity")
    try:
        repl = Repl()
        _, code, before, _ = chained(repl, b.sources(), until=TARGETS)
        out = {}
        for name in TARGETS:
            r, t = repl.timed({"cmd": code[name], "env": before[name], "infotree": "full"})
            _, terms, tactics = walk(r.get("infotree"))
            terms = [n for n in terms if n.get("expectedType")]
            lines = code[name].split("\n")
            spots = [(i + 1, c) for i, line in enumerate(lines) for c, ch in enumerate(line)
                     if ch.strip() and not line.lstrip().startswith(SKIP)]
            random.seed(1)
            picks = [{"line": line, "column": c} for line, c in random.sample(spots, 200)]
            goals = types = 0
            kept = []
            for p in picks:
                tac = [n for n in tactics if contains(n["stx"]["range"], p)]
                term = [n for n in terms if contains(n["stx"]["range"], p)]
                item = {}
                if tac:
                    goals += 1
                    item["goals"] = min(tac, key=lambda n: width(n["stx"]["range"]))["goalsBefore"]
                if term:
                    types += 1
                    n = min(term, key=lambda n: width(n["stx"]["range"]))
                    item["expected_type"], item["term"] = n["expectedType"], n["expr"]
                kept.append(item)
            out[name] = {"lines": len(lines), "seconds": t, "bytes": repl.last_bytes, "positions": len(picks),
                         "with_goals": goals, "with_expected_type": types,
                         "kept_bytes_per_position": len(json.dumps(kept).encode()) / len(picks),
                         "term_nodes_with_type": len(terms), "tactic_nodes": len(tactics),
                         "examples": [k for k in kept if "expected_type" in k][:3]}
            print(name, {k: v for k, v in out[name].items() if k != "examples"}, flush=True)
            out_path.write_text(json.dumps(out, indent=1, ensure_ascii=False))
        repl.close()
    finally:
        b.sh("docker", "rm", "-f", b.CONTAINER, check=False)


if __name__ == "__main__":
    main()
