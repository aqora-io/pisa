#!/usr/bin/env python3
"""End-to-end checks against a running `docker compose` stack with real Lean.

    PROJECT=lean-core docker compose up -d --build
    python3 scripts/e2e.py            # core checks
    python3 scripts/e2e.py --mathlib  # also Mathlib and Physlib (full image)
    python3 scripts/e2e.py --search   # also snapshot search (PISA_SEARCH_QDRANT
                                      # set and compose --profile search)
    python3 scripts/e2e.py --beam     # also open documents

Tactic sessions run on Lean Beam, so the workers need a Beam slot, as Compose
gives them by default. It stops and restarts each of the stack's two workers
to check that tactic sessions resume on the other (gate 3), so run it against
a dev stack only.
Pass --no-failover to skip that step, e.g. against Kubernetes through a
port-forward.
"""

import argparse
import io
import json
import os
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request

BASE = os.environ.get("PISA_URL", "http://localhost:8000")
COMPOSE = ["docker", "compose"]


def call(method, path, body=None, timeout=900, base=BASE, scope=None):
    data = None if body is None else json.dumps(body).encode()
    headers = {"content-type": "application/json"}
    if scope is not None:
        headers["pisa-scope"] = scope
    req = urllib.request.Request(base + path, data=data, method=method, headers=headers)
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
    # steps must replay the session's steps on the other worker.
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

    # Everything else is re-checked too, unless the request opts out.
    assert root["snapshot"]["verified"] and child["snapshot"]["verified"], (root, child)
    assert child["snapshot"]["status"] == "sorry", child
    one = f"{sibling['snapshot']['header']}\n\ntheorem Snap.one : Snap.double 1 = 2 := rfl"
    opted = post("/snapshots", {"code": one, "verify": False, "timeout": 600})
    created.append(opted)
    assert not opted["snapshot"]["verified"], opted
    status, verified = call("POST", f"/snapshots/{opted['snapshot']['id']}/verify")
    assert status == 200 and verified["snapshot"]["verified"], verified
    assert verified["snapshot"]["id"] == opted["snapshot"]["id"], verified
    assert verified["checked"] == [opted["snapshot"]["module"]], verified
    assert "failure" not in verified, verified
    assert call("POST", f"/snapshots/{opted['snapshot']['id']}/verify")[1]["checked"] == []
    asked = post("/snapshots", {"code": one, "verify": True, "timeout": 600})
    created.append(asked)
    assert asked["snapshot"]["verified"], asked
    step("snapshot: re-checked by default, or later when the request opts out, keeping sorry's status")

    status, info = call("GET", f"/snapshots/{child['snapshot']['id']}")
    assert status == 200 and info["imports"] == [root["snapshot"]["module"]], info
    assert info["id"] == child["snapshot"]["id"] and info["module"] == child["snapshot"]["module"], info
    delete_snapshots(created)
    # A worker that didn't run the delete imports the module until it looks
    # it up again, within PISA_SNAPSHOT_CONFIRM_SECS (60 by default).
    deadline = time.monotonic() + 75
    while "error" not in (r := on(merged, "example : True := trivial")) and time.monotonic() < deadline:
        time.sleep(2)
    assert r["error"]["kind"] == "not_found", r
    step("snapshot: errors aren't kept; read, and modules go with their last snapshot")


def auto_implicit():
    unbound = "theorem t : foo = foo := rfl"
    opted_in = f"set_option autoImplicit true in\n{unbound}"

    def names_foo(messages):
        return any(m["severity"] == "error" and "foo" in m["text"] for m in messages)

    def hinted(messages):
        return any(m["severity"] == "info" and "autoImplicit" in m["text"] for m in messages)

    r = check(unbound)
    assert not r["ok"] and names_foo(r["messages"]) and hinted(r["messages"]), r
    # A misspelt lemma in a proof is no name the option would bind.
    r = check("example : True := by exact foo_bar")
    assert not r["ok"] and not hinted(r["messages"]), r
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


