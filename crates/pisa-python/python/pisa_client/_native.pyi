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
    replaces: NotRequired[Replacement]
    result: CheckResult

class Replaced(TypedDict):
    entry: str
    allow: NotRequired[Literal["definitions"]]

class Replacement(TypedDict):
    entry: str
    allow: NotRequired[Literal["definitions"]]
    filled: list[str]
    changed_definitions: NotRequired[list[str]]
    affected: NotRequired[list[str]]

class SnapshotInfo(TypedDict):
    entry: str
    snapshot: str
    imports: list[str]
    header: str
    code: str
    labels: NotRequired[dict[str, str]]
    replaces: NotRequired[Replaced]
    dirty: bool
    verified: bool
    result: CheckResult
    created: int
    decls: NotRequired[list[ModuleDecl]]

class ModuleDecl(TypedDict):
    name: str
    kind: str
    signature: str
    docstring: NotRequired[str]
    informal: NotRequired[Informal]

class SnapshotSummary(TypedDict):
    entry: str
    snapshot: str
    imports: list[str]
    header: str
    labels: dict[str, str]
    replaces: NotRequired[Replaced]
    dirty: bool
    verified: bool
    created: int

class SnapshotList(TypedDict):
    entries: list[SnapshotSummary]

class SnapshotDependents(TypedDict):
    entries: list[SnapshotSummary]

class RebuildItem(TypedDict):
    level: int
    entry: NotRequired[str]
    snapshot: str
    new_snapshot: str
    new_header: str
    code: str
    labels: dict[str, str]
    key: str
    affected: NotRequired[list[str]]
    affected_unknown: NotRequired[bool]

class RebuildPlan(TypedDict):
    ok: bool
    changes: list[str]
    changed_definitions: NotRequired[list[str]]
    plan: list[RebuildItem]
    next: NotRequired[str]

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

