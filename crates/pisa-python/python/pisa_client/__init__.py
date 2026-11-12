"""Python client for the pisa Lean server.

    from pisa_client import Client, LeanError

    client = Client("http://localhost:8000")
    result = client.check("import Mathlib\\ntheorem t : 2 + 2 = 4 := by norm_num")
    assert result["ok"]

    session = client.start_session("theorem t (n : Nat) : n + 0 = n := by sorry")
    step = client.apply_tactic(session["states"][0]["id"], "simp")
    assert step["complete"]
    verdict = client.check_session(session["id"], "theorem t (n : Nat) : n + 0 = n := by simp")
    assert verdict["ok"]

    # Keep accepted code as a module, and build on it without re-checking it.
    base = client.create_snapshot("import Mathlib\\ndef double (n : Nat) := 2 * n")["snapshot"]
    assert client.check(base["header"] + "\\nexample : double 2 = 4 := rfl")["ok"]

    hits = client.search(description="trace of a density matrix is one")["hits"]

`Client`'s calls block, with the GIL released, and stop at Ctrl-C.
`AsyncClient` takes the same arguments, and its calls return asyncio futures
instead, which start at once:

    from pisa_client import AsyncClient

    async def main():
        client = AsyncClient("http://localhost:8000")
        results = await asyncio.gather(*(client.check(code) for code in codes))
"""

from ._native import AsyncClient, Client
from .errors import CapacityError, LeanError, LimitError, NotFoundError, PisaError

__all__ = ["AsyncClient", "Client", "PisaError", "LeanError", "LimitError", "CapacityError", "NotFoundError"]