def sessions_and_their_own_declarations():
    # A tactic runs in the context of the session's code, so it sees what the
    # code declares before the `sorry`.
    s = post("/sessions", {"code": "def foo : Nat := 1\n\ntheorem t : foo = 1 := by sorry"})
    root = s["states"][0]
    assert root["goals"] == ["⊢ foo = 1"], s
    unfolded = post(f"/states/{root['id']}/tactic", {"tactic": "unfold foo"})
    assert unfolded["state"]["goals"] == ["⊢ 1 = 1"], unfolded
    done = post(f"/states/{root['id']}/tactic", {"tactic": "decide"})
    assert done["complete"], done
    # And what a snapshot it imports declares.
    base = create_snapshot("def foo : Nat := 1")
    s = post("/sessions", {"code": f"{base['snapshot']['header']}\n\ntheorem t : foo = 1 := by sorry"})
    done = post(f"/states/{s['states'][0]['id']}/tactic", {"tactic": "decide"})
    assert done["complete"], done
    delete_snapshots([base])
    step("sessions: a tactic sees what its code declares before the sorry, and a snapshot's")


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
    skeleton = create_snapshot(f"{statement} := sorry", verify=True)
    out = audit(skeleton)
    assert out["clean"] is False, out
    assert [(t["name"], t["status"]) for t in out["targets"]] == [("e2e_target", "sorry")], out
    replaces = {"snapshot_id": skeleton["snapshot"]["id"]}
    proof = create_snapshot(f"{statement} := rfl", replaces=replaces, verify=True)
    out = audit(skeleton)
    assert out["clean"] is True and out["heads"] == [proof["snapshot"]["id"]], out
    delete_snapshots([proof, skeleton])
    step("spec audit: a run is clean once its spec and proof are verified")


def statements_under_replaces():
    swap = "theorem t (a b : Prop) (h : a ∧ b) : b ∧ a"
    skeleton = create_snapshot(f"def H : Nat := sorry\n\n{swap} := sorry", verify=True)
    replaces = {"snapshot_id": skeleton["snapshot"]["id"]}
    # A changed statement is refused, and the refusal says which and how.
    weaker = "def H : Nat := sorry\n\ntheorem t (a b : Prop) (h : a ∧ b) : a ∧ b := h"
    r = post("/snapshots", {"code": weaker, "replaces": replaces, "timeout": 600})
    assert "snapshot" not in r, r
    assert [(c["name"], c["change"]) for c in r["check"]["changes"]] == [("t", "type")], r
    # An untouched stub, whose `sorry` Lean labels with where it is, and a
    # proof by `match`, whose matcher is Lean's, keep the statements.
    by_match = f"def H : Nat := sorry\n\n\n{swap} :=\n  match h with\n  | ⟨x, y⟩ => ⟨y, x⟩"
    proof = create_snapshot(by_match, replaces=replaces, verify=True)
    assert proof["replacement"]["filled"] == ["t"], proof
    out = audit_against(skeleton)
    assert out["comparisons"][0]["replacement"]["filled"] == ["t"], out
    delete_snapshots([proof, skeleton])
    step("replaces: statements compare on their canonical form, from verified records")


def audit_against(spec, allow=(), review=False):
    query = [("against", spec["snapshot"]["id"])] + [("allow", n) for n in allow]
    query = urllib.parse.urlencode(query + ([("review", "true")] if review else []))
    status, out = call("GET", f"/snapshots/audit?{query}")
    assert status == 200, out
    return out


def allow_by_name():
    f = "def f : Nat → Nat\n  | 0 => 0\n  | n + 1 => {}\n\ntheorem t : f 0 = 0 := {}"
    skeleton = create_snapshot(f.format("n", "sorry"), verify=True)
    # Lean's matcher `f.match_1` is a declaration too, but the right-hand
    # sides don't change it.
    replaces = {"snapshot_id": skeleton["snapshot"]["id"], "allow": {"definitions": ["f"]}}
    changed = create_snapshot(f.format("n + 2", "rfl"), replaces=replaces, verify=True)
    assert "f" in changed["replacement"]["changed_definitions"], changed
    # The audit compares again from the verified records, under what the
    # leader allows, not the link.
    out = audit_against(skeleton)
    assert out["comparisons"][0]["reasons"] == ["statement: f has a different value"], out
    assert out["clean"] is False, out
    out = audit_against(skeleton, allow=["f"])
    assert out["comparisons"][0]["replacement"]["changed_definitions"] == ["f"], out
    assert out["clean"] is True, out
    delete_snapshots([changed, skeleton])
    step("allow: a pattern-matching definition's value can change by name")


