"""Tests the Python client against a real server and worker that run
fake-repl in place of Lean. Build the binaries first:

    cargo build -p pisa-server -p pisa-worker -p pisa-tests
"""

import os
import socket
import subprocess
import threading
import time
from pathlib import Path

import pytest

from pisa_client import CapacityError, Client, LeanError, NotFoundError, PisaError

BIN = Path(os.environ.get("PISA_BIN_DIR", Path(__file__).parents[3] / "target" / "debug"))


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


@pytest.fixture(scope="module")
def db(tmp_path_factory):
    return str(tmp_path_factory.mktemp("db") / "pisa.db")


@pytest.fixture(scope="module")
def url(db):
    http = free_port()
    # The server and the worker share the job queue in this file.
    env = {**os.environ, "RUST_LOG": "warn"}
    server = subprocess.Popen(
        [BIN / "pisa-server", "--http", f"127.0.0.1:{http}", "--database-url", db,
         "--max-wait-secs", "1"],
        env=env,
    )
    url = f"http://127.0.0.1:{http}"
    client = Client(url)
    # The server creates the tables first, so the two don't race to.
    wait_for(lambda: client.health()["status"] == "ok")
    worker = subprocess.Popen(
        [BIN / "pisa-worker", "--database-url", db, "--version", "v",
         "--slots", "2", "--repl", BIN / "fake-repl", "--lean", BIN / "fake-lean",
         # Snapshots are built and re-checked by the `pisa` tool, here its stand-in.
         "--tool", BIN / "fake-pisa",
         # Runs are accepted with `lake comparator`, here a stand-in.
         "--lake", BIN / "fake-lake",
         # Sessions run on Lean Beam, here its stand-in.
         "--beam-slots", "1", "--beam-bin", BIN / "fake-beam"],
        env=env,
    )
    wait_for(lambda: health(url)["workers"] == 1)
    yield url
    worker.terminate()
    server.terminate()


def wait_for(ready):
    for _ in range(100):
        try:
            if ready():
                return
        except Exception:
            pass
        time.sleep(0.05)
    pytest.fail("cluster did not start")


def health(url):
    import json
    import urllib.request

    with urllib.request.urlopen(url + "/health") as r:
        return json.load(r)


def test_health(url, db):
    assert Client(url).health()["status"] == "ok"

    # A server told to stop keeps serving for a few seconds, its /health
    # failing with a 503 that carries its health.
    http = free_port()
    server = subprocess.Popen(
        [BIN / "pisa-server", "--http", f"127.0.0.1:{http}", "--database-url", db],
        env={**os.environ, "RUST_LOG": "warn"},
    )
    client = Client(f"http://127.0.0.1:{http}")
    try:
        wait_for(lambda: client.health()["status"] == "ok")
        server.terminate()
        seen = []
        wait_for(lambda: seen.append(client.health()) or seen[-1]["status"] == "draining")
        assert seen[-1]["workers"] == 1 and seen[-1]["search"] is None
    finally:
        server.terminate()
        server.wait(timeout=30)


