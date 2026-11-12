"""The type stubs describe both clients: `AsyncClient`'s section of
`_native.pyi` is made from `Client`'s, each call returning a future of what
`Client`'s returns. `PISA_UPDATE_STUBS=1 pytest tests/test_stubs.py` writes it
again after `Client`'s changes.
"""

import ast
import inspect
import os
import re
from pathlib import Path

import pisa_client
from pisa_client import _native

STUBS = Path(pisa_client.__file__).parent / "_native.pyi"
BEGIN = "# Made from Client by tests/test_stubs.py: PISA_UPDATE_STUBS=1 pytest tests/test_stubs.py\n"
END = "# End of AsyncClient.\n"
HEADER = '''class AsyncClient:
    """A client for one pisa server whose calls return asyncio futures,
    which start at once; cancelling one drops its request, though a job a
    worker took runs on. Takes what `Client` takes."""

'''


def async_section(stubs: str) -> str:
    """`AsyncClient`'s section, made from `Client`'s in `stubs`."""
    tree = ast.parse(stubs)
    client = next(n for n in tree.body if isinstance(n, ast.ClassDef) and n.name == "Client")
    methods = []
    for node in client.body:
        if not isinstance(node, ast.FunctionDef):
            continue
        source = "    " + ast.get_source_segment(stubs, node)
        if node.name != "__init__":
            # The signature's return annotation, the first `) -> T:` line end.
            source = re.sub(r"\) -> (.+?):\n", r") -> Future[\1]:\n", source, count=1)
        methods.append(source + "\n")
    return BEGIN + HEADER + "".join(methods) + END


def test_the_async_stubs_are_made_from_the_sync_ones():
    stubs = STUBS.read_text()
    made = async_section(stubs)
    start, end = stubs.find(BEGIN), stubs.find(END)
    current = stubs[start : end + len(END)] if start >= 0 and end >= 0 else None
    if os.environ.get("PISA_UPDATE_STUBS"):
        if current is None:
            # After Client's class, before whatever follows it.
            at = stubs.index("\nclass ", stubs.index("\nclass Client:") + 1) + 1
            stubs = stubs[:at] + made + "\n" + stubs[at:]
        else:
            stubs = stubs.replace(current, made)
        STUBS.write_text(stubs)
        return
    assert current == made, "the AsyncClient stubs are stale: PISA_UPDATE_STUBS=1 pytest tests/test_stubs.py"


def public(cls) -> dict[str, list[str]]:
    """`cls`'s public methods, with their parameters."""
    methods = {}
    for name, f in inspect.getmembers(cls, callable):
        if name.startswith("_"):
            continue
        params = [p for p in inspect.signature(f).parameters if p not in ("self", "py")]
        methods[name] = params
    return methods


def stub_methods(name: str) -> dict[str, list[str]]:
    tree = ast.parse(STUBS.read_text())
    cls = next(n for n in tree.body if isinstance(n, ast.ClassDef) and n.name == name)
    methods = {}
    for node in cls.body:
        if isinstance(node, ast.FunctionDef) and not node.name.startswith("_"):
            args = node.args
            params = [a.arg for a in args.posonlyargs + args.args + args.kwonlyargs]
            methods[node.name] = [p for p in params if p != "self"]
    return methods


def test_both_clients_have_every_call_the_stubs_describe():
    sync, asynchronous = public(_native.Client), public(_native.AsyncClient)
    assert sync == asynchronous
    assert stub_methods("Client") == sync
    assert stub_methods("AsyncClient") == sync
    assert not isinstance(_native.AsyncClient("http://h"), _native.Client)