def holes():
    f = "def f : Nat → Nat\n  | 0 => 0\n  | n + 1 => {}\n\ndef g : Nat := {}\n\ntheorem t : f 0 = g := {}"
    skeleton = create_snapshot(f.format("n", "0", "sorry"), holes=["f"], verify=True)
    assert skeleton["snapshot"]["holes"] == ["f"], skeleton
    # A replacement fills the hole without `allow`.
    filled = create_snapshot(f.format("n + 2", "0", "rfl"), replaces={"snapshot_id": skeleton["snapshot"]["id"]},
                             verify=True)
    assert filled["replacement"]["changed_definitions"] == ["f"], filled
    out = audit_against(skeleton)
    assert out["clean"] is True, out
    delete_snapshots([filled])
    # `g` isn't a hole: changed under the link's own `allow`, the run isn't clean.
    replaces = {"snapshot_id": skeleton["snapshot"]["id"], "allow": {"definitions": ["g"]}}
    changed = create_snapshot(f.format("n", "f 0", "rfl"), replaces=replaces, verify=True)
    assert changed["replacement"]["changed_definitions"] == ["g"], changed
    out = audit_against(skeleton)
    assert out["comparisons"][0]["reasons"] == ["statement: g has a different value"], out
    assert out["clean"] is False, out
    delete_snapshots([changed, skeleton])
    step("holes: a spec's holes are filled without allow, and nothing else changes in a clean run")


def refute_amend_and_resubmit():
    false = "theorem e2e_false : ∀ n : Nat, n + 0 = n + 1"
    true = "theorem e2e_true (n : Nat) : n + 0 = n"
    spec = create_snapshot(f"def e2e_fee : Nat := sorry\n\n{false} := sorry\n\n{true} := sorry",
                           holes=["e2e_fee"], verify=True)
    spec_id = spec["snapshot"]["id"]
    head = create_snapshot(f"def e2e_fee : Nat := 1\n\n{false} := sorry\n\n{true} := rfl",
                           replaces={"snapshot_id": spec_id}, verify=True)
    # `e2e_false` is false: a proof of its negation on the spec refutes it.
    negation = "theorem e2e_refuted : ¬ type_of% @e2e_false := fun h => by have := h 0; omega"
    refuter = create_snapshot(f"{spec['snapshot']['header']}\n\n{negation}",
                              refutes={"snapshot_id": spec_id, "target": "e2e_false"})
    assert refuter["refutation"]["theorem"] == "e2e_refuted", refuter
    out = audit_against(spec)
    assert out["refutations"] == [refuter["refutation"]] and out["clean"] is False, out
    refuted = [t.get("refuted_by") for t in out["targets"] if t["name"] == "e2e_false"]
    assert refuted == [[refuter["snapshot"]["id"]]], out
    step("refute: a proof of ¬ type_of% @t on the spec refutes t, and the run isn't clean")

    amended = create_snapshot(f"def e2e_fee : Nat := sorry\n\n{true} := sorry", holes=["e2e_fee"],
                              amends={"snapshot_id": spec_id, "reason": "e2e_false is false"})
    assert [c["name"] for c in amended["amendment"]["changes"]] == ["e2e_false"], amended
    to = amended["snapshot"]["id"]
    done = post("/snapshots/resubmit", {"from": spec_id, "to": to, "timeout": 600})
    assert done["ok"] and done["items"][0]["snapshot_id"] == head["snapshot"]["id"], done
    carried = done["items"][0]["new_snapshot_id"]
    out = audit_against(amended)
    assert out["clean"] is True and out["heads"] == [carried], out
    assert out["amendments"] == [amended["amendment"]], out
    for x in [carried, to, refuter["snapshot"]["id"], head["snapshot"]["id"], spec_id]:
        status, gone = call("DELETE", f"/snapshots/{x}")
        assert status == 200, gone
    step("amend: a spec amended during a run, its run resubmitted onto it, is clean")