def test_check(url):
    client = Client(url)
    ok = client.check("import Foo\ntheorem t : 1 = 1 := rfl")
    assert ok["ok"]
    assert ok["declarations"][0]["name"] == "t"
    assert isinstance(ok["time"], float) and isinstance(ok["ok"], bool)
    # A summary, unless asked for in full.
    assert ok["declarations"][0] == {"name": "t", "kind": "theorem", "axioms": [], "status": "proved"}
    full = client.check("import Foo\ntheorem t : 1 = 1 := rfl", detail=["declarations"])
    assert full["declarations"][0]["axioms"] == ["propext"] and "type_hash" in full["declarations"][0]
    created = client.create_snapshot("import Foo\n\ntheorem py_slim : 1 = 1 := rfl")
    assert "type_hash" not in created["check"]["declarations"][0]
    detail = client.get_snapshot(created["snapshot"]["id"], detail=["declarations"])
    assert "type_hash" in detail["check"]["declarations"][0]

    results = client.check_batch(["#check Nat", "theorem t : False := sorry"])
    assert len(results) == 2 and all("id" not in r for r in results)
    assert results[0]["messages"][0]["text"] == "Nat : Type"
    assert results[0]["messages"][0]["pos"] == {"line": 1, "column": 0}
    assert results[1]["reasons"] == ["axiom: t uses sorryAx"]

    code = "import Foo\ntheorem t : True := by\n  trivial"
    assert "tactics" not in client.check(code)
    [tactic] = client.check(code, detail=["tactics"])["tactics"]
    assert tactic["tactic"] == "trivial" and tactic["pos"] == {"line": 3, "column": 2}
    assert tactic["goals_before"] == ["⊢ goal 2"] and tactic["goals_after"] == []
    [result] = client.check_batch([code], detail=["tactics"])
    assert result["tactics"] == [tactic]
    with pytest.raises(ValueError):
        client.check(code, detail=["bogus"])

    code = "import Foo\ntheorem t : True := by\n  exact foo"
    [at] = client.check(code, detail=["terms"], at=[(3, 9)])["at"]
    assert at["pos"] == {"line": 3, "column": 9}
    assert at["tactic"]["tactic"] == "exact foo" and at["tactic"]["goals_before"] == ["⊢ goal 2"]
    assert at["term"]["term"] == "foo" and at["term"]["expected_type"] == "goal 2"
    [result] = client.check_batch([code], detail=["terms"], at=[(3, 9)])
    assert result["at"] == [at]
    # Without terms, the tactic alone.
    job = client.submit_check(code, at=[(3, 9)])
    assert client.wait_job(job)["at"] == [{"pos": at["pos"], "tactic": at["tactic"]}]
    with pytest.raises(PisaError) as e:
        client.check(code, detail=["terms"])
    assert e.value.kind == "bad_request"

    timed_out = Client(url, timeout=1).check("SLEEP 3000")
    assert not timed_out["ok"] and timed_out["error"]["kind"] == "timeout"


def test_session(url):
    client = Client(url, timeout=5)
    session = client.start_session("theorem t : 1 = 1 := by sorry")
    root = session["states"][0]
    assert root["goals"] == ["⊢ True"] and root["pos"]["line"] == 1

    step = client.apply_tactic(root["id"], "simp")
    assert not step["complete"] and "pos" not in step["state"]
    done = client.apply_tactic(step["state"]["id"], "rfl")
    assert done["complete"] and done["state"]["goals"] == []

    with pytest.raises(LeanError, match="failed") as e:
        client.apply_tactic(root["id"], "fail")
    assert (e.value.kind, e.value.status) == ("lean", 422)
    with pytest.raises(LeanError) as e:
        client.apply_tactic(root["id"], "run_tac pure ()")
    assert e.value.kind == "policy"

    verdict = client.check_session(session["id"], "theorem t : 1 = 1 := by rfl")
    assert verdict["ok"]
    weaker = client.check_session(session["id"], "theorem t : 2 = 2 := by rfl")
    assert weaker["reasons"] == ["statement: t has a different type"]


def test_jobs(url):
    client = Client(url, timeout=5)
    job = client.submit_check("#check Nat")
    assert (job["kind"], job["status"]) == ("check", "queued")
    assert isinstance(job["id"], str)
    assert client.wait_job(job)["ok"]

    session = client.wait_job(client.submit_start_session("theorem t : 1 = 1 := by sorry"))
    state = session["states"][0]["id"]
    applied = client.submit_apply_tactic(state, "rfl")
    assert applied["kind"] == "apply_tactic"
    assert client.wait_job(applied)["complete"]
    # The answer raises what the request would have.
    with pytest.raises(LeanError):
        client.wait_job(client.submit_apply_tactic(state, "fail"))
    verdict = client.submit_check_session(session["id"], "theorem t : 1 = 1 := by rfl")
    assert client.wait_job(verdict)["ok"]

    made = client.wait_job(client.submit_create_snapshot("def jobbed := 1", labels={"run": "jobs"}))
    assert made["snapshot"]["labels"] == {"run": "jobs"}
    verifying = client.submit_verify_snapshot(made["snapshot"]["id"])
    assert verifying["kind"] == "verify_snapshot"
    assert client.wait_job(verifying)["snapshot"]["verified"]

    slow = client.submit_check("SLEEP 1000\n#check Nat")
    pending = client.poll_job(slow)
    assert pending["done"] is False
    assert pending["job"]["id"] == slow["id"]
    assert pending["job"]["status"] in ("queued", "running")
    polled = client.poll_job(slow, wait=10)
    assert polled["done"] is True and polled["answer"]["ok"]
    assert client.cancel_job(slow)["status"] == "done"
    with pytest.raises(ValueError):
        client.wait_job({"id": "x"})


