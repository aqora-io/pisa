"""Tests SearchClient against a real pisa-server searching a tiny corpus,
loaded into the Qdrant at PISA_TEST_QDRANT_URL (skipped when unset). Build
the binaries first:

    cargo build -p pisa-server -p pisa-search
"""

import json
import os
import socket
import subprocess
import time
import urllib.request
from pathlib import Path

import pytest

from pisa_client import NotFoundError, PisaError, SearchClient

ROOT = Path(__file__).parents[3]
BIN = Path(os.environ.get("PISA_BIN_DIR", ROOT / "target" / "debug"))

DECLS = [
    {"name": "Matrix", "kind": "def", "signature": "Matrix (m n α : Type) : Type", "type_refs": []},
    {
        "name": "Matrix.trace",
        "kind": "def",
        "signature": "Matrix.trace {n R} [Fintype n] [AddCommMonoid R] (A : Matrix n n R) : R",
        "type_refs": ["Matrix"],
        "docstring": "The trace of a square matrix: the sum of its diagonal entries.",
    },
    {
        "name": "Matrix.trace_one",
        "kind": "theorem",
        "signature": "Matrix.trace_one [Fintype n] [DecidableEq n] : Matrix.trace (1 : Matrix n n R) = Fintype.card n",
        "type_refs": ["Matrix.trace", "Matrix"],
        "docstring": "The trace of the identity matrix is the dimension.",
    },
    {
        "name": "Real.sqrt_mul_self",
        "kind": "theorem",
        "signature": "Real.sqrt_mul_self {a : ℝ} (h : 0 ≤ a) : √(a * a) = a",
        "type_refs": [],
    },
]


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


@pytest.fixture(scope="module")
def url(tmp_path_factory):
    qdrant = os.environ.get("PISA_TEST_QDRANT_URL")
    if not qdrant:
        pytest.skip("PISA_TEST_QDRANT_URL is unset")
    tmp = tmp_path_factory.mktemp("search")
    export = tmp / "export.ndjson"
    export.write_text(
        "".join(
            json.dumps({"record": "decl", "module": "Mathlib.Fixture", "library": "Mathlib",
                        "type_pp_full": d["signature"], **d}) + "\n"
            for d in DECLS
        )
    )
    exe = BIN / "pisa-search"
    env = {**os.environ, "RUST_LOG": "warn"}
    subprocess.run([exe, "ingest", "--export", export, "--project", ROOT / "docker" / "lean-project",
                    "--out", tmp / "corpus"], check=True, env=env, stdout=subprocess.DEVNULL)
    subprocess.run([exe, "build", "--corpus", tmp / "corpus", "--out", tmp / "bundle"],
                   check=True, env=env, stdout=subprocess.DEVNULL)
    # An environment of the test's own, so loading touches nothing else.
    env_id = f"pytest-{os.getpid()}"
    manifest = json.loads((tmp / "bundle" / "manifest.json").read_text())
    manifest["env_id"] = env_id
    (tmp / "bundle" / "manifest.json").write_text(json.dumps(manifest))
    subprocess.run([exe, "load", "--bundle", tmp / "bundle", "--qdrant", qdrant, "--min-coverage", "0"],
                   check=True, env=env, stdout=subprocess.DEVNULL)
    port = free_port()
    server = subprocess.Popen([BIN / "pisa-server", "--http", f"127.0.0.1:{port}",
                               "--database-url", tmp / "pisa.db", "--qdrant", qdrant,
                               "--env-id", env_id, "--no-snapshots"], env=env)
    url = f"http://127.0.0.1:{port}"
    for _ in range(100):
        try:
            with urllib.request.urlopen(f"{url}/health") as r:
                if json.load(r)["search"]["ready"]:
                    break
        except Exception:
            pass
        time.sleep(0.05)
    else:
        pytest.fail("pisa-server did not start searching")
    yield url
    server.terminate()
    for collection in [f"pisa-{env_id}", f"pisa-{env_id}-entries"]:
        req = urllib.request.Request(f"{qdrant}/collections/{collection}", method="DELETE")
        urllib.request.urlopen(req).close()


def test_search(url):
    client = SearchClient(url)
    hits = client.search(nl="trace of the identity matrix")["hits"]
    assert hits[0]["name"] == "Matrix.trace_one"
    assert isinstance(hits[0]["score"], float)

    by_name = client.search(name="Real.sqrt_mul_sefl", k=1)["hits"]
    assert [h["name"] for h in by_name] == ["Real.sqrt_mul_self"]

    results = client.search_batch([{"q": "Matrix.trace"}, {"nl": "sqrt of a product", "kinds": ["theorem"]}])
    assert results[0]["hits"][0]["name"] == "Matrix.trace"
    assert results[1]["hits"][0]["name"] == "Real.sqrt_mul_self"
    with pytest.raises(ValueError, match="invalid query"):
        client.search_batch([{"k": "five"}])


def test_resolve_and_decl(url):
    client = SearchClient(url)
    missing, found = client.resolve(["Real.sqrt_mul_sefl", "Matrix.trace"])
    assert not missing["exists"] and missing["suggestions"][0]["name"] == "Real.sqrt_mul_self"
    assert found["exists"]

    decl = client.decl("Matrix.trace")
    assert decl["deps"] == ["Matrix"] and "Matrix.trace_one" in decl["used_by"]
    with pytest.raises(NotFoundError, match="did you mean Real.sqrt_mul_self"):
        client.decl("Real.sqrt_mul_sefl")


def test_env_pin(url):
    with pytest.raises(NotFoundError, match="no index for env other"):
        SearchClient(url, env_id="other").search("trace")


def test_snapshot_scopes(url):
    client = SearchClient(url)
    assert client.search(nl="trace of the identity matrix", snapshots="none")["hits"]
    # This server doesn't search snapshots, so asking for them is refused.
    for scope in ["all", {"labels": {"run": "1"}}]:
        with pytest.raises(PisaError, match="doesn't search snapshots") as e:
            client.search(nl="trace", snapshots=scope)
        assert e.value.status == 400
    with pytest.raises(ValueError, match="invalid snapshots"):
        client.search(nl="trace", snapshots="some")