def through_links_and_settle():
    lemma = "theorem e2e_lemma (n : Nat) : n + 0 = n"
    lemma_spec = create_snapshot(f"{lemma} := sorry", verify=True)
    header = lemma_spec["snapshot"]["header"]
    main = "theorem e2e_main (n : Nat) : n + 0 + 0 = n"
    spec = create_snapshot(f"{header}\n\n{main} := sorry", verify=True)
    spec_id = spec["snapshot"]["id"]
    # The target is proved with the lemma while the lemma is still `sorry`,
    # and the lemma is proved after.
    head = create_snapshot(f"{header}\n\n{main} := (e2e_lemma (n + 0)).trans (e2e_lemma n)",
                           replaces={"snapshot_id": spec_id}, verify=True)
    lemma_head = create_snapshot(f"{lemma} := rfl",
                                 replaces={"snapshot_id": lemma_spec["snapshot"]["id"]}, verify=True)
    out = audit_against(spec)
    [target] = out["targets"]
    assert target["status"] == "sorry" and target["linked_status"] == "proved", out
    through = {"name": "e2e_lemma", "snapshot_id": lemma_spec["snapshot"]["id"],
               "head": lemma_head["snapshot"]["id"]}
    assert target["through"] == [through] and out["clean"] is False, out
    assert out["unsettled"] == [{"head": head["snapshot"]["id"], "swaps": [
        {"snapshot_id": lemma_spec["snapshot"]["id"], "head": lemma_head["snapshot"]["id"]}]}], out
    step("through links: a target proved with a lemma proved later holds through the lemma's run")

    done = post("/snapshots/settle", {"against": [spec_id], "timeout": 600})
    assert done["ok"] and len(done["created"]) == 1, done
    settled = done["created"][0]["new_snapshot_id"]
    out = audit_against(spec)
    [target] = out["targets"]
    assert out["clean"] is True and out["heads"] == [settled], out
    assert target["status"] == "proved" and "linked_status" not in target, out
    verdict = post("/snapshots/accept", {"against": [spec_id]})
    assert verdict["accepted"] and verdict["clean"], verdict
    step("settle: the head is rebuilt onto the lemma's proof, and the run is clean and accepted")

    # The copy builds on the lemma's head, as this does.
    retyped = (f"{lemma_head['snapshot']['header']}\n\n"
               "theorem e2e_main (n : Nat) : n + 0 + 0 + 0 = n := by simp")
    started = time.time()
    refused = post("/snapshots", {"code": retyped, "replaces": {"snapshot_id": settled}, "timeout": 600})
    built = time.time() - started
    assert "snapshot" not in refused and refused["check"]["ok"] is False, refused
    assert refused["refused"] == "proof", refused
    assert "statement: e2e_main has a different type" in refused["check"]["reasons"], refused
    started = time.time()
    kept = create_snapshot(retyped)
    again = time.time() - started
    # The kept build's record, with its build's time: nothing was built.
    assert kept["check"]["time"] == refused["check"]["time"], (kept, refused)
    print(f"  refused in {built:.1f}s, created again in {again:.1f}s")
    for x in [kept["snapshot"]["id"], settled, head["snapshot"]["id"], spec_id,
              lemma_head["snapshot"]["id"], lemma_spec["snapshot"]["id"]]:
        status, gone = call("DELETE", f"/snapshots/{x}")
        assert status == 200, gone
    step("replaces: a refused build is kept, so creating its code again builds nothing")


