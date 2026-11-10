from typing import Literal, NotRequired, TypedDict

ErrorKind = Literal[
    "timeout", "out_of_memory", "crash", "policy", "lean",
    "capacity", "unavailable", "bad_request", "not_found", "conflict", "internal",
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
    text: str

class Sorry(TypedDict):
    goal: str
    pos: NotRequired[Pos]
    end_pos: NotRequired[Pos]

ProofStatus = Literal["proved", "conditional", "sorry", "rejected"]

class Declaration(TypedDict):
    name: str
    kind: str
    axioms: list[str]
    assumes: list[str]
    status: ProofStatus
    type_hash: str
    value_hash: NotRequired[str]
    type_refs: list[str]
    value_refs: list[str]
    generated: NotRequired[bool]
    parent: NotRequired[str]

Change = Literal[
    "missing", "kind", "type", "value", "derived", "added_assumption", "assumption", "ambiguous"
]

class DeclarationChange(TypedDict):
    """A declaration that new code doesn't keep as it was, or an assumption
    it adds; one of a check's `reasons`. `was` and `now` are signatures from
    the modules' exports, for a changed type or assumption."""
    name: str
    module: NotRequired[str]
    change: Change
    was: NotRequired[str]
    now: NotRequired[str]
    includes: NotRequired[list[str]]

class TacticInfo(TypedDict):
    tactic: str
    goals_before: list[str]
    goals_after: list[str]
    pos: Pos
    end_pos: Pos

class TermInfo(TypedDict):
    term: str
    expected_type: str
    expr: str
    pos: Pos
    end_pos: Pos

class AtPosition(TypedDict):
    pos: Pos
    tactic: NotRequired[TacticInfo]
    term: NotRequired[TermInfo]

class CheckResult(TypedDict):
    ok: bool
    time: float
    error: NotRequired[Error]
    messages: list[Message]
    sorries: list[Sorry]
    reasons: list[str]
    changes: NotRequired[list[DeclarationChange]]
    declarations: list[Declaration]
    tactics: NotRequired[list[TacticInfo]]
    at: NotRequired[list[AtPosition]]

class State(TypedDict):
    id: str
    goals: list[str]
    pos: NotRequired[Pos]
    end_pos: NotRequired[Pos]

class Session(TypedDict):
    id: str
    states: list[State]
    messages: list[Message]

class ApplyTacticResponse(TypedDict):
    state: State
    complete: bool
    messages: list[Message]

class Allow(TypedDict):
    """The definitions whose value a replacement may change."""
    definitions: list[str]

class Replaces(TypedDict):
    """The snapshot a create replaces, and what the replacement may change."""
    snapshot_id: str
    allow: NotRequired[Allow]

class Replacement(TypedDict):
    filled: list[str]
    changed_definitions: NotRequired[list[str]]
    affected: NotRequired[list[str]]

class Snapshot(TypedDict):
    id: str
    version: str
    module: str
    header: str
    imports: list[str]
    labels: dict[str, str]
    replaces: NotRequired[Replaces]
    status: ProofStatus
    verified: bool
    created: int

class CreateSnapshotResponse(TypedDict):
    snapshot: NotRequired[Snapshot]
    check: CheckResult
    replacement: NotRequired[Replacement]
    heads: NotRequired[list[str]]

class Description(TypedDict):
    title: str
    text: str
    latex: NotRequired[str]
    aliases: list[str]
    source: str

class Export(TypedDict):
    name: str
    kind: str
    signature: str
    docstring: NotRequired[str]
    description: NotRequired[Description]

class SnapshotDetail(Snapshot):
    code: str
    check: CheckResult
    exports: NotRequired[list[Export]]

class ListSnapshotsResponse(TypedDict):
    snapshots: list[Snapshot]
    next: NotRequired[str]

class ListDependentsResponse(TypedDict):
    snapshots: list[Snapshot]

class DeleteSnapshotResponse(TypedDict):
    removed: list[str]
    deleted: list[str]

class AuditSummary(TypedDict):
    proved: int
    conditional: int
    sorry: int
    rejected: int
    unverified_modules: int

class AuditAssumption(TypedDict):
    name: str
    signature: NotRequired[str]
    snapshot_id: NotRequired[str]
    verified: bool
    used_by: list[str]
    in_spec: NotRequired[bool]

class AuditUnproved(TypedDict):
    name: str
    snapshot_id: str
    status: ProofStatus
    axioms: list[str]

class AuditFork(TypedDict):
    snapshot_id: str
    replaced_by: list[str]

class AuditTarget(TypedDict):
    name: str
    spec: str
    head: str
    status: ProofStatus
    verified: bool
    accepted: bool

class AuditLink(TypedDict):
    """A link that let definitions change: the snapshot, its `replaces` as
    the snapshot shows it, and what its comparison found: its create's, or the
    union of two if the snapshot between was deleted."""
    snapshot_id: str
    replaces: Replaces
    replacement: Replacement

class AuditComparison(TypedDict):
    """A head compared with a spec snapshot it is reached from, from their
    verified records: why it doesn't keep the spec's statements, or what it
    changed that the links on the way allow."""
    spec: str
    head: str
    reasons: NotRequired[list[str]]
    changes: NotRequired[list[DeclarationChange]]
    replacement: NotRequired[Replacement]

class AuditSnapshotsResponse(TypedDict):
    summary: AuditSummary
    assumptions: list[AuditAssumption]
    not_proved: list[AuditUnproved]
    clean: NotRequired[bool]
    heads: NotRequired[list[str]]
    forks: NotRequired[list[AuditFork]]
    targets: NotRequired[list[AuditTarget]]
    links: NotRequired[list[AuditLink]]
    comparisons: NotRequired[list[AuditComparison]]

class RebuildItem(TypedDict):
    """An item without `snapshot_id` is a support item: it has no labels and
    no `replaces`. Create it as given, then delete its snapshot once the
    items above it are created."""
    level: int
    snapshot_id: NotRequired[str]
    module: str
    new_module: str
    new_header: str
    code: str
    labels: dict[str, str]
    key: str
    replaces: NotRequired[Replaces]
    affected: NotRequired[list[str]]

class RebuildCreated(TypedDict):
    """A support item has no `new_snapshot_id`: the rebuild deleted its
    snapshot."""
    snapshot_id: NotRequired[str]
    module: str
    new_snapshot_id: NotRequired[str]
    new_module: str

class RebuildFailed(TypedDict):
    snapshot_id: NotRequired[str]
    module: str
    check: CheckResult

class RebuildSkipped(TypedDict):
    snapshot_id: NotRequired[str]
    module: str

class RebuildReplaced(TypedDict):
    """A labelled snapshot a rebuild left out, because snapshots without
    every label, `replaced_by`, replace it."""
    snapshot_id: str
    replaced_by: list[str]

class RebuildSnapshotsResponse(TypedDict):
    ok: bool
    changes: list[str]
    changed_definitions: NotRequired[list[str]]
    plan: list[RebuildItem]
    replaced: NotRequired[list[RebuildReplaced]]
    created: NotRequired[list[RebuildCreated]]
    failed: NotRequired[list[RebuildFailed]]
    skipped: NotRequired[list[RebuildSkipped]]
    retired: NotRequired[list[str]]

class VerifyFailure(TypedDict):
    module: str
    reason: str

class VerifySnapshotResponse(TypedDict):
    snapshot: Snapshot
    checked: list[str]
    failure: NotRequired[VerifyFailure]

JobKind = Literal[
    "check", "start_session", "apply_tactic", "check_session",
    "create_snapshot", "verify_snapshot", "rebuild_snapshots",
]
JobStatus = Literal["queued", "running", "done", "cancelled"]

class Job(TypedDict):
    """An async request's job, as a `submit_*` returns it. Keep it to wait for
    the answer later, also from another process; keep it secret too, as anyone
    who has it can read the answer or cancel the job."""
    id: str
    kind: JobKind
    status: JobStatus

class SearchHealth(TypedDict):
    ready: bool
    version: str
    qdrant: bool
    leader: bool
    error: NotRequired[str]

class Health(TypedDict):
    status: Literal["ok", "draining"]
    workers: int | None
    search: SearchHealth | None

class JobDone(TypedDict):
    done: Literal[True]
    answer: CheckResult | Session | ApplyTacticResponse | CreateSnapshotResponse | VerifySnapshotResponse | RebuildSnapshotsResponse

class JobPending(TypedDict):
    done: Literal[False]
    job: Job

JobPoll = JobDone | JobPending

class Client:
    """A client for one pisa server. Safe to share between threads; calls
    release the GIL while waiting."""

    def __init__(self, url: str, *, version: str | None = None, timeout: int = 60) -> None:
        """`version` selects a worker pool label or alias for checks,
        sessions, creates, listings, audits and searches; session, state and
        snapshot ids carry their own. Search answers for the version of its
        index and of its snapshots, and raises NotFoundError for any other.
        `timeout` is Lean's limit per request in seconds."""
    def health(self) -> Health:
        """The server's health. A draining server's 503 is returned, with
        `status` "draining", not raised."""
    def check(
        self,
        code: str,
        *,
        detail: list[Literal["tactics", "terms"]] = [],
        at: list[tuple[int, int]] = [],
    ) -> CheckResult:
        """Check one piece of code. `ok` is the verdict; a check that could not
        complete (timeout, policy, ...) has `error` set instead of raising.
        `detail=["tactics"]` adds `tactics`: each tactic Lean ran, with the
        goals before and after it.

        `at` takes up to 64 `(line, column)` positions in the code, lines from
        1 and columns from 0, and adds `at`: the tactic at each, with its
        goals. With `detail=["terms"]` too, each also has the term there and
        the type Lean expected it to have, which costs several times the
        check. Ask about every position in one check: each check elaborates
        the code again."""
    def check_batch(
        self,
        codes: list[str],
        *,
        detail: list[Literal["tactics", "terms"]] = [],
        at: list[tuple[int, int]] = [],
    ) -> list[CheckResult]:
        """Check several pieces of code in one request; results keep the order
        of `codes`. `detail` and `at` apply to every one."""
    def start_session(self, code: str) -> Session:
        """Run code containing `sorry`; returns one proof state per `sorry`."""
    def apply_tactic(self, state_id: str, tactic: str) -> ApplyTacticResponse:
        """Apply one tactic. Raises LeanError if Lean rejects it and LimitError
        if it times out. Earlier states stay valid."""
    def check_session(self, session_id: str, code: str) -> CheckResult:
        """Re-check a completed proof from scratch; `ok` also requires that it
        proves the session's original statements, with its imports."""
    def create_snapshot(
        self,
        code: str,
        *,
        verify: bool | None = None,
        labels: dict[str, str] | None = None,
        key: str | None = None,
        replaces: Replaces | None = None,
        fork: bool = False,
    ) -> CreateSnapshotResponse:
        """Check code like `check` and, unless Lean reports errors, keep it as
        a module that later code can import, with a new `snapshot` carrying
        `labels` that keeps the module until the snapshot is deleted. Start
        that code with the snapshot's `header` to reuse the Lean process that
        has the module loaded. Code with `sorry` is kept, with that `status`.
        Code that is already kept gets a new snapshot of its module without
        being built again. When nothing is kept, there is no `snapshot`, and
        `check` says why.

        `key` makes the call safe to retry: while a snapshot made with the
        same key exists, creating the same code again returns that snapshot
        as it is, ignoring the call's other arguments, `labels` and `verify`
        included, and other code is a `bad_request` in `check["error"]`. A
        create that keeps nothing doesn't use the key up, and deleting the
        snapshot frees it. A key is 1 to 256 bytes of letters, digits and
        `._:/-`.

        The kernel re-check runs by default only if the code uses macros, its
        own or an import's. `verify=True` always runs it and also re-checks the
        imports, so the snapshot ends `verified` or the check has an error,
        except on a `key` replay, which can be `verified: False` with no error.
        `verify=False` skips it, and the snapshot stays not `verified`.

        `replaces` names the snapshot this code replaces, e.g. the skeleton a
        proof fills in, as `{"snapshot_id": …}`. The code is kept only if it
        still declares everything that snapshot's module declares, with the
        same types and definitions' values, and what those use from a
        snapshot it no longer builds on is unchanged; `replacement` in the
        result lists what it `filled`; otherwise nothing is kept and
        `check["reasons"]` says what changed. `"allow": {"definitions":
        ["f"]}` lets `f` change its value, and lists the theorems that use it
        as `affected`. Each name is a definition of the replaced snapshot or of
        a snapshot the code no longer builds on, else the result's
        `check["error"]` is a `bad_request`; a private one is named without
        its module, `_private.h`. An empty list raises a `PisaError` of kind
        `bad_request`.

        The replaced snapshot must be a head, one that no snapshot replaces.
        Otherwise nothing is built or kept, `check["error"]["kind"]` is
        `conflict`, and `heads` lists the snapshots to replace instead, oldest
        first. Another create can replace it while this one builds, which is
        refused the same way. `fork=True` replaces it anyway, which forks the
        run. A `key` replay comes before this check."""
    def submit_create_snapshot(
        self,
        code: str,
        *,
        verify: bool | None = None,
        labels: dict[str, str] | None = None,
        key: str | None = None,
        replaces: Replaces | None = None,
        fork: bool = False,
    ) -> Job:
        """Like `create_snapshot`, as a job."""
    def get_snapshot(self, id: str) -> SnapshotDetail:
        """A snapshot, with its module's code, check result and exports.
        Raises NotFoundError for an unknown or deleted snapshot."""
    def list_snapshots(
        self,
        labels: dict[str, str] | None = None,
        *,
        limit: int | None = None,
        after: str | None = None,
        heads: bool = False,
    ) -> ListSnapshotsResponse:
        """Snapshots newest first: those with every label in `labels` (exact
        matches, up to 64), or all of them. With `heads`, only those that no
        other snapshot replaces. A page has up to `limit` snapshots (1 to 1000,
        default 100). Keep passing its `next` as `after` while it has one; a
        full page has one, and the page after it may be empty."""
    def update_snapshot(self, id: str, labels: dict[str, str | None]) -> Snapshot:
        """Change a snapshot's labels as a merge patch: a label set to `None`
        is removed, one set to a string takes that value, and the rest stay.
        Returns the snapshot. Labels that break the rules raise a
        `bad_request` PisaError, and NotFoundError means an unknown
        snapshot."""
    def delete_snapshot(self, id: str, *, with_replacers: bool = False) -> DeleteSnapshotResponse:
        """Delete a snapshot; `removed` lists the modules that went with it:
        its module, if no other snapshot or kept module needs it, and so on
        up its imports. The snapshots that replace it replace what it
        replaced instead. With `with_replacers`, they go too, and the ones
        that replace them, and nothing is spliced: a whole branch of a run is
        dropped, and what the snapshot replaced is a head again if nothing
        else replaces it. `deleted` lists every snapshot that went, the asked
        one first."""
    def verify_snapshot(self, id: str) -> VerifySnapshotResponse:
        """Re-check a snapshot's module and everything it imports through the
        kernel, imports first, skipping what already passed. A module the
        kernel rejects is reported in `failure` and stays usable. Raises
        NotFoundError for an unknown or deleted snapshot."""
    def submit_verify_snapshot(self, id: str) -> Job:
        """Like `verify_snapshot`, as a job."""
    def list_dependents(self, id: str, labels: dict[str, str] | None = None) -> ListDependentsResponse:
        """The snapshots of every module that imports `id`'s, directly or
        through others, parents first (by level, then oldest first): those
        with every label in `labels`, or all of them."""
    def audit_snapshots(
        self, labels: dict[str, str] | None = None, *, against: list[str] | None = None
    ) -> AuditSnapshotsResponse:
        """What the snapshots with every label in `labels` rest on: their
        declarations by `status` (`proved`, `conditional` on assumptions,
        `sorry`, `rejected`), each assumption (an axiom declared in snapshot
        code) with what uses it, and what isn't proved.

        With `against`, the ids of a run's spec snapshots, all of one
        version, it audits the run instead: the `heads` it reaches through
        the snapshots that replace them, the `forks` on the way, each spec
        declaration's status on each head (`targets`), the `links` that
        allowed changes, and whether the run is `clean`; the counts are the
        heads'. `labels` and `against` don't combine, and `against` can't be
        empty (ValueError)."""
    def rebuild_snapshots(
        self,
        from_: str,
        to: str,
        labels: dict[str, str],
        *,
        dry_run: bool = False,
        allow_changes: bool = False,
        retire: bool = False,
        verify: bool | None = None,
    ) -> RebuildSnapshotsResponse:
        """Rebuild the dependents of snapshot `from_` with every label in
        `labels` (and what links them to it) on snapshot `to`: the server
        plans the creates that copy their heads, each item's `code` the old
        code with its imports swapped and its `replaces` the snapshot it
        copies, unless the copy's statements would change, and runs them a
        level at a time, saying what it `created`, what `failed` and what it
        `skipped` as built on those. `replaced` lists the labelled snapshots
        left out because a snapshot without the labels replaces them.
        `retire` then deletes the old snapshots whose copies replace them,
        from the top down; `from_` stays. Calling it again replays the
        creates that are done. Unless `to` was created to replace `from_`, a
        `to` that changes what `from_` states gives `ok: False` and no plan,
        unless `allow_changes`.

        `dry_run` only plans: create each item with `code`, `labels`, `key`
        and any `replaces`, a level at a time; the keys make running the plan
        again safe. A run can take minutes: `submit_rebuild_snapshots` runs
        it as a job."""
    def submit_rebuild_snapshots(
        self,
        from_: str,
        to: str,
        labels: dict[str, str],
        *,
        dry_run: bool = False,
        allow_changes: bool = False,
        retire: bool = False,
        verify: bool | None = None,
    ) -> Job:
        """Like `rebuild_snapshots`, as a job. A dry run isn't one: the
        server answers it at once, which raises a `PisaError` here, of kind
        `internal` and status 200; use `rebuild_snapshots` for one."""
    def submit_check(
        self,
        code: str,
        *,
        detail: list[Literal["tactics", "terms"]] = [],
        at: list[tuple[int, int]] = [],
    ) -> Job:
        """Like `check`, as a job the server runs without the request waiting
        for it, which outlives proxies' time limits: see `wait_job`."""
    def submit_start_session(self, code: str) -> Job:
        """Like `start_session`, as a job."""
    def submit_apply_tactic(self, state_id: str, tactic: str) -> Job:
        """Like `apply_tactic`, as a job."""
    def submit_check_session(self, session_id: str, code: str) -> Job:
        """Like `check_session`, as a job."""
    def poll_job(self, job: Job, *, wait: int = 0) -> JobPoll:
        """Where the job stands, after waiting up to `wait` seconds (at most 60)
        for it to finish: `{"done": True, "answer": …}`, or `{"done": False,
        "job": …}` if it hasn't yet. The answer is what the request would have
        returned, and polling raises what it would have raised."""
    def wait_job(
        self, job: Job
    ) -> CheckResult | Session | ApplyTacticResponse | CreateSnapshotResponse | VerifySnapshotResponse | RebuildSnapshotsResponse:
        """Wait for the job to finish, however long it takes, and return what
        its request would have returned (or raise what it would have raised).
        The server keeps the answer for an hour after the job is done."""
    def cancel_job(self, job: Job) -> Job:
        """Withdraw the job if no worker has started it (`status` is then
        `cancelled`); one that is `running` or `done` stays as it is."""
    def search(
        self,
        text: str | None = None,
        *,
        description: str | None = None,
        latex: str | None = None,
        pattern: str | None = None,
        goal: str | None = None,
        name: str | None = None,
        libraries: list[str] | None = None,
        kinds: list[str] | None = None,
        limit: int = 5,
        include_sorried: bool = False,
        snapshots: SnapshotScope | None = None,
    ) -> QueryResult:
        """Search declarations by free `text` or by the fields that apply: a
        description, LaTeX, a Lean pattern, a goal state or a (partial) name.
        `snapshots` also searches snapshot modules: "all", or {"labels":
        {...}} for the modules of the heads with every label, heads being the
        snapshots that nothing replaces. Unknown names and empty results are
        answers, not errors."""
    def search_batch(self, queries: list[Query]) -> list[QueryResult]:
        """Run up to 64 queries in one request; results keep their order."""
    def resolve(self, names: list[str]) -> list[Resolution]:
        """Whether each name exists, with its replacement if deprecated and
        close names if it doesn't exist."""
    def get_declaration(self, name: str, *, module: str | None = None) -> DeclarationDetail:
        """One declaration with its description, dependencies and users: the
        one in snapshot module `module`, as a snapshot hit's `module` names
        it, if it has one, else the bundle's. Raises NotFoundError for an
        unknown name."""

class Query(TypedDict, total=False):
    """One search; fill as many fields as you can, each feeds different lanes."""
    text: str
    description: str
    latex: str
    pattern: str
    goal: str
    name: str
    libraries: list[str]
    kinds: list[str]
    limit: int
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
    title: NotRequired[str]
    summary: NotRequired[str]
    flags: Flags
    score: float
    source_url: NotRequired[str]
    text: str
    snapshot_ids: NotRequired[list[str]]
    status: NotRequired[ProofStatus]
    verified: NotRequired[bool]

class QueryResult(TypedDict):
    hits: list[Hit]
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

class DeclarationDetail(TypedDict):
    name: str
    kind: str
    library: str
    module: str
    signature: str
    type_full: str
    docstring: NotRequired[str]
    description: NotRequired[Description]
    flags: Flags
    source_url: NotRequired[str]
    dependencies: list[str]
    used_by: list[str]
