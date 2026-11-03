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

class Tactic(TypedDict):
    tactic: str
    goals_before: list[str]
    goals_after: list[str]
    pos: Pos
    end_pos: Pos

class CheckResult(TypedDict):
    id: NotRequired[str]
    ok: bool
    time: float
    error: NotRequired[Error]
    messages: list[Message]
    sorries: list[Sorry]
    reasons: list[str]
    declarations: list[Declaration]
    tactics: NotRequired[list[Tactic]]

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

class Snapshot(TypedDict):
    entry: NotRequired[str]
    snapshot: NotRequired[str]
    header: NotRequired[str]
    version: str
    dirty: bool
    verified: bool
    labels: NotRequired[dict[str, str]]
    result: CheckResult

class SnapshotInfo(TypedDict):
    entry: str
    snapshot: str
    imports: list[str]
    header: str
    code: str
    labels: NotRequired[dict[str, str]]
    dirty: bool
    verified: bool
    result: CheckResult
    created: int

class SnapshotFailure(TypedDict):
    snapshot: str
    reason: str

class SnapshotVerified(TypedDict):
    entry: str
    snapshot: str
    verified: bool
    dirty: bool
    checked: list[str]
    failure: NotRequired[SnapshotFailure]

class Client:
    """A client for one pisa server. Safe to share between threads; calls
    release the GIL while waiting."""

    def __init__(self, url: str, *, version: str | None = None, timeout: int = 60) -> None:
        """`version` selects a worker pool label or alias; `timeout` is Lean's
        limit per request in seconds."""
    def health(self) -> None: ...
    def check(self, code: str, *, detail: list[Literal["tactics"]] = []) -> CheckResult:
        """Check one piece of code. `ok` is the verdict; a check that could not
        complete (timeout, policy, ...) has `error` set instead of raising.
        `detail=["tactics"]` adds `tactics`: each tactic Lean ran, with the
        goals before and after it."""
    def check_batch(
        self, items: list[tuple[str, str]], *, detail: list[Literal["tactics"]] = []
    ) -> list[CheckResult]:
        """Check `(id, code)` pairs in one request; results keep their order.
        `detail` applies to every item."""
    def start_session(self, code: str) -> Session:
        """Run code containing `sorry`; returns one proof state per `sorry`."""
    def tactic(self, state: str, tactic: str) -> Step:
        """Apply one tactic. Raises LeanError if Lean rejects it and LimitError
        if it times out. Earlier states stay valid."""
    def check_session(self, session: str, code: str) -> CheckResult:
        """Re-check a completed proof from scratch; `ok` also requires that it
        proves the session's original statements, with its imports."""
    def create_snapshot(
        self, code: str, *, verify: bool | None = None, labels: dict[str, str] | None = None
    ) -> Snapshot:
        """Check code like `check` and, unless Lean reports errors, keep it as
        a module (`snapshot`) that later code can import, with a new `entry`
        carrying `labels` that keeps the module until the entry is deleted.
        Start that code with the returned `header` to reuse the Lean process
        that has it loaded. Code with `sorry` is kept but `dirty`. Code that is
        already kept gets a new entry on its module without being built again.

        The kernel re-check runs by default only if the code uses macros, its
        own or an import's. `verify=True` always runs it and also re-checks the
        imports, so the snapshot ends `verified` or the result has an error.
        `verify=False` skips it, and the snapshot stays not `verified`."""
    def verify_snapshot(self, entry: str) -> SnapshotVerified:
        """Re-check an entry's module and everything it imports through the
        kernel, imports first, skipping what already passed. A module the kernel
        rejects is reported in `failure` and stays usable. Raises NotFoundError
        for an unknown or deleted entry."""
    def snapshot(self, entry: str) -> SnapshotInfo:
        """An entry with its labels, and its module's code, imports and check
        result. Raises NotFoundError for an unknown or deleted entry."""
    def delete_snapshot(self, entry: str) -> list[str]:
        """Delete an entry; returns the modules that went with it: its module,
        if no other entry or kept module needs it, and so on up its imports."""

class Query(TypedDict, total=False):
    """One search; fill as many fields as you can, each feeds different lanes."""
    q: str
    nl: str
    latex: str
    pattern: str
    goal: str
    name: str
    libraries: list[str]
    kinds: list[str]
    k: int
    include_sorried: bool

class Flags(TypedDict):
    uses_sorry: NotRequired[bool]
    informal: NotRequired[bool]
    deprecated_to: NotRequired[str]
    additive_sibling: NotRequired[str]
    imports: NotRequired[str]

class Hit(TypedDict):
    name: str
    kind: str
    library: str
    module: str
    signature: str
    informal_name: NotRequired[str]
    summary: NotRequired[str]
    flags: Flags
    score: float
    lanes: list[str]
    source_url: NotRequired[str]
    text: str

class QueryResult(TypedDict):
    hits: list[Hit]
    verification: NotRequired[str]
    warnings: NotRequired[list[str]]

class Suggestion(TypedDict):
    name: str
    kind: str
    signature: str

class Resolution(TypedDict):
    name: str
    exists: bool
    deprecated_to: NotRequired[str]
    suggestions: NotRequired[list[Suggestion]]

class Informal(TypedDict):
    informal_name: str
    statement_nl: str
    statement_latex: NotRequired[str]
    aliases: NotRequired[list[str]]
    source: str

class DeclDetail(TypedDict):
    name: str
    kind: str
    library: str
    module: str
    signature: str
    type_full: str
    docstring: NotRequired[str]
    informal: NotRequired[Informal]
    flags: Flags
    source_url: NotRequired[str]
    deps: list[str]
    used_by: list[str]

class SearchClient:
    """A client for one declaration search server (pisa-search). Safe to
    share between threads; calls release the GIL while waiting."""

    def __init__(self, url: str, *, env_id: str | None = None) -> None:
        """`env_id` pins the index's environment (a worker pool version); a
        server with another raises NotFoundError."""
    def health(self) -> None: ...
    def search(
        self,
        q: str | None = None,
        *,
        nl: str | None = None,
        latex: str | None = None,
        pattern: str | None = None,
        goal: str | None = None,
        name: str | None = None,
        libraries: list[str] = [],
        kinds: list[str] = [],
        k: int = 5,
        include_sorried: bool = False,
    ) -> QueryResult:
        """Search by free text `q` or by the fields that apply: a description,
        LaTeX, a Lean pattern, a goal state or a (partial) name."""
    def search_batch(self, queries: list[Query]) -> list[QueryResult]:
        """Run up to 64 queries in one request; results keep their order."""
    def resolve(self, names: list[str]) -> list[Resolution]:
        """Whether each name exists, with its replacement if deprecated and
        close names if it doesn't exist."""
    def decl(self, name: str) -> DeclDetail:
        """One declaration with its description, dependencies and users.
        Raises NotFoundError for an unknown name."""