def run_names(outside):
    lemma = "theorem e2e_named_lemma (n : Nat) : n + 0 = n"
    spec = create_snapshot(f"{lemma} := sorry", verify=True, name="e2e.lemma")
    assert spec["snapshot"]["run"] == "e2e.lemma", spec
    main = "theorem e2e_named_main (n : Nat) : n + 0 + 0 = n"
    target = create_snapshot(f"import PisaRun.e2e.lemma\n\n{main} := sorry", verify=True, name="e2e.main")
    target_id = target["snapshot"]["id"]
    proof = f"import PisaRun.e2e.lemma\n\n{main} := (e2e_named_lemma (n + 0)).trans (e2e_named_lemma n)"
    checked = check(proof)
    legend = [{"name": "e2e.lemma", "module": spec["snapshot"]["module"], "snapshot_id": spec["snapshot"]["id"]}]
    # Proved but for the lemma, still `sorry` there.
    assert "error" not in checked and checked["runs"] == legend, checked
    head = create_snapshot(proof, replaces={"snapshot_id": target_id}, verify=True)
    lemma_head = create_snapshot(f"{lemma} := rfl", replaces={"snapshot_id": spec["snapshot"]["id"]}, verify=True)
    checked = check(proof)
    assert checked["ok"] and checked["runs"][0]["snapshot_id"] == lemma_head["snapshot"]["id"], checked
    status, run = call("GET", "/runs/e2e.lemma")
    assert status == 200 and run["snapshot_id"] == lemma_head["snapshot"]["id"], run
    assert run["root"] == spec["snapshot"]["id"], run
    step("run names: code imports a run by name, which follows the run's head")

    # The head imports the lemma's spec, which settling moves to its proof;
    # the copy keeps the text as written.
    done = post("/snapshots/settle", {"against": [target_id], "timeout": 600})
    assert done["ok"] and len(done["created"]) == 1, done
    settled = done["created"][0]["new_snapshot_id"]
    status, detail = call("GET", f"/snapshots/{settled}")
    assert status == 200 and detail["source"] == proof, detail
    assert lemma_head["snapshot"]["module"] in detail["code"] and detail["run"] == "e2e.main", detail
    step("run names: a settled copy keeps the code as written, importing the run by name")

    # Text resent after a conflict, replacing the head the answer names.
    resend = f"{proof}\n-- resent"
    refused = post("/snapshots", {"code": resend, "replaces": {"snapshot_id": target_id}, "timeout": 600})
    assert refused["check"]["error"]["kind"] == "conflict", refused
    assert refused["refused"] == "conflict" and refused["heads"] == [settled], refused
    resent = create_snapshot(resend, replaces={"snapshot_id": settled}, verify=True)
    assert resent["snapshot"]["run"] == "e2e.main", resent
    status, detail = call("GET", f"/snapshots/{resent['snapshot']['id']}")
    assert status == 200 and detail["source"] == resend, detail
    out = audit_against(target)
    assert out["clean"] is True and out["heads"] == [resent["snapshot"]["id"]], out
    step("run names: a conflict is resent, imports unchanged, and the run is clean on the proved lemma")

    # Against both specs, each pair is named by its run.
    specs = [target_id, spec["snapshot"]["id"]]
    query = urllib.parse.urlencode([("against", s) for s in specs])
    with urllib.request.urlopen(BASE + f"/snapshots/export?{query}", timeout=900) as r:
        archive = r.read()
    with tarfile.open(fileobj=io.BytesIO(archive), mode="r:gz") as tar:
        names = tar.getnames()
        configs = [json.load(tar.extractfile(f"pisa-run/comparator/{n}.json")) for n in (1, 2)]
    for path in ["PisaRun/e2e/main.lean", "PisaSpec/e2e/main.lean", "PisaRun/e2e/lemma.lean",
                 "PisaSpec/e2e/lemma.lean"]:
        assert f"pisa-run/{path}" in names, names
    judged = {("PisaSpec.e2e.main", "PisaRun.e2e.main"), ("PisaSpec.e2e.lemma", "PisaRun.e2e.lemma")}
    assert {(c["challenge_module"], c["solution_module"]) for c in configs} == judged, configs
    # Acceptance judges pisa's own builds, named by their hash.
    verdict = post("/snapshots/accept", {"against": specs})
    assert verdict["accepted"] and verdict["clean"], verdict
    assert all((p["challenge_module"], p["solution_module"]) == (p["spec_module"], p["head_module"])
               for p in verdict["pairs"]), verdict
    if outside:
        built_outside(archive)
    step("run names: the export names each pair by its run, builds, and the run is accepted")

    # Named again, the run keeps one name.
    status, run = call("PUT", "/runs/e2e.renamed", {"snapshot_id": resent["snapshot"]["id"]})
    assert status == 200 and run["heads"] == [resent["snapshot"]["id"]], run
    assert call("GET", "/runs/e2e.main")[0] == 404
    step("run names: a run is named again by PUT /runs/{name}")
    for x in [resent["snapshot"]["id"], settled, head["snapshot"]["id"], target_id,
              lemma_head["snapshot"]["id"], spec["snapshot"]["id"]]:
        status, gone = call("DELETE", f"/snapshots/{x}")
        assert status == 200, gone


def scopes():
    a, b = "e2e=a", "e2e=b"
    code = "theorem e2e_scoped : 1 + 1 = 2 := rfl"
    body = {"code": code, "name": "e2e.scoped", "key": "e2e-scoped", "timeout": 600}
    status, mine = call("POST", "/snapshots", body, scope=a)
    assert status == 200 and mine["snapshot"]["labels"] == {"e2e": "a"}, mine
    mine = mine["snapshot"]
    assert call("GET", f"/snapshots/{mine['id']}", scope=b)[0] == 404
    status, listed = call("GET", "/snapshots", scope=b)
    assert status == 200 and mine["id"] not in [s["id"] for s in listed["snapshots"]], listed
    # The same key and name are the other scope's to take.
    status, theirs = call("POST", "/snapshots", {**body, "code": f"{code}\n-- b"}, scope=b)
    assert status == 200 and theirs["snapshot"]["id"] != mine["id"], theirs
    theirs = theirs["snapshot"]
    uses = "import PisaRun.e2e.scoped\n\nexample : 1 + 1 = 2 := e2e_scoped"
    for scope, made in [(a, mine), (b, theirs)]:
        status, r = call("POST", "/check", {"code": uses, "timeout": 600}, scope=scope)
        assert status == 200 and r["ok"] and r["runs"][0]["snapshot_id"] == made["id"], r
    status, r = call("POST", "/check", {"code": uses, "timeout": 600}, scope="e2e=c")
    assert r["error"]["kind"] == "not_found", r
    by_hash = f"{mine['header']}\n\nexample : 1 + 1 = 2 := e2e_scoped"
    status, r = call("POST", "/check", {"code": by_hash, "timeout": 600}, scope=b)
    assert r["error"]["kind"] == "not_found", r
    assert call("PATCH", f"/snapshots/{mine['id']}", {"labels": {"e2e": None}}, scope=a)[0] == 400
    step("scopes: what a scope makes is its own, and other scopes reach none of it")
    for scope, made in [(a, mine), (b, theirs)]:
        assert call("DELETE", f"/snapshots/{made['id']}", scope=b if scope == a else a)[0] == 404
        status, gone = call("DELETE", f"/snapshots/{made['id']}", scope=scope)
        assert status == 200, gone