def test_errors(url):
    with pytest.raises(PisaError) as e:
        Client(url).apply_tactic("not-a-key", "rfl")
    assert (e.value.kind, e.value.status) == ("bad_request", 400)

    with pytest.raises(NotFoundError):
        Client(url).apply_tactic("v~abc~0123", "rfl")

    with pytest.raises(CapacityError) as e:
        Client(url, version="nope").check("#check Nat")
    assert e.value.status == 429

    with pytest.raises(ValueError):
        Client("not a url")

    with pytest.raises(PisaError) as e:
        Client(f"http://127.0.0.1:{free_port()}").health()
    assert e.value.kind is None


def test_threads_share_a_client(url):
    client = Client(url)
    results = []
    threads = [
        threading.Thread(target=lambda i=i: results.append(client.check(f"theorem t{i} : 1 = 1 := rfl")))
        for i in range(8)
    ]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    assert len(results) == 8 and all(r["ok"] for r in results)


def test_snapshots(url):
    client = Client(url, timeout=5)
    # Not re-checked, so that `verify_snapshot` has it to do.
    created = client.create_snapshot("import Foo\ntheorem base : 1 = 1 := rfl", verify=False)
    assert created["check"]["ok"] and "replacement" not in created
    base = created["snapshot"]
    assert base["status"] == "proved" and "dirty" not in base
    snapshot_id, name = base["id"], base["module"]
    assert snapshot_id.startswith(base["version"] + "~")
    assert base["header"] == f"import Foo\nimport {name}"
    assert client.check(base["header"] + "\nneeds base")["ok"]

    rejected = client.create_snapshot("theorem t : 1 = 1 := error_here")
    assert "snapshot" not in rejected and not rejected["check"]["ok"]

    detail = client.get_snapshot(snapshot_id)
    assert {k: detail[k] for k in base} == base
    assert detail["imports"] == [] and not detail["verified"]
    assert [e["name"] for e in detail["exports"]] == ["base"]

    verified = client.verify_snapshot(snapshot_id)
    assert verified["snapshot"]["verified"] and verified["snapshot"]["id"] == snapshot_id
    assert verified["checked"] == [name] and "failure" not in verified
    assert client.get_snapshot(snapshot_id)["verified"]
    forged = client.create_snapshot("import Foo\ntheorem forge : 1 = 1 := rfl", verify=True)
    assert "snapshot" not in forged
    opted_out = client.create_snapshot('import Foo\nmacro "m" : term => `(1)', verify=False)
    assert not opted_out["snapshot"]["verified"]

    labelled = client.create_snapshot("import Foo\ntheorem labelled : 1 = 1 := rfl", labels={"k": "v"})["snapshot"]
    assert labelled["labels"] == {"k": "v"}
    assert client.get_snapshot(labelled["id"])["labels"] == {"k": "v"}
    code = "import Foo\ntheorem keyed : 1 = 1 := rfl"
    keyed = client.create_snapshot(code, key="run-1")["snapshot"]
    assert client.create_snapshot(code, key="run-1")["snapshot"]["id"] == keyed["id"]
    with pytest.raises(PisaError) as e:
        client.get_snapshot(name)
    assert (e.value.kind, e.value.status) == ("bad_request", 400)

    assert client.delete_snapshot(snapshot_id) == {"removed": [name], "deleted": [snapshot_id]}
    with pytest.raises(NotFoundError):
        client.get_snapshot(snapshot_id)


