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
def url(tmp_path_factory):
    http = free_port()
    # The server and the worker share the job queue in this file.
    db = str(tmp_path_factory.mktemp("db") / "pisa.db")
    env = {**os.environ, "RUST_LOG": "warn"}
    server = subprocess.Popen(
        [BIN / "pisa-server", "--http", f"127.0.0.1:{http}", "--database-url", db,
         "--max-wait-secs", "1"],
        env=env,
    )
    url = f"http://127.0.0.1:{http}"
    client = Client(url)
    # The server creates the tables first, so the two don't race to.
    wait_for(lambda: client.health() is None)
    worker = subprocess.Popen(
        [BIN / "pisa-worker", "--database-url", db, "--version", "v",
         "--slots", "2", "--repl", BIN / "fake-repl", "--lean", BIN / "fake-lean"],
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


def test_check(url):
    client = Client(url)
    ok = client.check("import Foo\ntheorem t : 1 = 1 := rfl")
    assert ok["ok"]
    assert ok["declarations"][0]["name"] == "t"
    assert isinstance(ok["time"], float) and isinstance(ok["ok"], bool)

    results = client.check_batch([("a", "#check Nat"), ("b", "theorem t : False := sorry")])
    assert [r["id"] for r in results] == ["a", "b"]
    assert results[0]["messages"][0]["data"] == "Nat : Type"
    assert results[0]["messages"][0]["pos"] == {"line": 1, "column": 0}
    assert results[1]["reasons"] == ["axiom: t uses sorryAx"]

    code = "import Foo\ntheorem t : True := by\n  trivial"
    assert "tactics" not in client.check(code)
    [tactic] = client.check(code, detail=["tactics"])["tactics"]
    assert tactic["tactic"] == "trivial" and tactic["pos"] == {"line": 3, "column": 2}
    assert tactic["goals_before"] == ["⊢ goal 2"] and tactic["goals_after"] == []
    [result] = client.check_batch([("a", code)], detail=["tactics"])
    assert result["tactics"] == [tactic]
    with pytest.raises(ValueError):
        client.check(code, detail=["bogus"])

    timed_out = Client(url, timeout=1).check("SLEEP 3000")
    assert not timed_out["ok"] and timed_out["error"]["kind"] == "timeout"


def test_session(url):
    client = Client(url, timeout=5)
    session = client.start_session("theorem t : 1 = 1 := by sorry")
    root = session["states"][0]
    assert root["goal"] == "⊢ goal 1"

    step = client.tactic(root["state"], "simp")
    assert not step["complete"]
    done = client.tactic(step["state"], "rfl")
    assert done["complete"] and done["goals"] == []

    with pytest.raises(LeanError, match="failed") as e:
        client.tactic(root["state"], "fail")
    assert (e.value.kind, e.value.status) == ("lean", 422)
    with pytest.raises(LeanError) as e:
        client.tactic(root["state"], "run_tac pure ()")
    assert e.value.kind == "policy"

    verdict = client.check_session(session["session"], "theorem t : 1 = 1 := by rfl")
    assert verdict["ok"]
    weaker = client.check_session(session["session"], "theorem t : 2 = 2 := by rfl")
    assert weaker["reasons"] == ["statement: t has a different type"]


def test_jobs(url):
    client = Client(url, timeout=5)
    job = client.submit_check("#check Nat")
    assert (job["kind"], job["status"]) == ("check", "queued")
    assert client.wait_job(job)["ok"]

    session = client.wait_job(client.submit_session("theorem t : 1 = 1 := by sorry"))
    state = session["states"][0]["state"]
    assert client.wait_job(client.submit_tactic(state, "rfl"))["complete"]
    # The answer raises what the request would have.
    with pytest.raises(LeanError):
        client.wait_job(client.submit_tactic(state, "fail"))
    verdict = client.submit_session_check(session["session"], "theorem t : 1 = 1 := by rfl")
    assert client.wait_job(verdict)["ok"]

    made = client.wait_job(client.submit_snapshot("def jobbed := 1", labels={"run": "jobs"}))
    assert made["labels"] == {"run": "jobs"}
    assert client.wait_job(client.submit_verify(made["entry"]))["verified"]

    slow = client.submit_check("SLEEP 1000\n#check Nat")
    assert client.poll_job(slow) is None
    assert client.poll_job(slow, wait=10)["ok"]
    assert client.cancel_job(slow)["status"] == "done"
    with pytest.raises(ValueError):
        client.wait_job({"job": "x"})


def test_errors(url):
    with pytest.raises(PisaError) as e:
        Client(url).tactic("not-a-key", "rfl")
    assert (e.value.kind, e.value.status) == ("bad_request", 400)

    with pytest.raises(NotFoundError):
        Client(url).tactic("v~abc~0123", "rfl")

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
    base = client.create_snapshot("import Foo\ntheorem base : 1 = 1 := rfl")
    assert base["result"]["ok"] and not base["dirty"]
    name, entry = base["snapshot"], base["entry"]
    assert base["header"] == f"import Foo\nimport {name}"
    assert client.check(base["header"] + "\nneeds base")["ok"]

    rejected = client.create_snapshot("theorem t : 1 = 1 := error_here")
    assert "snapshot" not in rejected and "entry" not in rejected and not rejected["result"]["ok"]

    info = client.snapshot(entry)
    assert info["entry"] == entry and info["snapshot"] == name
    assert info["header"] == base["header"] and info["imports"] == []
    assert not base["verified"] and not info["verified"]

    verified = client.verify_snapshot(entry)
    assert verified["verified"] and verified["entry"] == entry and verified["checked"] == [name]
    assert "failure" not in verified
    assert client.snapshot(entry)["verified"]
    forged = client.create_snapshot("import Foo\ntheorem forge : 1 = 1 := rfl", verify=True)
    assert "snapshot" not in forged
    opted_out = client.create_snapshot('import Foo\nmacro "m" : term => `(1)', verify=False)
    assert not opted_out["verified"]

    labelled = client.create_snapshot("import Foo\ntheorem labelled : 1 = 1 := rfl", labels={"k": "v"})
    assert labelled["labels"] == {"k": "v"}
    assert client.snapshot(labelled["entry"])["labels"] == {"k": "v"}
    code = "import Foo\ntheorem keyed : 1 = 1 := rfl"
    keyed = client.create_snapshot(code, key="run-1")
    assert client.create_snapshot(code, key="run-1")["entry"] == keyed["entry"]
    with pytest.raises(PisaError) as e:
        client.snapshot(name)
    assert (e.value.kind, e.value.status) == ("bad_request", 400)

    assert client.delete_snapshot(entry) == [name]
    with pytest.raises(NotFoundError):
        client.snapshot(entry)


def test_replaces(url):
    client = Client(url, timeout=5)
    old = client.create_snapshot("import Foo\ntheorem replaced : 1 = 1 := sorry")["entry"]
    proof = client.create_snapshot("import Foo\ntheorem replaced : 1 = 1 := rfl", replaces=old)
    assert proof["replaces"] == {"entry": old, "filled": ["replaced"]}
    assert client.snapshot(proof["entry"])["replaces"] == {"entry": old}

    weakened = client.create_snapshot("import Foo\ntheorem replaced : True := trivial", replaces=old)
    assert "entry" not in weakened and not weakened["result"]["ok"]
    assert weakened["result"]["reasons"] == ["statement: replaced has a different type"]

    skeleton = "import Foo\ndef g : Nat := 1\ntheorem uses_g : g = g := sorry"
    old = client.create_snapshot(skeleton)["entry"]
    fixed = "import Foo\ndef g : Nat := 2\ntheorem uses_g : g = g := rfl"
    job = client.submit_snapshot(fixed, replaces={"entry": old, "allow": "definitions"})
    done = client.wait_job(job)
    assert done["replaces"]["changed_definitions"] == ["g"]
    assert done["replaces"]["affected"] == ["uses_g"]
    with pytest.raises(ValueError):
        client.create_snapshot(fixed, replaces={"entry": old, "allow": "everything"})


def test_snapshot_labels(url):
    client = Client(url, timeout=5)
    code = "import Foo\ntheorem listed : 1 = 1 := rfl"
    first = client.create_snapshot(code, labels={"suite": "labels", "k": "v"})["entry"]
    second = client.create_snapshot(code, labels={"suite": "labels", "k": "w"})["entry"]

    def entries(listing):
        return [s["entry"] for s in listing["entries"]]

    found = client.list_snapshots({"suite": "labels", "k": "v"})
    assert entries(found) == [first] and "next" not in found
    assert found["entries"][0]["labels"] == {"suite": "labels", "k": "v"}
    page = client.list_snapshots({"suite": "labels"}, limit=1)
    assert entries(page) == [second]
    assert entries(client.list_snapshots({"suite": "labels"}, limit=1, after=page["next"])) == [first]

    summary = client.set_snapshot_labels(first, set={"k": "u"})
    assert summary["entry"] == first and summary["labels"] == {"suite": "labels", "k": "u"}
    assert client.list_snapshots({"suite": "labels", "k": "v"})["entries"] == []
    assert entries(client.list_snapshots({"suite": "labels", "k": "u"})) == [first]
    assert client.set_snapshot_labels(first, remove=["k"])["labels"] == {"suite": "labels"}
    with pytest.raises(PisaError) as e:
        client.set_snapshot_labels(first, set={"bad key": "x"})
    assert (e.value.kind, e.value.status) == ("bad_request", 400)
    with pytest.raises(PisaError) as e:
        client.list_snapshots(limit=0)
    assert (e.value.kind, e.value.status) == ("bad_request", 400)

    for entry in [first, second]:
        client.delete_snapshot(entry)
    assert client.list_snapshots({"suite": "labels"})["entries"] == []
