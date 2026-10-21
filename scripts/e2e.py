#!/usr/bin/env python3
"""End-to-end checks against a running `docker compose` stack with real Lean.

    PROJECT=lean-core docker compose up -d --build
    python3 scripts/e2e.py            # core checks
    python3 scripts/e2e.py --mathlib  # also Mathlib and Physlib (full image)

It stops and restarts workers to check that tactic sessions resume on another
worker (gate 3), so run it against a dev stack only.
"""

import argparse
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

BASE = os.environ.get("PISA_URL", "http://localhost:8000")
COMPOSE = ["docker", "compose"]


def call(method, path, body=None, timeout=900):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(
        BASE + path, data=data, method=method, headers={"content-type": "application/json"}
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            text = r.read().decode()
            return r.status, (json.loads(text) if r.headers.get_content_type() == "application/json" else text)
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read().decode() or "null")


def post(path, body):
    status, out = call("POST", path, body)
    assert status == 200, (path, status, out)
    return out


def wait_for_workers(n):
    for _ in range(120):
        try:
            _, metrics = call("GET", "/metrics")
            if f"pisa_workers {n}" in metrics:
                return
        except OSError:
            pass
        time.sleep(1)
    sys.exit(f"timed out waiting for {n} workers")


def compose(*args):
    subprocess.run(COMPOSE + list(args), check=True, capture_output=True)


def check(code, **extra):
    return post("/api/check", {"snippets": [{"id": "x", "code": code}], "timeout": 600, **extra})[
        "results"
    ][0]


def step(name):
    print(f"ok  {name}")


def core():
    r = check("\n#check Nat")
    assert r["response"]["messages"][0]["pos"]["line"] == 2, r
    step("positions")

    r = check("example : True := by\n  sleep 5000\n  trivial", timeout=1)
    assert "timed out" in r["error"], r
    step("timeout")

    for code, reason in [
        ("theorem t : False := by sorry", "sorryAx"),
        ("theorem t : False := by admit", "sorryAx"),
        ("theorem t : 2 + 2 = 4 := by native_decide", "native_decide"),
        ("axiom cheat : False\ntheorem t : False := cheat", "cheat"),
        ("set_option debug.skipKernelTC true in\ntheorem t : 1 = 1 := rfl", "skipKernelTC"),
        ("#eval IO.println \"hi\"", "#eval"),
    ]:
        r = check(code)
        v = r["verification"]
        assert not v["ok"] and any(reason in x for x in v["reasons"]), (code, r)
    assert check("theorem t (a b : Nat) : a + b = b + a := Nat.add_comm a b")["verification"]["ok"]
    step("verification policy")

    code = "theorem t (n : Nat) : n + 0 = n ∧ 0 + n = n := by sorry"
    s = post("/sessions", {"code": code})
    assert "error" not in s, s
    root = s["states"][0]
    split = post(f"/sessions/{root['state']}/tactic", {"tactic": "constructor"})
    assert len(split["goals"]) == 2, split
    step("session start and tactic")

    # Gate 3: continue with each worker stopped in turn; at least one of the two
    # steps must load the state from Garage on the other worker.
    compose("stop", "worker-1")
    wait_for_workers(1)
    left = post(f"/sessions/{split['state']}/tactic", {"tactic": "simp"})
    assert left.get("goals") == ["case right\nn : Nat\n⊢ 0 + n = n"], left
    compose("start", "worker-1")
    wait_for_workers(2)
    compose("stop", "worker-2")
    wait_for_workers(1)
    done = post(f"/sessions/{left['state']}/tactic", {"tactic": "simp"})
    assert done.get("proofStatus") == "Completed", done
    compose("start", "worker-2")
    wait_for_workers(2)
    step("sessions resume on another worker")

    bad = post(f"/sessions/{root['state']}/tactic", {"tactic": "foo"})
    assert "error" in bad and "state" not in bad, bad
    step("tactic errors")

    session = s["session"]
    good = post(
        f"/sessions/{session}/check",
        {"code": "theorem t (n : Nat) : n + 0 = n ∧ 0 + n = n := by\n  constructor <;> simp"},
    )
    assert good["ok"], good
    weaker = post(f"/sessions/{session}/check", {"code": "theorem t (n : Nat) : n + 0 = n := by simp"})
    assert not weaker["ok"] and "statement: t has a different type" in weaker["reasons"], weaker
    step("final check with statement check")

    _, metrics = call("GET", "/metrics")
    for series in ["pisa_header_cache_total", "pisa_queue_wait_seconds_bucket", "pisa_repl_restarts_total"]:
        assert series in metrics, series
    step("metrics")


def mathlib():
    r = check("import Mathlib\n\ntheorem t (a b : ℝ) : (a + b)^2 = a^2 + 2*a*b + b^2 := by ring")
    assert r["verification"]["ok"], r
    r = check("import Physlib\n\n#check SpaceTime")
    assert r["response"]["messages"][0]["pos"]["line"] == 3, r
    s = post("/sessions", {"code": "import Mathlib\n\ntheorem t (x : ℝ) (h : 0 < x) : 0 < x ^ 2 := by sorry"})
    assert "error" not in s, s
    done = post(f"/sessions/{s['states'][0]['state']}/tactic", {"tactic": "positivity"})
    assert done["proofStatus"] == "Completed", done
    step("Mathlib and Physlib")


if __name__ == "__main__":
    p = argparse.ArgumentParser()
    p.add_argument("--mathlib", action="store_true")
    args = p.parse_args()
    wait_for_workers(2)
    core()
    if args.mathlib:
        mathlib()
    print("all end-to-end checks passed")
