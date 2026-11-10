#!/usr/bin/env python3
"""End-to-end checks against a running `docker compose` stack with real Lean.

    PROJECT=lean-core docker compose up -d --build
    python3 scripts/e2e.py            # core checks
    python3 scripts/e2e.py --mathlib  # also Mathlib and Physlib (full image)
    python3 scripts/e2e.py --search   # also snapshot search (PISA_SEARCH_QDRANT
                                      # set and compose --profile search)
    python3 scripts/e2e.py --beam     # also open documents (workers started
                                      # with PISA_BEAM_SLOTS=1)

It stops and restarts each of the stack's two workers to check that tactic
sessions resume on the other (gate 3), so run it against a dev stack only.
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


def sessions_and_their_own_declarations():
    hidden = "can't use what its code declares"

    def warned(session):
        return [m["pos"]["line"] for m in session["messages"] if m["severity"] == "warning" and hidden in m["text"]]

    # The REPL makes each state on the environment the code started from.
    s = post("/sessions", {"code": "def foo : Nat := 1\n\ntheorem t : foo = 1 := by sorry"})
    assert warned(s) == [1], s
    status, out = call("POST", f"/states/{s['states'][0]['id']}/tactic", {"tactic": "decide"})
    errors = [m for m in out.get("messages", []) if m["severity"] == "error"]
    assert (status == 422 or errors) and "foo" in json.dumps(out), (status, out)
    # A snapshot's declarations are in the environment of what imports it.
    base = create_snapshot("def foo : Nat := 1")
    s = post("/sessions", {"code": f"{base['snapshot']['header']}\n\ntheorem t : foo = 1 := by sorry"})
    assert warned(s) == [], s
    done = post(f"/states/{s['states'][0]['id']}/tactic", {"tactic": "decide"})
    assert done["complete"], done
    delete_snapshots([base])
    step("sessions: a state can't use what its code declares before the sorry, and can a snapshot's")


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


def comparator():
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

    verdict = post("/snapshots/comparator", {"against": [spec_id]})
    assert verdict["accepted"] and verdict["pairs"][0]["exit_code"] == 0, verdict
    step("comparator: a head that fills the spec's holes is accepted, paranoid")
    delete_snapshots([filled])

    # `good` isn't a hole: changed under the head's own `allow`, the audit
    # isn't clean, and comparator refuses it too.
    replaces = {"snapshot_id": spec_id, "allow": {"definitions": ["good"]}}
    cheat = create_snapshot(spec.format("3", "True", "trivial", "⟨0, trivial⟩"), replaces=replaces, verify=True)
    assert audit_against(skeleton)["clean"] is False
    verdict = post("/snapshots/comparator", {"against": [spec_id], "paranoid": False})
    pair = verdict["pairs"][0]
    assert not verdict["accepted"] and pair["exit_code"] == 1, verdict
    assert "does not match" in pair["output"] and "good" in pair["output"], pair
    delete_snapshots([cheat, skeleton])
    step("comparator: a head that changes what a statement means is refused")


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
    assert post("/snapshots/comparator", {"against": [spec_id], "paranoid": False})["accepted"]
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
        verdict = post("/snapshots/comparator", {"against": [spec_id], "paranoid": False})
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
    assert post("/snapshots/comparator", {"against": [loose["snapshot"]["id"]], "paranoid": False})["accepted"]
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
    comparator()
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
