#!/usr/bin/env python3
"""End-to-end checks against a running `docker compose` stack with real Lean.

    PROJECT=lean-core docker compose up -d --build
    python3 scripts/e2e.py            # core checks
    python3 scripts/e2e.py --mathlib  # also Mathlib and Physlib (full image)
    python3 scripts/e2e.py --search   # also snapshot search (PISA_SEARCH_QDRANT
                                      # set and compose --profile search)
    PISA_URL=http://localhost:8001 python3 scripts/e2e.py  # through the other server

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
import urllib.parse
import urllib.request

BASE = os.environ.get("PISA_URL", "http://localhost:8000")
COMPOSE = ["docker", "compose"]


def call(method, path, body=None, timeout=900, base=BASE):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(
        base + path, data=data, method=method, headers={"content-type": "application/json"}
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

    r = check(TACTIC_PROOF, detail=["tactics"])
    assert r["ok"] and [t["tactic"] for t in r["tactics"]] == ["constructor", "simp", "simp"], r
    first, left = r["tactics"][0], r["tactics"][1]
    assert first["goals_before"] == ["n : Nat\n⊢ n + 0 = n ∧ 0 + n = n"], r
    assert first["goals_after"] == ["case left\nn : Nat\n⊢ n + 0 = n", "case right\nn : Nat\n⊢ 0 + n = n"], r
    assert left["goals_before"] == first["goals_after"][:1] and left["goals_after"] == [], r
    step("tactic detail")

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


TACTIC_PROOF = "theorem t (n : Nat) : n + 0 = n ∧ 0 + n = n := by\n  constructor\n  · simp\n  · simp"

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


def delete_entries(created):
    """Deletes the entry of every kept snapshot in `created` and checks that
    the modules went with them: each exactly once, once nothing needs it."""
    kept = [x for x in created if "entry" in x]
    removed = []
    for x in kept:
        status, gone = call("DELETE", f"/snapshots/{x['entry']}")
        assert status == 200 and gone["entry"] == x["entry"], gone
        removed += gone["removed"]
    assert sorted(removed) == sorted({x["snapshot"] for x in kept}), (removed, kept)


def snapshots():
    created = []

    def create(code):
        start = time.monotonic()
        r = post("/snapshots", {"code": code, "timeout": 600})
        created.append(r)
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

    # Macros are allowed on a default pool, because they are kernel-re-verified.
    macro_snap, _ = create("macro \"two\" : term => `((2 : Nat))\ndef via_macro : Nat := two")
    assert macro_snap["result"]["ok"] and macro_snap["verified"], macro_snap
    assert on(macro_snap, "example : via_macro = 2 := rfl")["ok"]
    inherits, _ = create(f"{macro_snap['header']}\n\ndef via_import : Nat := two")
    assert inherits["verified"], inherits
    opted_out = post("/snapshots", {"code": "macro \"three\" : term => `((3 : Nat))", "verify": False, "timeout": 600})
    created.append(opted_out)
    assert "snapshot" in opted_out and not opted_out["verified"], opted_out
    step("snapshot: macros are re-checked unless the request opts out")

    # Everything else skips the re-check, until it is asked for.
    assert not root["verified"] and not child["verified"], (root, child)
    status, verified = call("POST", f"/snapshots/{child['entry']}/verify")
    assert status == 200 and verified["verified"] and verified["entry"] == child["entry"], verified
    assert verified["checked"] == [root["snapshot"], child["snapshot"]], verified
    assert verified["dirty"] and "failure" not in verified, verified
    assert call("GET", f"/snapshots/{root['entry']}")[1]["verified"]
    assert call("POST", f"/snapshots/{child['entry']}/verify")[1]["checked"] == []
    asked, _ = create(f"{sibling['header']}\n\ntheorem Snap.one : Snap.double 1 = 2 := rfl")
    assert not asked["verified"], asked
    asked = post("/snapshots", {"code": f"{sibling['header']}\n\ntheorem Snap.one : Snap.double 1 = 2 := rfl",
                                "verify": True, "timeout": 600})
    created.append(asked)
    assert asked["verified"], asked
    step("snapshot: verify on request, at creation or later, keeps sorry dirty")

    status, info = call("GET", f"/snapshots/{child['entry']}")
    assert status == 200 and info["imports"] == [root["snapshot"]], info
    assert info["entry"] == child["entry"] and info["snapshot"] == child["snapshot"], info
    delete_entries(created)
    r = on(merged, "example : True := trivial")
    assert r["error"]["kind"] == "not_found", r
    step("snapshot: errors aren't kept; read, and modules go with their last entry")


def labels():
    def create(run):
        return post("/snapshots", {"code": SNAPSHOT_ROOT, "labels": {"e2e": "1", "run": run}, "timeout": 600})

    def listed(*filters):
        query = urllib.parse.urlencode([("label", f) for f in filters])
        status, out = call("GET", f"/snapshots?{query}")
        assert status == 200, (filters, status, out)
        return [x["entry"] for x in out["entries"]], out

    first, second = create("a"), create("b")
    entries, out = listed("e2e:1")
    assert entries == [second["entry"], first["entry"]], out
    assert {x["snapshot"] for x in out["entries"]} == {first["snapshot"]}, out
    status, patched = call("PATCH", f"/snapshots/{first['entry']}", {"set": {"run": "c"}})
    assert status == 200 and patched["labels"] == {"e2e": "1", "run": "c"}, patched
    assert listed("run:c")[0] == [first["entry"]]
    step("labels: list, filter, edit")

    status, gone = call("DELETE", f"/snapshots/{first['entry']}")
    assert status == 200 and gone["removed"] == [], gone
    r = check(f"{second['header']}\n\nexample : Snap.double 3 = 6 := by simp")
    assert r["ok"], r
    status, gone = call("DELETE", f"/snapshots/{second['entry']}")
    assert status == 200 and gone["removed"] == [second["snapshot"]], gone
    step("labels: modules go with their last entry")


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
    created = []

    def create(code):
        r = post("/snapshots", {"code": code, "timeout": 600})
        created.append(r)
        return r

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
    assert not child["verified"], child
    step("module snapshot: sorry in a proof body is still reported")

    status, verified = call("POST", f"/snapshots/{child['entry']}/verify")
    assert status == 200 and verified["verified"] and verified["dirty"], verified
    assert verified["checked"] == [root["snapshot"], child["snapshot"]], verified
    step("module snapshot: verify re-checks the chain")

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
    delete_entries(created)
    step("module snapshot: plain code imports it, a module can't import plain code")


def snapshot_search():
    run = str(int(time.time()))
    # The comment makes it a new module, which this release has exported.
    code = (f"-- e2e {run}\n/-- Tripling a number adds it to itself twice. -/\n"
            "theorem e2e_triple (n : Nat) : 3 * n = n + n + n := by omega\n")
    created = post("/snapshots", {"code": code, "labels": {"e2e-search": run}, "timeout": 600})

    def hits(scope):
        query = {"nl": "tripling a number", "name": "e2e_triple", "snapshots": scope}
        status, out = call("POST", "/v1/search", {"queries": [query]})
        assert status == 200, out
        return [h for h in out["results"][0]["hits"] if h["module"] == created["snapshot"]]

    def until(f, what):
        for _ in range(60):
            if f():
                return
            time.sleep(1)
        sys.exit(f"snapshot search: timed out waiting for {what}")

    labelled = {"labels": {"e2e-search": run}}
    until(lambda: hits(labelled), "a new snapshot")
    hit = hits(labelled)[0]
    assert hit["library"] == "PisaSnap" and hit["snapshot"]["entries"] == [created["entry"]], hit
    assert hit["flags"]["imports"] == created["header"], hit
    r = check(f"{hit['flags']['imports']}\n\nexample : 3 * 2 = 2 + 2 + 2 := e2e_triple 2")
    assert r["ok"], r
    assert not hits({"labels": {"e2e-search": "other"}}) and not hits("none")
    step("snapshot search: a new snapshot is found by label, and its header loads it")

    moved = {"labels": {"e2e-search": run + "-moved"}}
    status, _ = call("PATCH", f"/snapshots/{created['entry']}", {"set": moved["labels"]})
    assert status == 200
    until(lambda: hits(moved) and not hits(labelled), "a relabel")
    status, _ = call("DELETE", f"/snapshots/{created['entry']}")
    assert status == 200
    until(lambda: not hits("all"), "a delete")
    step("snapshot search: follows relabels and deletes")


def mathlib():
    r = check("import Mathlib\n\ntheorem t (a b : ℝ) : (a + b)^2 = a^2 + 2*a*b + b^2 := by ring")
    assert r["ok"], r
    # Mathlib and QuantumInfo both declare Commute.log_left, so they can't be
    # imported together; the error must say so rather than leave names unknown.
    r = check("import Mathlib\nimport QuantumInfo\n#check MState")
    assert not r["ok"] and "Commute.log_left" in r["messages"][0]["data"], r
    r = check("import Physlib\n\n#check SpaceTime")
    assert r["messages"][0]["pos"]["line"] == 3, r
    r = check("import Mathlib\n\n" + TACTIC_PROOF, detail=["tactics"])
    assert r["tactics"][0]["pos"] == {"line": 4, "column": 2}, r
    s = post("/sessions", {"code": "import Mathlib\n\ntheorem t (x : ℝ) (h : 0 < x) : 0 < x ^ 2 := by sorry"})
    assert "error" not in s, s
    done = post(f"/states/{s['states'][0]['state']}/tactic", {"tactic": "positivity"})
    assert done["complete"], done
    step("Mathlib and Physlib")


if __name__ == "__main__":
    p = argparse.ArgumentParser()
    p.add_argument("--mathlib", action="store_true")
    p.add_argument("--no-failover", action="store_true", help="don't stop workers (not using compose)")
    p.add_argument("--search", action="store_true", help="also check snapshot search")
    args = p.parse_args()
    if not args.no_failover:
        wait_for_workers(2)
    core(failover=not args.no_failover)
    snapshots()
    labels()
    module_snapshots()
    if args.mathlib:
        mathlib()
    if args.search:
        snapshot_search()
    print("all end-to-end checks passed")