def test_replaces(url):
    client = Client(url, timeout=5)
    old = client.create_snapshot("import Foo\ntheorem replaced : 1 = 1 := sorry")["snapshot"]["id"]
    link = {"snapshot_id": old}
    proof = client.create_snapshot("import Foo\ntheorem replaced : 1 = 1 := rfl", replaces=link)
    assert proof["replacement"] == {"filled": ["replaced"]}
    assert proof["snapshot"]["replaces"] == link
    assert client.get_snapshot(proof["snapshot"]["id"])["replaces"] == link

    weakened = client.create_snapshot("import Foo\ntheorem replaced : True := trivial", replaces=link, fork=True)
    assert "snapshot" not in weakened and not weakened["check"]["ok"]
    assert weakened["check"]["reasons"] == ["statement: replaced has a different type"]

    skeleton = "import Foo\ndef g : Nat := 1\ntheorem uses_g : g = g := sorry"
    old = client.create_snapshot(skeleton)["snapshot"]["id"]
    fixed = "import Foo\ndef g : Nat := 2\ntheorem uses_g : g = g := rfl"
    job = client.submit_create_snapshot(fixed, replaces={"snapshot_id": old, "allow": {"definitions": ["g"]}})
    assert job["kind"] == "create_snapshot"
    done = client.wait_job(job)
    assert done["replacement"]["changed_definitions"] == ["g"]
    assert done["replacement"]["affected"] == ["uses_g"]
    with pytest.raises(ValueError):
        client.create_snapshot(fixed, replaces={"snapshot_id": old, "allow": "definitions"})
    allow = {"definitions": ["nope"]}
    unknown = client.create_snapshot(fixed, replaces={"snapshot_id": old, "allow": allow}, fork=True)
    assert "snapshot" not in unknown and unknown["check"]["error"]["kind"] == "bad_request"
    with pytest.raises(ValueError):
        client.create_snapshot(fixed, replaces=old)


def test_replacing_only_a_head(url):
    client = Client(url, timeout=5)
    old = client.create_snapshot("import Foo\ntheorem head_t : 1 = 1 := sorry")["snapshot"]["id"]
    link = {"snapshot_id": old}
    first = client.create_snapshot("import Foo\ntheorem head_t : 1 = 1 := rfl", replaces=link)["snapshot"]
    other = "import Foo\ntheorem head_t : 1 = 1 := rfl\ntheorem head_u : 2 = 2 := rfl"
    refused = client.create_snapshot(other, replaces=link)
    assert "snapshot" not in refused and refused["check"]["error"]["kind"] == "conflict"
    assert refused["heads"] == [first["id"]]
    forked = client.create_snapshot(other, replaces=link, fork=True)
    assert forked["snapshot"]["replaces"] == link and "heads" not in forked
    job = client.submit_create_snapshot(other + "\ntheorem head_v : 3 = 3 := rfl", replaces=link, fork=True)
    assert client.wait_job(job)["snapshot"]["replaces"] == link


def test_dropping_a_branch(url):
    client = Client(url, timeout=5)
    spec = client.create_snapshot("import Foo\ntheorem branch_t : 1 = 1 := sorry")["snapshot"]["id"]
    link = {"snapshot_id": spec}
    first = client.create_snapshot("import Foo\ntheorem branch_t : 1 = 1 := rfl", replaces=link)["snapshot"]["id"]
    other = client.create_snapshot("import Foo\ntheorem branch_t : 1 = 1 := by decide", replaces=link, fork=True)
    deeper = "import Foo\ntheorem branch_t : 1 = 1 := rfl\ntheorem branch_u : 2 = 2 := rfl"
    last = client.create_snapshot(deeper, replaces={"snapshot_id": first})["snapshot"]["id"]
    assert len(client.audit_snapshots(against=[spec])["forks"]) == 1

    dropped = client.delete_snapshot(first, with_replacers=True)
    assert dropped["deleted"] == [first, last]
    audit = client.audit_snapshots(against=[spec])
    assert audit["heads"] == [other["snapshot"]["id"]] and "forks" not in audit
    with pytest.raises(NotFoundError):
        client.get_snapshot(last)


