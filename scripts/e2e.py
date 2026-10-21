#!/usr/bin/env python3
"""End-to-end checks against a running `docker compose` stack with real Lean.

    PROJECT=lean-core docker compose up -d --build
    python3 scripts/e2e.py            # core checks
    python3 scripts/e2e.py --mathlib  # also Mathlib and Physlib (full image)

It stops and restarts workers to check that tactic sessions resume on another
worker (gate 3), so run it against a dev stack only. Pass --no-failover to skip
that step, e.g. against Kubernetes through a port-forward.
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
    return post("/check", {"code": code, "timeout": 600, **extra})


def step(name):
    print(f"ok  {name}")


def core(failover=True):
    r = check("\n#check Nat")
    assert r["messages"][0]["pos"]["line"] == 2, r
    step("positions")

    r = check("example : True := by\n  sleep 5000\n  trivial", timeout=1)
    assert r["error"]["kind"] == "timeout", r
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
        assert not r["ok"] and any(reason in x for x in r["reasons"]), (code, r)
    assert check("theorem t (a b : Nat) : a + b = b + a := Nat.add_comm a b")["ok"]
    step("verification policy")

    code = "theorem t (n : Nat) : n + 0 = n ∧ 0 + n = n := by sorry"
    s = post("/sessions", {"code": code})
    assert "error" not in s, s
    root = s["states"][0]
    split = post(f"/states/{root['state']}/tactic", {"tactic": "constructor"})
    assert len(split["goals"]) == 2, split
    step("session start and tactic")

    # Gate 3: continue with each worker stopped in turn; at least one of the two
    # steps must load the state from Garage on the other worker.
    if failover:
        compose("stop", "worker-1")
        wait_for_workers(1)
    left = post(f"/states/{split['state']}/tactic", {"tactic": "simp"})
    assert left.get("goals") == ["case right\nn : Nat\n⊢ 0 + n = n"], left
    if failover:
        compose("start", "worker-1")
        wait_for_workers(2)
        compose("stop", "worker-2")
        wait_for_workers(1)
    done = post(f"/states/{left['state']}/tactic", {"tactic": "simp"})
    assert done.get("complete"), done
    if failover:
        compose("start", "worker-2")
        wait_for_workers(2)
        step("sessions resume on another worker")
    else:
        step("tactic steps")

    status, bad = call("POST", f"/states/{root['state']}/tactic", {"tactic": "foo"})
    assert status == 422 and bad["error"]["kind"] == "lean", bad
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
    assert r["ok"], r
    r = check("import Physlib\n\n#check SpaceTime")
    assert r["messages"][0]["pos"]["line"] == 3, r
    s = post("/sessions", {"code": "import Mathlib\n\ntheorem t (x : ℝ) (h : 0 < x) : 0 < x ^ 2 := by sorry"})
    assert "error" not in s, s
    done = post(f"/states/{s['states'][0]['state']}/tactic", {"tactic": "positivity"})
    assert done["complete"], done
    step("Mathlib and Physlib")


if __name__ == "__main__":
    p = argparse.ArgumentParser()
    p.add_argument("--mathlib", action="store_true")
    p.add_argument("--no-failover", action="store_true", help="don't stop workers (not using compose)")
    args = p.parse_args()
    if not args.no_failover:
        wait_for_workers(2)
    core(failover=not args.no_failover)
    if args.mathlib:
        mathlib()
    print("all end-to-end checks passed")
