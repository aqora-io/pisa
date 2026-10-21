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
"""

from ._native import Client
from .errors import CapacityError, LeanError, LimitError, NotFoundError, PisaError

__all__ = ["Client", "PisaError", "LeanError", "LimitError", "CapacityError", "NotFoundError"]
