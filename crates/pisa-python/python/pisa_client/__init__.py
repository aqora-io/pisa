"""Python client for the pisa Lean server.

    from pisa_client import Client, LeanError

    client = Client("http://localhost:8000")
    result = client.check("import Mathlib\\ntheorem t : 2 + 2 = 4 := by norm_num")
    assert result["ok"]

    session = client.start_session("theorem t (n : Nat) : n + 0 = n := by sorry")
    step = client.tactic(session["states"][0]["state"], "simp")
    assert step["complete"]
    verdict = client.check_session(session["session"], "theorem t (n : Nat) : n + 0 = n := by simp")
    assert verdict["ok"]

    search = SearchClient("http://localhost:8100")
    hits = search.search(nl="trace of a density matrix is one")["hits"]
"""

from ._native import Client, SearchClient
from .errors import CapacityError, LeanError, LimitError, NotFoundError, PisaError

__all__ = ["Client", "SearchClient", "PisaError", "LeanError", "LimitError", "CapacityError", "NotFoundError"]