class JobStatus(TypedDict):
    """An async request's job, as a `submit_*` returns it. Keep it to wait for
    the answer later, also from another process; keep it secret too, as anyone
    who has it can read the answer or cancel the job."""
    job: str
    kind: Literal["check", "start_session", "tactic", "check_session", "create_snapshot", "verify_snapshot"]
    status: Literal["queued", "running", "done", "cancelled"]

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
        self,
        code: str,
        *,
        verify: bool | None = None,
        labels: dict[str, str] | None = None,
        key: str | None = None,
        replaces: str | Replaced | None = None,
    ) -> Snapshot:
        """Check code like `check` and, unless Lean reports errors, keep it as
        a module (`snapshot`) that later code can import, with a new `entry`
        carrying `labels` that keeps the module until the entry is deleted.
        Start that code with the returned `header` to reuse the Lean process
        that has it loaded. Code with `sorry` is kept but `dirty`. Code that is
        already kept gets a new entry on its module without being built again.

        `key` makes the call safe to retry: while an entry made with the same
        key exists, creating the same code again returns that entry as it is,
        ignoring the call's other arguments, `labels` and `verify` included,
        and other code is a `bad_request` in `result["error"]`. A create that
        keeps nothing doesn't use the key up, and deleting the entry frees it.
        A key is 1 to 256 bytes of letters, digits and `._:/-`.

        The kernel re-check runs by default only if the code uses macros, its
        own or an import's. `verify=True` always runs it and also re-checks the
        imports, so the snapshot ends `verified` or the result has an error,
        except on a `key` replay, which can be `verified: False` with no error.
        `verify=False` skips it, and the snapshot stays not `verified`.

        `replaces` names the entry this code replaces, e.g. the skeleton a
        proof fills in. The code is kept only if it still declares everything
        that entry's module declares, with the same types and definitions'
        values, and still builds on the snapshots that module builds on (it
        may import more); `replaces` in the result lists what it `filled`;
        otherwise nothing is kept and `result["reasons"]` says what changed.
        `{"entry": …, "allow": "definitions"}` lets definitions change their
        value, and lists the theorems that use them as `affected`."""
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
    def list_snapshots(
        self,
        labels: dict[str, str] | None = None,
        *,
        limit: int | None = None,
        after: str | None = None,
    ) -> SnapshotList:
        """Entries newest first, as summaries without code or check results:
        those with every label in `labels` (exact matches, up to 64), or all of
        them. A page has up to `limit` entries (1 to 1000, default 100). Keep
        passing its `next` as `after` while it has one; a full page has one,
        and the page after it may be empty."""
    def dependents(self, entry: str, labels: dict[str, str] | None = None) -> SnapshotDependents:
        """The entries on every module that imports `entry`'s, directly or
        through others, parents first (by level, then oldest first): those
        with every label in `labels`, or all of them."""
    def rebuild_plan(
        self,
        from_entry: str,
        to_entry: str,
        labels: dict[str, str],
        *,
        allow_changes: bool = False,
    ) -> RebuildPlan:
        """Plan the creates that rebuild `from_entry`'s dependents with every
        label in `labels` (and what links them to it) on `to_entry`: each
        item's `code` is the old code with its imports swapped. It only plans:
        create each item with `code`, `labels` and `key`, a level at a time;
        the keys make running the plan again safe. Unless `to_entry` was
        created to replace `from_entry`, a `to_entry` that changes what
        `from_entry` states gives `ok: False` and no plan, unless
        `allow_changes`."""
    def set_snapshot_labels(
        self,
        entry: str,
        *,
        set: dict[str, str] | None = None,
        remove: list[str] | None = None,
    ) -> SnapshotSummary:
        """Remove the labels `remove` names from an entry, then set those in
        `set`; returns the entry's summary. Labels that break the rules raise a
        `bad_request` PisaError, and NotFoundError means an unknown entry."""
    def submit_check(self, code: str, *, detail: list[Literal["tactics"]] = []) -> JobStatus:
        """Like `check`, as a job the server runs without the request waiting
        for it, which outlives proxies' time limits: see `wait_job`."""
    def submit_session(self, code: str) -> JobStatus:
        """Like `start_session`, as a job."""
    def submit_tactic(self, state: str, tactic: str) -> JobStatus:
        """Like `tactic`, as a job."""
    def submit_session_check(self, session: str, code: str) -> JobStatus:
        """Like `check_session`, as a job."""
    def submit_snapshot(
        self,
        code: str,
        *,
        verify: bool | None = None,
        labels: dict[str, str] | None = None,
        key: str | None = None,
        replaces: str | Replaced | None = None,
    ) -> JobStatus:
        """Like `create_snapshot`, as a job."""
    def submit_verify(self, entry: str) -> JobStatus:
        """Like `verify_snapshot`, as a job."""
    def poll_job(
        self, job: JobStatus, *, wait: int = 0
    ) -> CheckResult | Session | Step | Snapshot | SnapshotVerified | None:
        """The job's answer, after waiting up to `wait` seconds (at most 60) for
        it to finish, or None if it hasn't yet. The answer is what the request
        would have returned, and raises what it would have raised."""
    def wait_job(self, job: JobStatus) -> CheckResult | Session | Step | Snapshot | SnapshotVerified:
        """Wait for the job to finish, however long it takes, and return what
        its request would have returned (or raise what it would have raised).
        The server keeps the answer for an hour after the job is done."""
    def cancel_job(self, job: JobStatus) -> JobStatus:
        """Withdraw the job if no worker has started it (`status` is then
        `cancelled`); one that is `running` or `done` stays as it is."""

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
    snapshots: SnapshotScope

class SnapshotLabels(TypedDict):
    labels: dict[str, str]

SnapshotScope = Literal["none", "all"] | SnapshotLabels

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
    source_url: NotRequired[str]
    text: str
    snapshot: NotRequired[SnapshotHit]

class SnapshotHit(TypedDict):
    entries: list[str]
    dirty: bool
    verified: bool

class QueryResult(TypedDict):
    hits: list[Hit]
    verification: NotRequired[str]
    warnings: NotRequired[list[str]]
    dense_pending: NotRequired[bool]

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
    """A client for declaration search on one pisa-server. Safe to share
    between threads; calls release the GIL while waiting."""

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
        snapshots: SnapshotScope | None = None,
    ) -> QueryResult:
        """Search by free text `q` or by the fields that apply: a description,
        LaTeX, a Lean pattern, a goal state or a (partial) name. `snapshots`
        also searches snapshot modules: "all", or {"labels": {...}} for those
        of the entries with every label."""
    def search_batch(self, queries: list[Query]) -> list[QueryResult]:
        """Run up to 64 queries in one request; results keep their order."""
    def resolve(self, names: list[str]) -> list[Resolution]:
        """Whether each name exists, with its replacement if deprecated and
        close names if it doesn't exist."""
    def decl(self, name: str) -> DeclDetail:
        """One declaration with its description, dependencies and users.
        Raises NotFoundError for an unknown name."""