def built_outside(archive):
    """Unpacks a run's export in a worker container and builds and judges it
    there with the commands its README gives: Lean and Lake only, no pisa."""
    worker = worker_containers()[0]
    with tempfile.NamedTemporaryFile(suffix=".tar.gz") as f:
        f.write(archive)
        f.flush()
        docker("cp", f.name, f"{worker}:/tmp/e2e-run.tar.gz")
    script = ("set -e; rm -rf /tmp/e2e-run; mkdir /tmp/e2e-run; cd /tmp/e2e-run; "
              "tar xzf /tmp/e2e-run.tar.gz; cd pisa-run; lake build; "
              "lake comparator --paranoid --config comparator/1.json")
    out = subprocess.run(["docker", "exec", worker, "bash", "-c", script], capture_output=True, text=True)
    docker("exec", worker, "rm", "-rf", "/tmp/e2e-run", "/tmp/e2e-run.tar.gz")
    assert out.returncode == 0, (out.stdout[-3000:], out.stderr[-3000:])


def comparator(outside):
    spec = ("def cost : Nat := {}\n\ndef good (n : Nat) : Prop := {}\n\n"
            "theorem t : good cost := {}\n\ntheorem t_nonvacuous : ∃ n, good n := {}")
    skeleton = create_snapshot(spec.format("sorry", "n ≤ 10", "sorry", "sorry"), holes=["cost"], verify=True)
    spec_id = skeleton["snapshot"]["id"]
    proofs = ("3", "n ≤ 10", "by unfold good; decide", "⟨0, by unfold good; decide⟩")
    filled = create_snapshot(spec.format(*proofs), replaces={"snapshot_id": spec_id}, verify=True)
    assert audit_against(skeleton)["clean"] is True

    query = urllib.parse.urlencode([("against", spec_id)])
    with urllib.request.urlopen(BASE + f"/snapshots/export?{query}", timeout=900) as r:
        assert r.headers.get_content_type() == "application/gzip", r.headers
        archive = r.read()
    with tarfile.open(fileobj=io.BytesIO(archive), mode="r:gz") as tar:
        names = tar.getnames()
        config = json.load(tar.extractfile("pisa-run/comparator/1.json"))
    assert config["theorem_names"] == ["t", "t_nonvacuous"] and config["definition_names"] == ["cost"], config
    assert config["solution_module"] == filled["snapshot"]["module"], config
    head_file = filled["snapshot"]["module"].replace(".", "/")
    assert f"pisa-run/{head_file}.lean" in names and "pisa-run/lakefile.toml" in names, names
    step("comparator: a run exports as a Lake project")
    if outside:
        built_outside(archive)
        step("comparator: an unpacked export builds and is accepted as its README says, outside pisa")

    verdict = post("/snapshots/accept", {"against": [spec_id]})
    assert verdict["accepted"] and verdict["clean"] and verdict["pairs"][0]["exit_code"] == 0, verdict
    assert verdict["heads"] == [filled["snapshot"]["id"]], verdict
    status, stored = call("GET", f"/snapshots/accept/{verdict['id']}")
    assert status == 200 and stored["id"] == verdict["id"] and stored["accepted"], stored
    step("accept: a head that fills the spec's holes is accepted, paranoid, and the verdict is kept")
    delete_snapshots([filled])

    # `good` isn't a hole: changed under the head's own `allow`, the audit
    # isn't clean, and comparator refuses it too.
    replaces = {"snapshot_id": spec_id, "allow": {"definitions": ["good"]}}
    cheat = create_snapshot(spec.format("3", "True", "trivial", "⟨0, trivial⟩"), replaces=replaces, verify=True)
    assert audit_against(skeleton)["clean"] is False
    verdict = post("/snapshots/accept", {"against": [spec_id], "paranoid": False})
    pair = verdict["pairs"][0]
    assert not verdict["accepted"] and not verdict["clean"] and pair["exit_code"] == 1, verdict
    assert pair["error"]["kind"] == "refused", pair
    assert "does not match" in pair["output"] and "good" in pair["output"], pair
    delete_snapshots([cheat, skeleton])
    step("accept: a head that changes what a statement means is refused by the audit and comparator")


