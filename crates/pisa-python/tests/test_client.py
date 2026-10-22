"""Tests the Python client against a real router and worker that run
fake-repl in place of Lean. Build the binaries first:

    cargo build -p pisa-router -p pisa-worker -p pisa-tests
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
def url():
    http, workers = free_port(), free_port()
    env = {**os.environ, "RUST_LOG": "warn"}
    router = subprocess.Popen(
        [BIN / "pisa-router", "--http", f"127.0.0.1:{http}", "--workers", f"127.0.0.1:{workers}",
         "--max-wait-secs", "1"],
        env=env,
    )
    worker = subprocess.Popen(
        [BIN / "pisa-worker", "--router", f"127.0.0.1:{workers}", "--version", "v",
         "--slots", "2", "--repl", BIN / "fake-repl", "--lean", BIN / "fake-lean"],
        env=env,
    )
    url = f"http://127.0.0.1:{http}"
    client = Client(url)
    for _ in range(100):
        try:
            client.health()
            if "pisa_workers 1" in metrics(url):
                break
        except Exception:
            pass
        time.sleep(0.05)
    else:
        pytest.fail("cluster did not start")
    yield url
    worker.terminate()
    router.terminate()


def metrics(url):
    import urllib.request

    with urllib.request.urlopen(url + "/metrics") as r:
        return r.read().decode()


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
    name = base["snapshot"]
    assert base["header"] == f"import Foo\nimport {name}"
    assert client.check(base["header"] + "\nneeds base")["ok"]

    rejected = client.create_snapshot("theorem t : 1 = 1 := error_here")
    assert "snapshot" not in rejected and not rejected["result"]["ok"]

    info = client.snapshot(name)
    assert info["header"] == base["header"] and info["imports"] == []
    assert client.delete_snapshot(name) == [name]
    with pytest.raises(NotFoundError):
        client.snapshot(name)
