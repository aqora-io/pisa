#!/usr/bin/env python3
"""End-to-end checks against a running `docker compose` stack with real Lean.

    PROJECT=lean-core docker compose up -d --build
    python3 scripts/e2e.py            # core checks
    python3 scripts/e2e.py --mathlib  # also Mathlib and Physlib (full image)
    python3 scripts/e2e.py --search   # also snapshot search (PISA_SEARCH_QDRANT
                                      # set and compose --profile search)

It stops and restarts each of the stack's two workers to check that tactic
sessions resume on the other (gate 3), so run it against a dev stack only.
Pass --no-failover to skip that step, e.g. against Kubernetes through a
port-forward.
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
            _, health = call("GET", "/health")
            if health["workers"] == n:
                return
        except OSError:
            pass
        time.sleep(1)
    sys.exit(f"timed out waiting for {n} workers")


def worker_containers():
    out = subprocess.run(COMPOSE + ["ps", "-q", "worker"], check=True, capture_output=True, text=True)
    return out.stdout.split()


def docker(*args):
    subprocess.run(["docker", *args], check=True, capture_output=True)


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
    split = post(f"/states/{root['id']}/tactic", {"tactic": "constructor"})["state"]
    assert len(split["goals"]) == 2, split
    step("session start and tactic")

    # Gate 3: continue with each worker stopped in turn; at least one of the two
    # steps must load the state from Garage on the other worker.
    if failover:
        first, second = worker_containers()
        docker("stop", first)
        wait_for_workers(1)
    left = post(f"/states/{split['id']}/tactic", {"tactic": "simp"})["state"]
    assert left["goals"] == ["case right\nn : Nat\n⊢ 0 + n = n"], left
    if failover:
        docker("start", first)
        wait_for_workers(2)
        docker("stop", second)
        wait_for_workers(1)
    done = post(f"/states/{left['id']}/tactic", {"tactic": "simp"})
    assert done.get("complete"), done
    if failover:
        docker("start", second)
        wait_for_workers(2)
        step("sessions resume on another worker")
    else:
        step("tactic steps")

    status, bad = call("POST", f"/states/{root['id']}/tactic", {"tactic": "foo"})
    assert status == 422 and bad["error"]["kind"] == "lean", bad
    step("tactic errors")

    session = s["id"]
    good = post(
        f"/sessions/{session}/check",
        {"code": "theorem t (n : Nat) : n + 0 = n ∧ 0 + n = n := by\n  constructor <;> simp"},
    )
    assert good["ok"], good
    weaker = post(f"/sessions/{session}/check", {"code": "theorem t (n : Nat) : n + 0 = n := by simp"})
    assert not weaker["ok"] and "statement: t has a different type" in weaker["reasons"], weaker
    step("final check with statement check")


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


def delete_snapshots(created):
    """Deletes every snapshot the creates in `created` kept and checks that
    the modules went with them: each exactly once, once nothing needs it."""
    kept = [x["snapshot"] for x in created if "snapshot" in x]
    removed = []
    for x in kept:
        status, gone = call("DELETE", f"/snapshots/{x['id']}")
        assert status == 200, gone
        removed += gone["removed"]
    assert sorted(removed) == sorted({x["module"] for x in kept}), (removed, kept)


def snapshots():
    created = []

    def create(code):
        start = time.monotonic()
        r = post("/snapshots", {"code": code, "timeout": 600})
        created.append(r)
        return r, time.monotonic() - start

    def on(created, body):
        return check(f"{created['snapshot']['header']}\n\n{body}")

    root, took = create(SNAPSHOT_ROOT)
    assert root["check"]["ok"] and root["snapshot"]["status"] == "proved", root
    assert root["snapshot"]["header"] == f"import {root['snapshot']['module']}", root
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

    child, _ = create(f"{root['snapshot']['header']}\n\ntheorem Snap.later (n : Nat) : Snap.double n ≥ n := by sorry")
    assert child["snapshot"]["status"] == "sorry", child
    assert child["check"]["reasons"] == ["axiom: Snap.later uses sorryAx"], child
    r = on(child, "theorem uses_later : Snap.double 2 ≥ 2 := Snap.later 2\n"
                  "theorem clean : Snap.double 2 = 4 := rfl")
    assert r["reasons"] == ["axiom: uses_later uses sorryAx"], r
    sibling, _ = create(f"{root['snapshot']['header']}\n\ntheorem Snap.zero : Snap.double 0 = 0 := rfl")
    merged, _ = create(f"import {child['snapshot']['module']}\nimport {sibling['snapshot']['module']}\n\n"
                       "theorem Snap.both : Snap.double 0 = 0 ∧ Snap.double 1 ≥ 1 := ⟨Snap.zero, by simp⟩")
    assert merged["check"]["ok"] and merged["snapshot"]["status"] == "proved", merged
    step("snapshot: sorry shows in its status; branches merge")

    rejected, _ = create("theorem broken : 1 = 2 := rfl")
    assert "snapshot" not in rejected and rejected["check"]["messages"][0]["severity"] == "error", rejected

    # Macros are allowed on a default pool, because they are kernel-re-verified.
    macro_snap, _ = create("macro \"two\" : term => `((2 : Nat))\ndef via_macro : Nat := two")
    assert macro_snap["check"]["ok"] and macro_snap["snapshot"]["verified"], macro_snap
    assert on(macro_snap, "example : via_macro = 2 := rfl")["ok"]
    inherits, _ = create(f"{macro_snap['snapshot']['header']}\n\ndef via_import : Nat := two")
    assert inherits["snapshot"]["verified"], inherits
    opted_out = post("/snapshots", {"code": "macro \"three\" : term => `((3 : Nat))", "verify": False, "timeout": 600})
    created.append(opted_out)
    assert "snapshot" in opted_out, opted_out
    assert not opted_out["snapshot"]["verified"], opted_out
    step("snapshot: macros are re-checked unless the request opts out")

    # Everything else skips the re-check, until it is asked for.
    assert not root["snapshot"]["verified"] and not child["snapshot"]["verified"], (root, child)
    status, verified = call("POST", f"/snapshots/{child['snapshot']['id']}/verify")
    assert status == 200 and verified["snapshot"]["verified"], verified
    assert verified["snapshot"]["id"] == child["snapshot"]["id"], verified
    assert verified["checked"] == [root["snapshot"]["module"], child["snapshot"]["module"]], verified
    assert verified["snapshot"]["status"] == "sorry" and "failure" not in verified, verified
    assert call("GET", f"/snapshots/{root['snapshot']['id']}")[1]["verified"]
    assert call("POST", f"/snapshots/{child['snapshot']['id']}/verify")[1]["checked"] == []
    asked, _ = create(f"{sibling['snapshot']['header']}\n\ntheorem Snap.one : Snap.double 1 = 2 := rfl")
    assert not asked["snapshot"]["verified"], asked
    asked = post("/snapshots", {"code": f"{sibling['snapshot']['header']}\n\ntheorem Snap.one : Snap.double 1 = 2 := rfl",
                                "verify": True, "timeout": 600})
    created.append(asked)
    assert asked["snapshot"]["verified"], asked
    step("snapshot: verify on request, at creation or later, keeps sorry's status")

    status, info = call("GET", f"/snapshots/{child['snapshot']['id']}")
    assert status == 200 and info["imports"] == [root["snapshot"]["module"]], info
    assert info["id"] == child["snapshot"]["id"] and info["module"] == child["snapshot"]["module"], info
    delete_snapshots(created)
    r = on(merged, "example : True := trivial")
    assert r["error"]["kind"] == "not_found", r
    step("snapshot: errors aren't kept; read, and modules go with their last snapshot")


def auto_implicit():
    unbound = "theorem t : foo = foo := rfl"
    opted_in = f"set_option autoImplicit true in\n{unbound}"

    def names_foo(messages):
        return any(m["severity"] == "error" and "foo" in m["text"] for m in messages)

    r = check(unbound)
    assert not r["ok"] and names_foo(r["messages"]), r
    r = post("/snapshots", {"code": unbound, "timeout": 600})
    assert "snapshot" not in r and names_foo(r["check"]["messages"]), r
    assert check(opted_in)["ok"]
    kept = post("/snapshots", {"code": opted_in, "timeout": 600})
    assert kept["check"]["ok"] and "snapshot" in kept, kept
    delete_snapshots([kept])
    # Lean recovers from the error, so a session reports it in `messages`.
    s = post("/sessions", {"code": "theorem t : foo = foo := by sorry"})
    assert names_foo(s["messages"]), s
    step("autoImplicit is off unless code turns it on")


def create_snapshot(code, **extra):
    r = post("/snapshots", {"code": code, "timeout": 600, **extra})
    assert "snapshot" in r, r
    return r


def spec_audit():
    def audit(spec):
        query = urllib.parse.urlencode({"against": spec["snapshot"]["id"]})
        status, out = call("GET", f"/snapshots/audit?{query}")
        assert status == 200, out
        return out

    statement = "theorem e2e_target (n : Nat) : n + 0 = n"
    skeleton = create_snapshot(f"{statement} := sorry")
    out = audit(skeleton)
    assert out["clean"] is False, out
    assert [(t["name"], t["status"]) for t in out["targets"]] == [("e2e_target", "sorry")], out
    replaces = {"snapshot_id": skeleton["snapshot"]["id"]}
    proof = create_snapshot(f"{statement} := rfl", replaces=replaces, verify=True)
    out = audit(skeleton)
    assert out["clean"] is True and out["heads"] == [proof["snapshot"]["id"]], out
    delete_snapshots([proof, skeleton])
    step("spec audit: a run is clean once its proof is verified")


def allow_by_name():
    f = "def f : Nat → Nat\n  | 0 => 0\n  | n + 1 => {}\n\ntheorem t : f 0 = 0 := {}"
    skeleton = create_snapshot(f.format("n", "sorry"))
    # Lean's matcher `f.match_1` is a declaration too, but the right-hand
    # sides don't change it.
    replaces = {"snapshot_id": skeleton["snapshot"]["id"], "allow": {"definitions": ["f"]}}
    changed = create_snapshot(f.format("n + 2", "rfl"), replaces=replaces)
    assert "f" in changed["replacement"]["changed_definitions"], changed
    delete_snapshots([changed, skeleton])
    step("allow: a pattern-matching definition's value can change by name")


def allow_through_a_lost_snapshot():
    f = "def f : Nat → Nat\n  | 0 => 0\n  | n + 1 => {}"
    allow = {"definitions": ["f"]}
    base1 = create_snapshot(f.format("n"))
    skeleton = create_snapshot(f"{base1['snapshot']['header']}\n\ntheorem t : f 0 = 0 := sorry")
    base2 = create_snapshot(f.format("n + 2"), replaces={"snapshot_id": base1["snapshot"]["id"], "allow": allow})
    # The proof imports Base2 instead of Base1: the comparison follows `t`'s
    # statement into Base1, which it lost, and finds `f` changed there.
    replaces = {"snapshot_id": skeleton["snapshot"]["id"], "allow": allow}
    proof = create_snapshot(f"{base2['snapshot']['header']}\n\ntheorem t : f 0 = 0 := rfl", replaces=replaces)
    assert "f" in proof["replacement"]["changed_definitions"], proof
    delete_snapshots([proof, skeleton, base2, base1])
    step("allow: a definition changed in a snapshot the code no longer builds on")


def labels():
    def create(run):
        return post("/snapshots", {"code": SNAPSHOT_ROOT, "labels": {"e2e": "1", "run": run}, "timeout": 600})["snapshot"]

    def listed(*filters):
        query = urllib.parse.urlencode([("label", f) for f in filters])
        status, out = call("GET", f"/snapshots?{query}")
        assert status == 200, (filters, status, out)
        return [x["id"] for x in out["snapshots"]], out

    first, second = create("a"), create("b")
    ids, out = listed("e2e:1")
    assert ids == [second["id"], first["id"]], out
    assert {x["module"] for x in out["snapshots"]} == {first["module"]}, out
    status, patched = call("PATCH", f"/snapshots/{first['id']}", {"labels": {"run": "c"}})
    assert status == 200 and patched["labels"] == {"e2e": "1", "run": "c"}, patched
    assert listed("run:c")[0] == [first["id"]]
    step("labels: list, filter, edit")

    status, gone = call("DELETE", f"/snapshots/{first['id']}")
    assert status == 200 and gone["removed"] == [], gone
    r = check(f"{second['header']}\n\nexample : Snap.double 3 = 6 := by simp")
    assert r["ok"], r
    status, gone = call("DELETE", f"/snapshots/{second['id']}")
    assert status == 200 and gone["removed"] == [second["module"]], gone
    step("labels: modules go with their last snapshot")


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

    def on(created, body):
        return check(f"{created['snapshot']['header']}\n\n{body}")

    root = create(MODULE_ROOT)
    assert root["check"]["ok"] and root["snapshot"]["status"] == "proved", root
    assert root["snapshot"]["header"] == f"module\npublic import {root['snapshot']['module']}", root
    assert on(root, "example : MSnap.double 3 = 6 := by simp [MSnap.double_eq]")["ok"]
    assert not on(root, "#check MSnap.hidden")["ok"]
    # In a module, an imported definition unfolds only if it is exposed; plain
    # code sees through it.
    assert not on(root, "example : MSnap.opaque 1 = 2 := rfl")["ok"]
    assert check(f"import {root['snapshot']['module']}\n\nexample : MSnap.opaque 1 = 2 := rfl")["ok"]
    step("module snapshot: public and exposed declarations carry, the rest doesn't")

    child = create(f"{root['snapshot']['header']}\n\npublic theorem MSnap.cheat : 1 = 2 := by\n"
                   "  have h : False := sorry\n  exact h.elim")
    assert child["snapshot"]["status"] == "sorry", child
    assert child["check"]["reasons"] == ["axiom: MSnap.cheat uses sorryAx"], child
    r = on(child, "public theorem uses : 1 = 2 := MSnap.cheat")
    assert r["reasons"] == ["axiom: uses uses sorryAx"], r
    assert not child["snapshot"]["verified"], child
    step("module snapshot: sorry in a proof body is still reported")

    status, verified = call("POST", f"/snapshots/{child['snapshot']['id']}/verify")
    assert status == 200 and verified["snapshot"]["verified"], verified
    assert verified["snapshot"]["status"] == "sorry", verified
    assert verified["checked"] == [root["snapshot"]["module"], child["snapshot"]["module"]], verified
    step("module snapshot: verify re-checks the chain")

    code = f"{root['snapshot']['header']}\n\npublic theorem t (n : Nat) : MSnap.double n = n + n := by sorry"
    s = post("/sessions", {"code": code})
    done = post(f"/states/{s['states'][0]['id']}/tactic", {"tactic": "exact MSnap.double_eq n"})
    assert done["complete"], done
    proof = code.replace("by sorry", "MSnap.double_eq n")
    assert post(f"/sessions/{s['id']}/check", {"code": proof, "timeout": 600})["ok"]
    step("module snapshot: sessions in module mode")

    plain = create(f"import {root['snapshot']['module']}\n\ntheorem on_module : MSnap.double 1 = 2 := rfl")
    assert plain["check"]["ok"], plain
    mixed = create(f"module\npublic import {plain['snapshot']['module']}\n\npublic def x : Nat := 1")
    assert "snapshot" not in mixed and "non-`module`" in mixed["check"]["messages"][0]["text"], mixed
    delete_snapshots(created)
    step("module snapshot: plain code imports it, a module can't import plain code")


def snapshot_search():
    run = str(int(time.time()))
    # The comment makes it a new module, which this release has exported.
    code = (f"-- e2e {run}\n/-- Tripling a number adds it to itself twice. -/\n"
            "theorem e2e_triple (n : Nat) : 3 * n = n + n + n := by omega\n")
    created = post("/snapshots", {"code": code, "labels": {"e2e-search": run}, "timeout": 600})["snapshot"]

    def hits(scope, *modules):
        query = {"description": "tripling a number", "name": "e2e_triple", "snapshots": scope}
        status, out = call("POST", "/search", {"queries": [query]})
        assert status == 200, out
        return [h for h in out["results"][0]["hits"] if h["module"] in (modules or [created["module"]])]

    def until(f, what):
        for _ in range(60):
            if f():
                return
            time.sleep(1)
        sys.exit(f"snapshot search: timed out waiting for {what}")

    labelled = {"labels": {"e2e-search": run}}
    until(lambda: hits(labelled), "a new snapshot")
    hit = hits(labelled)[0]
    assert hit["library"] == "PisaSnap" and hit["snapshot_ids"] == [created["id"]], hit
    assert hit["flags"]["imports"] == created["header"], hit
    r = check(f"{hit['flags']['imports']}\n\nexample : 3 * 2 = 2 + 2 + 2 := e2e_triple 2")
    assert r["ok"], r
    assert not hits({"labels": {"e2e-search": "other"}}) and not hits("none")
    step("snapshot search: a new snapshot is found by label, and its header loads it")

    # The same statement, so the replacement is kept.
    proof = post("/snapshots", {"code": code.replace(f"-- e2e {run}", f"-- e2e {run} proof"),
                                "labels": {"e2e-search": run}, "timeout": 600,
                                "replaces": {"snapshot_id": created["id"]}})["snapshot"]
    until(lambda: hits(labelled, proof["module"]) and not hits(labelled), "a replacement")
    assert hits(labelled, proof["module"])[0]["snapshot_ids"] == [proof["id"]]
    step("snapshot search: a replaced snapshot gives way to what replaces it")

    both = (created["module"], proof["module"])
    moved = {"labels": {"e2e-search": run + "-moved"}}
    status, _ = call("PATCH", f"/snapshots/{proof['id']}", {"labels": moved["labels"]})
    assert status == 200
    until(lambda: hits(moved, proof["module"]) and not hits(labelled, *both), "a relabel")
    for snapshot in (proof, created):
        status, _ = call("DELETE", f"/snapshots/{snapshot['id']}")
        assert status == 200
    until(lambda: not hits("all", *both), "a delete")
    step("snapshot search: follows relabels and deletes")


def mathlib():
    r = check("import Mathlib\n\ntheorem t (a b : ℝ) : (a + b)^2 = a^2 + 2*a*b + b^2 := by ring")
    assert r["ok"], r
    # Mathlib and QuantumInfo both declare Commute.log_left, so they can't be
    # imported together; the error must say so rather than leave names unknown.
    r = check("import Mathlib\nimport QuantumInfo\n#check MState")
    assert not r["ok"] and "Commute.log_left" in r["messages"][0]["text"], r
    r = check("import Physlib\n\n#check SpaceTime")
    assert r["messages"][0]["pos"]["line"] == 3, r
    r = check("import Mathlib\n\n" + TACTIC_PROOF, detail=["tactics"])
    assert r["tactics"][0]["pos"] == {"line": 4, "column": 2}, r
    s = post("/sessions", {"code": "import Mathlib\n\ntheorem t (x : ℝ) (h : 0 < x) : 0 < x ^ 2 := by sorry"})
    assert "error" not in s, s
    done = post(f"/states/{s['states'][0]['id']}/tactic", {"tactic": "positivity"})
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
    auto_implicit()
    spec_audit()
    allow_by_name()
    allow_through_a_lost_snapshot()
    labels()
    module_snapshots()
    if args.mathlib:
        mathlib()
    if args.search:
        snapshot_search()
    print("all end-to-end checks passed")