def circuit(program, budget="3", promise="n < 4", accepts="n < c.length + 2", gates="",
            fits="by decide", solves="fun n h => by unfold accepts program; unfold promise at h; simp; omega",
            witness="⟨0, by unfold promise; decide⟩", budget_last=False):
    """A small circuit problem: a program of gates, within a budget, that
    accepts every input the promise allows."""
    budget = f"def budget : Nat := {budget}\n\n"
    program = f"def program : List Gate := {program}\n\n"
    return (f"inductive Gate where\n  | h\n  | cnot{gates}\n\n"
            "def cost (c : List Gate) : Nat := c.length\n\n"
            f"def promise (n : Nat) : Prop := {promise}\n\n"
            f"def accepts (c : List Gate) (n : Nat) : Prop := {accepts}\n\n"
            + (program + budget if budget_last else budget + program)
            + f"theorem fits : cost program ≤ budget := {fits}\n\n"
            f"theorem solves : ∀ n, promise n → accepts program n := {solves}\n\n"
            f"theorem nonvacuous : ∃ n, promise n := {witness}\n")


def review():
    skeleton = circuit("sorry", fits="sorry", solves="sorry", witness="sorry")
    spec = create_snapshot(skeleton, holes=["program"], verify=True)
    spec_id = spec["snapshot"]["id"]
    honest = create_snapshot(circuit("[Gate.h, Gate.cnot]"), replaces={"snapshot_id": spec_id}, verify=True)
    out = audit_against(spec, review=True)
    assert out["clean"] is True, out
    review_list = out["review"][0]
    listed = {d["name"]: d for d in review_list["declarations"]}
    wanted = {"Gate", "Gate.h", "Gate.cnot", "cost", "promise", "accepts", "budget", "program",
              "fits", "solves", "nonvacuous"}
    assert wanted <= listed.keys() and all(d["origin"] == "spec" for d in listed.values()), listed
    program = listed["program"]
    assert program["hole"] and "Gate.cnot" in program["value"] and "[" not in program["value"], program
    assert not listed["budget"].get("hole") and "value" not in listed["budget"], listed["budget"]
    assert {"List", "Nat"} <= set(review_list["library"]), review_list
    assert post("/snapshots/accept", {"against": [spec_id], "paranoid": False})["accepted"]
    delete_snapshots([honest])
    step("review: a head lists what the spec's statements use, its hole by value, and is accepted")

    # Each of these misses the claim with a valid proof; made outside the
    # holes, the audit and comparator refuse it.
    moves = [
        ("budget", circuit("[Gate.h, Gate.cnot]", budget="cost program", fits="Nat.le_refl _", budget_last=True)),
        ("accepts", circuit("[]", accepts="True", solves="fun _ _ => trivial")),
        ("promise", circuit("[]", promise="n < 1")),
        ("Gate", circuit("[Gate.magic, Gate.magic]", gates="\n  | magic")),
    ]
    for name, code in moves:
        replaces = {"snapshot_id": spec_id, "allow": {"definitions": [name]}}
        head = create_snapshot(code, replaces=replaces, verify=True)
        out = audit_against(spec)
        reasons = out["comparisons"][0].get("reasons", [])
        assert out["clean"] is False and any(name in r for r in reasons), (name, out)
        verdict = post("/snapshots/accept", {"against": [spec_id], "paranoid": False})
        assert not verdict["accepted"], (name, verdict)
        delete_snapshots([head])
    delete_snapshots([spec])
    step("review: a budget, an acceptance condition, a promise or a gate changed outside the holes is refused")

    # A spec that leaves the budget as a hole allows the first move: only the
    # review list's value shows it.
    loose = create_snapshot(skeleton, holes=["program", "budget"], verify=True)
    gamed = create_snapshot(moves[0][1], replaces={"snapshot_id": loose["snapshot"]["id"]}, verify=True)
    out = audit_against(loose, review=True)
    assert out["clean"] is True, out
    budget = {d["name"]: d for d in out["review"][0]["declarations"]}["budget"]
    assert budget["hole"] and "cost" in budget["value"] and "program" in budget["value"], budget
    # It names the other hole, whose value the reviewer reads with it.
    assert budget.get("holes") == ["program"], budget
    assert post("/snapshots/accept", {"against": [loose["snapshot"]["id"]], "paranoid": False})["accepted"]
    delete_snapshots([gamed, loose])
    step("review: a budget left as a hole and set to the candidate's own cost is clean, and the review shows its value")


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
    # Module-system code is re-checked when it is created: the build sees it
    # only as importers do.
    assert root["snapshot"]["verified"] and child["snapshot"]["verified"], child
    step("module snapshot: sorry in a proof body is still reported")

    status, verified = call("POST", f"/snapshots/{child['snapshot']['id']}/verify")
    assert status == 200 and verified["snapshot"]["verified"], verified
    assert verified["snapshot"]["status"] == "sorry", verified
    assert verified["checked"] == [], verified
    step("module snapshot: re-checked at creation")

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