def test_rebuild_snapshots(url):
    client = Client(url, timeout=5)
    base = client.create_snapshot("import Foo\ntheorem rebuilt : 1 = 1 := sorry")["snapshot"]
    on_top = base["header"] + "\n\ntheorem on_top : 2 = 2 := rfl"
    above = client.create_snapshot(on_top, labels={"suite": "rebuild"})["snapshot"]
    listed = client.list_dependents(base["id"], {"suite": "rebuild"})
    assert listed["snapshots"] == [above]

    replaces = {"snapshot_id": base["id"]}
    proof = client.create_snapshot("import Foo\ntheorem rebuilt : 1 = 1 := rfl", replaces=replaces)["snapshot"]
    plan = client.rebuild_snapshots(base["id"], proof["id"], {"suite": "rebuild"}, dry_run=True)
    assert plan["ok"] and plan["changes"] == [] and "created" not in plan and "replaced" not in plan
    with pytest.raises(PisaError, match="answered at once") as e:
        client.submit_rebuild_snapshots(base["id"], proof["id"], {"suite": "rebuild"}, dry_run=True)
    assert (e.value.kind, e.value.status) == ("internal", 200)
    [item] = plan["plan"]
    assert (item["snapshot_id"], item["module"], item["level"]) == (above["id"], above["module"], 1)
    assert item["replaces"] == {"snapshot_id": above["id"]}
    made = client.create_snapshot(item["code"], labels=item["labels"], key=item["key"], replaces=item["replaces"])["snapshot"]
    assert made["module"] == item["new_module"]
    run = client.rebuild_snapshots(base["id"], proof["id"], {"suite": "rebuild"})
    assert run["ok"] and run["created"][0]["new_snapshot_id"] == made["id"]
    job = client.submit_rebuild_snapshots(from_=base["id"], to=proof["id"], labels={"suite": "rebuild"}, retire=True)
    assert job["kind"] == "rebuild_snapshots"
    assert client.wait_job(job)["retired"] == [above["id"]]
    assert client.get_snapshot(base["id"])["id"] == base["id"]
    with pytest.raises(PisaError) as e:
        client.rebuild_snapshots(base["id"], proof["id"], {}, dry_run=True)
    assert (e.value.kind, e.value.status) == ("bad_request", 400)


def test_audit(url):
    client = Client(url, timeout=5)
    a = client.create_snapshot("import Foo\naxiom py_assumed : 1 = 1")["snapshot"]
    uses = a["header"] + "\n\ntheorem py_uses : 1 = 1 := py_assumed"
    b = client.create_snapshot(uses, labels={"suite": "audit"})["snapshot"]
    assert b["status"] == "conditional"
    audit = client.audit_snapshots({"suite": "audit"})
    assert audit["summary"]["conditional"] == 1
    assert [x["name"] for x in audit["assumptions"]] == ["py_assumed"]
    assert audit["assumptions"][0]["used_by"] == ["py_uses"]
    assert audit["assumptions"][0]["snapshot_id"] == a["id"]


def test_audit_against(url):
    client = Client(url, timeout=5)
    spec = client.create_snapshot("import Foo\ntheorem py_target : 2 = 2 := sorry", verify=True)["snapshot"]
    audit = client.audit_snapshots(against=[spec["id"]])
    assert audit["clean"] is False and audit["heads"] == [spec["id"]]
    assert audit["targets"][0]["status"] == "sorry"
    proof = client.create_snapshot(
        "import Foo\ntheorem py_target : 2 = 2 := rfl", replaces={"snapshot_id": spec["id"]}, verify=True
    )["snapshot"]
    audit = client.audit_snapshots(against=[spec["id"]])
    assert audit["clean"] is True and audit["heads"] == [proof["id"]]
    assert audit["targets"] == [
        {"name": "py_target", "spec": spec["id"], "head": proof["id"], "status": "proved", "verified": True,
         "accepted": True}
    ]
    with pytest.raises(ValueError):
        client.audit_snapshots({"suite": "audit"}, against=[spec["id"]])
    with pytest.raises(ValueError):
        client.audit_snapshots(against=[])
    with pytest.raises(ValueError):
        client.audit_snapshots({"suite": "audit"}, allow=["f"])


def test_holes(url):
    client = Client(url, timeout=5)
    spec = client.create_snapshot("import Foo\ndef py_hole : Nat := 1\ntheorem py_h : py_hole = py_hole := sorry",
                                  holes=["py_hole"], verify=True)["snapshot"]
    assert spec["holes"] == ["py_hole"]
    filled = client.create_snapshot("import Foo\ndef py_hole : Nat := 2\ntheorem py_h : py_hole = py_hole := rfl",
                                    replaces={"snapshot_id": spec["id"]}, verify=True)
    assert filled["replacement"]["changed_definitions"] == ["py_hole"]
    assert filled["snapshot"]["holes"] == ["py_hole"]
    assert client.audit_snapshots(against=[spec["id"]])["clean"] is True


