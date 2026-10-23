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


SNAPSHOT_ROOT = """namespace Snap
def double (n : Nat) : Nat := 2 * n
@[simp] theorem double_eq (n : Nat) : double n = n + n := by unfold double; omega
private def hidden : Nat := 7
class Shape (α : Type) where
  sides : α → Nat
instance : Shape Nat := ⟨fun _ => 4⟩
end Snap
notation "⟪" x "⟫" => Snap.double x
open Snap
"""


def snapshots():
    def create(code):
        start = time.monotonic()
        r = post("/snapshots", {"code": code, "timeout": 600})
        return r, time.monotonic() - start

    def on(snapshot, body):
        return check(f"{snapshot['header']}\n\n{body}")

    root, took = create(SNAPSHOT_ROOT)
    assert root["result"]["ok"] and not root["dirty"], root
    assert root["header"] == f"import {root['snapshot']}", root
    start = time.monotonic()
    r = on(root, "example : Snap.double 3 = 6 := by simp\n"
                 "example : Snap.Shape.sides (5 : Nat) = 4 := rfl\n"
                 "example : ⟪2⟫ = 4 := rfl")
    assert r["ok"], r
    step(f"snapshot: definitions, simp lemmas, instances and notation carry "
         f"(create {took:.1f}s, check {time.monotonic() - start:.1f}s)")
    assert not on(root, "#check Snap.hidden")["ok"]
    assert not on(root, "example : double 1 = 2 := rfl")["ok"]
    step("snapshot: private declarations and `open` don't carry")

    child, _ = create(f"{root['header']}\n\ntheorem Snap.later (n : Nat) : Snap.double n ≥ n := by sorry")
    assert child["dirty"] and child["result"]["reasons"] == ["axiom: Snap.later uses sorryAx"], child
    r = on(child, "theorem uses_later : Snap.double 2 ≥ 2 := Snap.later 2\n"
                  "theorem clean : Snap.double 2 = 4 := rfl")
    assert r["reasons"] == ["axiom: uses_later uses sorryAx"], r
    sibling, _ = create(f"{root['header']}\n\ntheorem Snap.zero : Snap.double 0 = 0 := rfl")
    merged, _ = create(f"import {child['snapshot']}\nimport {sibling['snapshot']}\n\n"
                       "theorem Snap.both : Snap.double 0 = 0 ∧ Snap.double 1 ≥ 1 := ⟨Snap.zero, by simp⟩")
    assert merged["result"]["ok"] and not merged["dirty"], merged
    step("snapshot: sorry marks it dirty; branches merge")

    rejected, _ = create("theorem broken : 1 = 2 := rfl")
    assert "snapshot" not in rejected and rejected["result"]["messages"][0]["severity"] == "error", rejected
    status, info = call("GET", f"/snapshots/{child['snapshot']}")
    assert status == 200 and info["imports"] == [root["snapshot"]], info
    status, gone = call("DELETE", f"/snapshots/{root['snapshot']}")
    assert status == 200 and len(gone["deleted"]) == 4, gone
    r = on(merged, "example : True := trivial")
    assert r["error"]["kind"] == "not_found", r
    step("snapshot: errors aren't kept; read and cascading delete")


MODULE_ROOT = """module

@[expose] public section
namespace MSnap
def double (n : Nat) : Nat := 2 * n
theorem double_eq (n : Nat) : double n = n + n := by unfold double; omega
end MSnap
end

public def MSnap.opaque (n : Nat) : Nat := n + 1
def MSnap.hidden : Nat := 7
"""


def module_snapshots():
    def create(code):
        return post("/snapshots", {"code": code, "timeout": 600})

    def on(snapshot, body):
        return check(f"{snapshot['header']}\n\n{body}")

    root = create(MODULE_ROOT)
    assert root["result"]["ok"] and not root["dirty"], root
    assert root["header"] == f"module\npublic import {root['snapshot']}", root
    assert on(root, "example : MSnap.double 3 = 6 := by simp [MSnap.double_eq]")["ok"]
    assert not on(root, "#check MSnap.hidden")["ok"]
    # In a module, an imported definition unfolds only if it is exposed; plain
    # code sees through it.
    assert not on(root, "example : MSnap.opaque 1 = 2 := rfl")["ok"]
    assert check(f"import {root['snapshot']}\n\nexample : MSnap.opaque 1 = 2 := rfl")["ok"]
    step("module snapshot: public and exposed declarations carry, the rest doesn't")

    child = create(f"{root['header']}\n\npublic theorem MSnap.cheat : 1 = 2 := by\n"
                   "  have h : False := sorry\n  exact h.elim")
    assert child["dirty"] and child["result"]["reasons"] == ["axiom: MSnap.cheat uses sorryAx"], child
    r = on(child, "public theorem uses : 1 = 2 := MSnap.cheat")
    assert r["reasons"] == ["axiom: uses uses sorryAx"], r
    step("module snapshot: sorry in a proof body is still reported")

    code = f"{root['header']}\n\npublic theorem t (n : Nat) : MSnap.double n = n + n := by sorry"
    s = post("/sessions", {"code": code})
    done = post(f"/states/{s['states'][0]['state']}/tactic", {"tactic": "exact MSnap.double_eq n"})
    assert done["complete"], done
    proof = code.replace("by sorry", "MSnap.double_eq n")
    assert post(f"/sessions/{s['session']}/check", {"code": proof, "timeout": 600})["ok"]
    step("module snapshot: sessions in module mode")

    plain = create(f"import {root['snapshot']}\n\ntheorem on_module : MSnap.double 1 = 2 := rfl")
    assert plain["result"]["ok"], plain
    mixed = create(f"module\npublic import {plain['snapshot']}\n\npublic def x : Nat := 1")
    assert "snapshot" not in mixed and "non-`module`" in mixed["result"]["messages"][0]["data"], mixed
    status, gone = call("DELETE", f"/snapshots/{root['snapshot']}")
    assert status == 200 and len(gone["deleted"]) == 3, gone
    step("module snapshot: plain code imports it, a module can't import plain code")


def mathlib():
    r = check("import Mathlib\n\ntheorem t (a b : ℝ) : (a + b)^2 = a^2 + 2*a*b + b^2 := by ring")
    assert r["ok"], r
    # Mathlib and QuantumInfo both declare Commute.log_left, so they can't be
    # imported together; the error must say so rather than leave names unknown.
    r = check("import Mathlib\nimport QuantumInfo\n#check MState")
    assert not r["ok"] and "Commute.log_left" in r["messages"][0]["data"], r
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
    snapshots()
    module_snapshots()
    if args.mathlib:
        mathlib()
    print("all end-to-end checks passed")