def documents(failover=True):
    code = "theorem t (a b : Nat) : a + b = b + a := by\n  sorry"
    d = post("/documents", {"code": code, "timeout": 600})
    assert d["revision"] == 1 and len(d["sorries"]) == 1, d
    sorry = d["sorries"][0]
    assert sorry["pos"] == {"line": 2, "column": 2} and "⊢ a + b = b + a" in sorry["goal"], d
    path = f"/documents/{d['id']}"
    g = post(path + "/goals", {"at": [{"line": 2}, {"line": 1, "column": 0}]})
    at_sorry, outside = g["at"]
    assert at_sorry["pos"] == {"line": 2, "column": 2} and at_sorry["goals"] == [sorry["goal"]], g
    assert outside["goals"] == [], g
    step("document goals")

    candidates = ["omega", "rfl", "#eval 1", "sorry"]
    omega, rfl, ev, admitted = post(path + "/probes", {"at": {"sorry": 0}, "candidates": candidates})["results"]
    assert omega["ok"] and omega["closed"], omega
    assert not rfl["ok"] and rfl["messages"], rfl
    assert ev["error"]["kind"] == "policy", ev
    assert admitted["ok"] and not admitted["closed"], admitted
    step("document probes")

    proved = "theorem t (a b : Nat) : a + b = b + a := by\n  omega"
    status, e = call("PATCH", path, {"code": proved, "revision": 1, "timeout": 600})
    assert status == 200 and e["revision"] == 2 and e["sorries"] == [], (status, e)
    status, e = call("PATCH", path, {"code": code, "revision": 1})
    assert status == 409 and e["error"]["kind"] == "conflict", (status, e)
    step("document edits")

    # With each worker stopped in turn, the other answers, reopening the
    # document if it didn't hold it.
    if failover:
        for worker in worker_containers():
            docker("stop", worker)
            wait_for_workers(1)
            g = post(path + "/goals", {"at": [{"line": 2}], "timeout": 600})
            assert g["revision"] == 2 and g["at"][0]["goals"], g
            docker("start", worker)
            wait_for_workers(2)
        step("documents reopen on another worker")

    status, _ = call("DELETE", path)
    assert status == 204, status
    status, e = call("POST", path + "/goals", {"at": [{"line": 1}]})
    assert status == 404, (status, e)
    step("document close")


def mathlib():
    r = check("import Mathlib\n\ntheorem t (a b : ℝ) : (a + b)^2 = a^2 + 2*a*b + b^2 := by ring")
    assert r["ok"], r
    # Up to Physlib v4.35.0, Mathlib and QuantumInfo both declared
    # Commute.log_left; now they import together.
    r = check("import Mathlib\nimport QuantumInfo\n#check MState")
    assert r["ok"], r
    r = check("import Mathlib\nimport PhyslibAlpha\n#check ClassicalFieldTheory.Local.Lagrangian")
    assert r["ok"], r
    # So the search index is exported from all four together.
    r = check("import Mathlib\nimport Physlib\nimport QuantumInfo\nimport PhyslibAlpha\n#check MState")
    assert r["ok"], r
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
    p.add_argument("--beam", action="store_true", help="also check documents (PISA_BEAM_SLOTS)")
    args = p.parse_args()
    if not args.no_failover:
        wait_for_workers(2)
    core(failover=not args.no_failover)
    snapshots()
    auto_implicit()
    sessions_and_their_own_declarations()
    spec_audit()
    statements_under_replaces()
    allow_by_name()
    holes()
    refute_amend_and_resubmit()
    through_links_and_settle()
    run_names(outside=not args.mathlib and not args.no_failover)
    scopes()
    # The export of a project with packages fetches them to build.
    comparator(outside=not args.mathlib and not args.no_failover)
    review()
    allow_through_a_lost_snapshot()
    labels()
    module_snapshots()
    if args.beam:
        documents(failover=not args.no_failover)
    if args.mathlib:
        mathlib()
    if args.search:
        snapshot_search()
    print("all end-to-end checks passed")