def test_audit_allowing(url):
    client = Client(url, timeout=5)
    spec = client.create_snapshot("import Foo\ndef py_f : Nat := 1\ntheorem py_t : py_f = py_f := sorry",
                                  verify=True)["snapshot"]
    replaces = {"snapshot_id": spec["id"], "allow": {"definitions": ["py_f"]}}
    client.create_snapshot("import Foo\ndef py_f : Nat := 2\ntheorem py_t : py_f = py_f := rfl",
                           replaces=replaces, verify=True)
    assert client.audit_snapshots(against=[spec["id"]])["clean"] is False
    assert client.audit_snapshots(against=[spec["id"]], allow=["py_f"])["clean"] is True


def test_documents(url):
    client = Client(url, timeout=10)
    code = "import Foo\n\ntheorem t : 𝓝 = 𝓝 := by\n  sorry\n"
    document = client.open_document(code)
    assert document["revision"] == 1 and len(document["sorries"]) == 1
    goals = client.document_goals(document["id"], [{"line": 4}])
    assert goals["at"][0]["goals"] == ["⊢ sorry"]
    probes = client.probe_document(document["id"], {"sorry": 0}, ["rfl", "omega"])
    assert probes["results"][0]["closed"] and not probes["results"][1]["ok"]
    edited = client.edit_document(document["id"], 1, code.replace("sorry", "rfl"))
    assert edited["revision"] == 2 and edited["sorries"] == []
    assert client.close_document(document["id"]) is None
    with pytest.raises(NotFoundError):
        client.document_goals(document["id"], [{"line": 4}])
    with pytest.raises(ValueError):
        client.document_goals(document["id"], [{"line": 4}], mode="during")


def test_review_export_and_accept(url):
    client = Client(url, timeout=30)
    spec = client.create_snapshot("import Foo\ndef py_cost : Nat := sorry\ntheorem py_c : py_cost = py_cost := sorry",
                                  holes=["py_cost"])["snapshot"]
    head = client.create_snapshot("import Foo\ndef py_cost : Nat := 3\ntheorem py_c : py_cost = py_cost := rfl",
                                  replaces={"snapshot_id": spec["id"]})["snapshot"]
    audit = client.audit_snapshots(against=[spec["id"]], review=True)
    cost = {d["name"]: d for d in audit["review"][0]["declarations"]}["py_cost"]
    assert cost["hole"] and cost["value"] == "3", cost
    with pytest.raises(ValueError):
        client.audit_snapshots(review=True)

    assert client.export_run([spec["id"]])[:2] == b"\x1f\x8b"

    accepted = client.accept_run([spec["id"]])
    assert accepted["accepted"] and accepted["clean"] and accepted["heads"] == [head["id"]], accepted
    assert client.get_acceptance(accepted["id"])["pairs"] == accepted["pairs"]
    listed = client.list_acceptances(spec["id"])["acceptances"]
    assert [a["id"] for a in listed][-1] == accepted["id"]
    job = client.submit_accept_run([spec["id"]], paranoid=False)
    assert job["kind"] == "accept_run" and client.wait_job(job)["accepted"]


def test_snapshot_labels(url):
    client = Client(url, timeout=5)
    code = "import Foo\ntheorem listed : 1 = 1 := rfl"
    first = client.create_snapshot(code, labels={"suite": "labels", "k": "v"})["snapshot"]["id"]
    second = client.create_snapshot(code, labels={"suite": "labels", "k": "w"})["snapshot"]["id"]

    def ids(listing):
        return [s["id"] for s in listing["snapshots"]]

    found = client.list_snapshots({"suite": "labels", "k": "v"})
    assert ids(found) == [first] and "next" not in found
    assert found["snapshots"][0]["labels"] == {"suite": "labels", "k": "v"}
    page = client.list_snapshots({"suite": "labels"}, limit=1)
    assert ids(page) == [second]
    assert ids(client.list_snapshots({"suite": "labels"}, limit=1, after=page["next"])) == [first]
    # Nothing replaces either, so both are heads.
    assert ids(client.list_snapshots({"suite": "labels"}, heads=True)) == [second, first]

    updated = client.update_snapshot(first, {"k": "u"})
    assert updated["id"] == first and updated["labels"] == {"suite": "labels", "k": "u"}
    assert client.list_snapshots({"suite": "labels", "k": "v"})["snapshots"] == []
    assert ids(client.list_snapshots({"suite": "labels", "k": "u"})) == [first]
    assert client.update_snapshot(first, {"k": None, "absent": None})["labels"] == {"suite": "labels"}
    with pytest.raises(PisaError) as e:
        client.update_snapshot(first, {"bad key": "x"})
    assert (e.value.kind, e.value.status) == ("bad_request", 400)
    with pytest.raises(PisaError) as e:
        client.list_snapshots(limit=0)
    assert (e.value.kind, e.value.status) == ("bad_request", 400)

    for snapshot_id in [first, second]:
        client.delete_snapshot(snapshot_id)
    assert client.list_snapshots({"suite": "labels"})["snapshots"] == []


