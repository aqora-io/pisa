from typing import Literal, NotRequired, TypedDict

ErrorKind = Literal[
    "timeout", "out_of_memory", "crash", "policy", "lean",
    "capacity", "unavailable", "bad_request", "not_found", "internal",
]

class Error(TypedDict):
    kind: ErrorKind
    message: str

class Pos(TypedDict):
    line: int
    column: int

class Message(TypedDict):
    severity: Literal["trace", "info", "warning", "error"]
    pos: Pos
    end_pos: NotRequired[Pos]
    data: str

class Sorry(TypedDict):
    goal: str
    pos: NotRequired[Pos]
    end_pos: NotRequired[Pos]

class Declaration(TypedDict):
    name: str
    kind: str
    axioms: list[str]
    type_hash: str
    value_hash: NotRequired[str]

class CheckResult(TypedDict):
    id: NotRequired[str]
    ok: bool
    time: float
    error: NotRequired[Error]
    messages: list[Message]
    sorries: list[Sorry]
    reasons: list[str]
    declarations: list[Declaration]

class StateInfo(TypedDict):
    state: str
    goal: str
    pos: NotRequired[Pos]
    end_pos: NotRequired[Pos]

class Session(TypedDict):
    session: str
    states: list[StateInfo]
    messages: list[Message]

class Step(TypedDict):
    state: str
    goals: list[str]
    complete: bool
    messages: list[Message]

class Client:
    """A client for one pisa server. Safe to share between threads; calls
    release the GIL while waiting."""

    def __init__(self, url: str, *, version: str | None = None, timeout: int = 60) -> None:
        """`version` selects a worker pool label or alias; `timeout` is Lean's
        limit per request in seconds."""
    def health(self) -> None: ...
    def check(self, code: str) -> CheckResult:
        """Check one piece of code. `ok` is the verdict; a check that could not
        complete (timeout, policy, ...) has `error` set instead of raising."""
    def check_batch(self, items: list[tuple[str, str]]) -> list[CheckResult]:
        """Check `(id, code)` pairs in one request; results keep their order."""
    def start_session(self, code: str) -> Session:
        """Run code containing `sorry`; returns one proof state per `sorry`."""
    def tactic(self, state: str, tactic: str) -> Step:
        """Apply one tactic. Raises LeanError if Lean rejects it and LimitError
        if it times out. Earlier states stay valid."""
    def check_session(self, session: str, code: str) -> CheckResult:
        """Re-check a completed proof from scratch; `ok` also requires that it
        proves the session's original statements."""