def test_amend_resubmit_and_refute(url):
    client = Client(url, timeout=30)
    spec = client.create_snapshot(
        "import Foo\ndef py_fee : Nat := sorry\ntheorem py_f : py_fee = py_fee := sorry\ntheorem py_w : 1 = 2 := sorry",
        holes=["py_fee"])["snapshot"]
    head = client.create_snapshot(
        "import Foo\ndef py_fee : Nat := 1\ntheorem py_f : py_fee = py_fee := rfl\ntheorem py_w : 1 = 2 := sorry",
        replaces={"snapshot_id": spec["id"]})["snapshot"]
    refuted = client.create_snapshot(spec["header"] + "\ntheorem py_r : ¬ (1 = 2) := by decide",
                                     refutes={"snapshot_id": spec["id"], "target": "py_w"})
    assert refuted["refutation"]["theorem"] == "py_r", refuted
    assert client.audit_snapshots(against=[spec["id"]])["refutations"] == [refuted["refutation"]]
    with pytest.raises(ValueError):
        client.create_snapshot("x", amends={"snapshot_id": spec["id"]})
    amended = client.create_snapshot(
        "import Foo\ndef py_fee : Nat := sorry\ntheorem py_f : py_fee = py_fee := sorry",
        holes=["py_fee"], amends={"snapshot_id": spec["id"], "reason": "py_w is false"})
    assert amended["amendment"]["changes"][0]["name"] == "py_w", amended
    to = amended["snapshot"]["id"]
    plan = client.resubmit_snapshots(spec["id"], to, dry_run=True)
    assert plan["ok"] and plan["items"][0]["snapshot_id"] == head["id"], plan
    job = client.submit_resubmit_snapshots(spec["id"], to)
    assert job["kind"] == "resubmit_snapshots"
    done = client.wait_job(job)
    assert done["ok"] and "new_snapshot_id" in done["items"][0], done


def test_status_through_links_and_settle(url):
    client = Client(url, timeout=30)
    spec_l = client.create_snapshot("import Foo\n\ntheorem py_l : 1 = 1 := sorry")["snapshot"]
    spec_t = client.create_snapshot(spec_l["header"] + "\n\ntheorem py_t : 2 = 2 := sorry")["snapshot"]
    client.create_snapshot(spec_l["header"] + "\n\ntheorem py_t : 2 = 2 := by have := py_l; rfl",
                           replaces={"snapshot_id": spec_t["id"]})
    client.create_snapshot("import Foo\n\ntheorem py_l : 1 = 1 := rfl", replaces={"snapshot_id": spec_l["id"]})
    specs = [spec_l["id"], spec_t["id"]]
    audit = client.audit_snapshots(against=specs)
    top = {t["name"]: t for t in audit["targets"]}["py_t"]
    assert top["linked_status"] == "proved" and top["through"][0]["name"] == "py_l", audit
    assert client.settle_run(specs, dry_run=True)["ok"]
    job = client.submit_settle_run(specs)
    assert client.wait_job(job)["ok"]
    assert client.audit_snapshots(against=specs)["clean"]
    assert client.verify_snapshot(spec_t["id"], again=True)["checked"]


def test_run_names(url):
    client = Client(url, timeout=30)
    spec = client.create_snapshot("import Foo\ntheorem py_lemma : 1 = 1 := sorry", name="py.lemma")["snapshot"]
    assert spec["run"] == "py.lemma", spec
    code = "import PisaRun.py.lemma\n\nneeds py_lemma\ntheorem py_t : 1 = 1 := py_lemma"
    checked = client.check(code)
    assert checked["ok"] and checked["runs"] == [
        {"name": "py.lemma", "module": spec["module"], "snapshot_id": spec["id"]}], checked
    made = client.create_snapshot(code)
    assert made["runs"] == checked["runs"], made
    detail = client.get_snapshot(made["snapshot"]["id"])
    assert detail["source"] == code and spec["module"] in detail["code"], detail
