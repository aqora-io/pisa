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

class Inherited(TypedDict):
    """A theorem of another snapshot module a declaration gets axioms
    through."""
    name: str
    module: str

class Declaration(TypedDict):
    """Without `detail=["declarations"]`, only `name`, `kind`, `status`, the
    `axioms` outside the worker's allowed ones, and `inherits` and
    `own_status` when it inherits through other snapshots' theorems."""
    name: str
    kind: str
    axioms: list[str]
    assumes: NotRequired[list[str]]
    status: ProofStatus
    type_hash: NotRequired[str]
    value_hash: NotRequired[str]
    type_refs: NotRequired[list[str]]
    value_refs: NotRequired[list[str]]
    generated: NotRequired[bool]
    parent: NotRequired[str]
    negates: NotRequired[str]
    """For a theorem of `¬ X`, the hash of `X`: the `type_hash` of a
    declaration that states `X`."""
    own_axioms: NotRequired[list[str]]
    """The axioms it gets other than through `inherits`."""
    inherits: NotRequired[list[Inherited]]
    own_status: NotRequired[ProofStatus]
    """What its own axioms make it, when it inherits through `inherits`: a
    finished proof on a sorried theorem is `sorry`, its own status
    `proved`."""

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

class RunImport(TypedDict):
    """What `import PisaRun.<name>` resolved to: the module of the run's
    head, imported in its place."""
    name: str
    module: str
    snapshot_id: str
    # Where the name resolves to keep it, as a session's and a document's do:
    # the run's head now, when it is on another module.
    moved_to: NotRequired[str]

class NamedRun(TypedDict):
    """A run by its name: its root, its heads, and, when they are on one
    module, what `import PisaRun.<name>` resolves to."""
    name: str
    root: str
    heads: list[str]
    module: NotRequired[str]
    snapshot_id: NotRequired[str]

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
    runs: NotRequired[list[RunImport]]

class State(TypedDict):
    id: str
    goals: list[str]
    pos: NotRequired[Pos]
    end_pos: NotRequired[Pos]

class Session(TypedDict):
    id: str
    states: list[State]
    messages: list[Message]
    runs: NotRequired[list[RunImport]]
    """What the code's imports by run name resolved to, which the session's
    final check keeps."""

class ApplyTacticResponse(TypedDict):
    state: State
    complete: bool
    messages: list[Message]
    reopened: bool
    """The session's document was opened again first, as after its worker
    went."""
    replayed: int
    """The tactics replayed to reach the state, where no handle on it was
    kept."""

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
    holes: NotRequired[list[str]]
    status: ProofStatus
    verified: bool
    created: int
    run: NotRequired[str]
    """The name of the run it is in, if the run has one."""

class Amends(TypedDict):
    """The spec a create amends, and why (1 to 4096 characters)."""
    snapshot_id: str
    reason: str

class AmendedHoles(TypedDict):
    """An amendment's change to a spec's holes."""
    before: list[str]
    after: list[str]

class Amendment(TypedDict):
    """A spec's amendment of an earlier spec: what changed of it, and what
    the amending spec adds."""
    snapshot_id: str
    amends: str
    reason: str
    changes: list[DeclarationChange]
    added: list[str]
    # When the amending spec names other holes than the amended spec's.
    holes: NotRequired[AmendedHoles]

class Superseded(TypedDict):
    """A spec an amendment supersedes, and the spec now current: the newest
    amendment, followed through the amendments made of it."""
    spec: str
    by: str

class Refutes(TypedDict):
    """The spec target a create refutes."""
    snapshot_id: str
    target: str

class Refutation(TypedDict):
    """A snapshot whose `theorem` proves `¬` the statement of the spec's
    `target`."""
    snapshot_id: str
    spec: str
    target: str
    theorem: str
    # The refuting snapshot was deleted since; the record stays.
    gone: NotRequired[bool]

class CreateSnapshotResponse(TypedDict):
    snapshot: NotRequired[Snapshot]
    check: CheckResult
    replacement: NotRequired[Replacement]
    heads: NotRequired[list[str]]
    # A re-check out of time or memory that kept plain code unverified.
    unverified: NotRequired[VerifyFailure]
    amendment: NotRequired[Amendment]
    refutation: NotRequired[Refutation]
    # A create into a run: the refutations of the run's targets.
    refutations: NotRequired[list[Refutation]]
    runs: NotRequired[list[RunImport]]

class Description(TypedDict):
    title: str
    text: str
    latex: NotRequired[str]
    aliases: list[str]
    source: str

class Export(TypedDict):
    name: str
    kind: str
    # Absent when it didn't print, and then `error` says why.
    signature: NotRequired[str]
    error: NotRequired[str]
    docstring: NotRequired[str]
    description: NotRequired[Description]

class SnapshotDetail(Snapshot):
    code: str
    source: NotRequired[str]
    """The code as its create sent it, where it imported runs by name."""
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
    refuted_by: NotRequired[list[str]]
    # Refuted, and yet proved on the head.
    contradicted: NotRequired[bool]
    # Its status with each theorem it inherits through read on that theorem's
    # run's head, where that differs: it holds once the run is settled.
    linked_status: NotRequired[ProofStatus]
    through: NotRequired[list[AuditThrough]]
    # Why each theorem it inherits through wasn't read through links.
    unlinked: NotRequired[list[AuditUnlinked]]

class AuditUnlinked(TypedDict):
    name: str
    module: NotRequired[str]
    reason: str

class AuditThrough(TypedDict):
    name: str
    snapshot_id: str
    head: str

class AuditSwap(TypedDict):
    snapshot_id: str
    head: str

class AuditUnsettled(TypedDict):
    head: str
    swaps: list[AuditSwap]
    blockers: NotRequired[list[str]]

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

class ReviewDeclaration(TypedDict):
    """A declaration a reviewer reads: `origin` is `spec`, `head` or
    `snapshot`. A hole, and each definition the head adds that a hole's value
    reaches, has its `value`; `holes` names the other holes it mentions."""
    name: str
    kind: str
    origin: Literal["spec", "head", "snapshot"]
    snapshot_id: NotRequired[str]
    signature: NotRequired[str]
    type: NotRequired[str]
    hole: NotRequired[bool]
    value: NotRequired[str]
    # Why there is no value where there should be one.
    unprinted: NotRequired[str]
    holes: NotRequired[list[str]]

class AuditReview(TypedDict):
    spec: str
    head: str
    declarations: list[ReviewDeclaration]
    library: list[str]

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
    review: NotRequired[list[AuditReview]]
    # How many declarations the review list left out, past `review_limit`.
    review_left_out: NotRequired[int]
    unverified: NotRequired[list[str]]
    amendments: NotRequired[list[Amendment]]
    # A superseded spec's run takes no more submissions, and isn't clean.
    superseded: NotRequired[list[Superseded]]
    # A refuted target makes the run not clean.
    refutations: NotRequired[list[Refutation]]
    # The heads whose targets hold only through links, for `settle_run`.
    unsettled: NotRequired[list[AuditUnsettled]]

class PairError(TypedDict):
    kind: Literal["refused", "build_failed", "timeout", "out_of_memory", "cannot_run", "nothing_to_judge"]
    message: str

class LeftOut(TypedDict):
    name: str
    reason: Literal[
        "private",
        "not_a_definition_in_spec",
        "not_a_definition_in_head",
        "private_in_statement",
        "axiom_not_in_head",
    ]

class AcceptPair(TypedDict):
    """Comparator's verdict on a spec snapshot and a head it reaches."""
    spec: str
    head: str
    # Their modules, `PisaSnap.s…`: the code judged, named when a snapshot goes.
    spec_module: NotRequired[str]
    head_module: NotRequired[str]
    # Their modules as the export names them, which comparator judged: a
    # named run's `PisaSpec.<name>` and `PisaRun.<name>`.
    challenge_module: NotRequired[str]
    solution_module: NotRequired[str]
    config: str
    accepted: bool
    error: NotRequired[PairError]
    exit_code: NotRequired[int]
    output: str
    theorems: int
    left_out: NotRequired[list[LeftOut]]

class AcceptResponse(TypedDict):
    """A run's acceptance, stored under `id`: `accepted` when the audit is
    `clean` and comparator accepted every pair, one at least."""
    id: str
    created: int
    against: list[str]
    allow: NotRequired[list[str]]
    paranoid: bool
    accepted: bool
    clean: bool
    heads: list[str]
    audit: AuditSnapshotsResponse
    pairs: list[AcceptPair]
    # Read back: the run's heads are no longer the ones it judged.
    stale: NotRequired[bool]
    # What was left out to fit 6 MiB: "output", then "review".
    truncated: NotRequired[list[str]]

class ListAcceptancesResponse(TypedDict):
    """A spec snapshot's acceptances, newest first."""
    acceptances: list[AcceptResponse]

class Document(TypedDict):
    id: str
    revision: int
    messages: list[Message]
    sorries: list[Sorry]
    runs: NotRequired[list[RunImport]]

class GoalsAt(TypedDict):
    pos: Pos
    line: str
    """The text of the line asked at."""
    in_proof: bool
    """No goals and `in_proof` is a finished proof; no goals outside a proof
    is no proof at all."""
    goals: list[str]

class DocumentGoals(TypedDict):
    revision: int
    at: list[GoalsAt]
    reopened: bool
    """The document was opened again first, within its open's time limit."""

class ProbeResult(TypedDict):
    ok: bool
    closed: bool
    goals: list[str]
    messages: list[Message]
    error: NotRequired[Error]

class DocumentProbes(TypedDict):
    revision: int
    pos: Pos
    results: list[ProbeResult]
    reopened: bool

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

class RebuildNotDeleted(TypedDict):
    snapshot_id: str
    error: Error

class RebuildReplaced(TypedDict):
    """A labelled snapshot a rebuild left out, because snapshots without
    every label, `replaced_by`, replace it."""
    snapshot_id: str
    replaced_by: list[str]

class RebuildSuperseded(TypedDict):
    """A head a rebuild left out, as its run's spec was amended: `by` is the
    spec now current."""
    snapshot_id: str
    by: str

class RebuildSnapshotsResponse(TypedDict):
    ok: bool
    changes: list[str]
    changed_definitions: NotRequired[list[str]]
    plan: list[RebuildItem]
    replaced: NotRequired[list[RebuildReplaced]]
    superseded: NotRequired[list[RebuildSuperseded]]
    created: NotRequired[list[RebuildCreated]]
    failed: NotRequired[list[RebuildFailed]]
    skipped: NotRequired[list[RebuildSkipped]]
    not_deleted: NotRequired[list[RebuildNotDeleted]]
    retired: NotRequired[list[str]]

class ResubmitItem(TypedDict):
    """A head of the amended spec's run: whether it keeps the amending spec's
    statements, and the snapshot that carries it over once created."""
    snapshot_id: str
    module: str
    key: str
    passes: bool
    reasons: NotRequired[list[str]]
    changes: NotRequired[list[DeclarationChange]]
    replacement: NotRequired[Replacement]
    new_snapshot_id: NotRequired[str]
    check: NotRequired[CheckResult]

class ResubmitSnapshotsResponse(TypedDict):
    ok: bool
    items: list[ResubmitItem]

class VerifyFailure(TypedDict):
    module: str
    # `lean` when the kernel rejected the module; `timeout`, `out_of_memory`
    # or `crash` when the re-check didn't finish.
    kind: ErrorKind
    reason: str

class VerifySnapshotResponse(TypedDict):
    snapshot: Snapshot
    checked: list[str]
    failure: NotRequired[VerifyFailure]

JobKind = Literal[
    "check", "start_session", "apply_tactic", "check_session",
    "create_snapshot", "verify_snapshot", "rebuild_snapshots", "accept_run",
    "resubmit_snapshots",
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

class SweepRefused(TypedDict):
    version: str
    reason: str
    since: int

class Health(TypedDict):
    status: Literal["ok", "draining"]
    workers: int | None
    unlimited_workers: list[str] | None
    sweeps_refused: list[SweepRefused] | None
    search: SearchHealth | None

class JobDone(TypedDict):
    done: Literal[True]
    answer: CheckResult | Session | ApplyTacticResponse | CreateSnapshotResponse | VerifySnapshotResponse | RebuildSnapshotsResponse | AcceptResponse | ResubmitSnapshotsResponse

class JobPending(TypedDict):
    done: Literal[False]
    job: Job

JobPoll = JobDone | JobPending

class Client:
    """A client for one pisa server. Safe to share between threads; calls
    release the GIL while waiting."""

    def __init__(
        self,
        url: str,
        *,
        version: str | None = None,
        timeout: int = 60,
        scope: dict[str, str] | None = None,
    ) -> None:
        """`version` selects a worker pool label or alias for checks,
        sessions, creates, listings, audits and searches; session, state and
        snapshot ids carry their own. Search answers for the version of its
        index and of its snapshots, and raises NotFoundError for any other.
        `timeout` is Lean's limit per request in seconds. `scope` sends every
        request in that scope (the `Pisa-Scope` header): what it creates
        carries these labels, and what it names or reaches must, else
        NotFoundError; run names and keys are unique within a scope."""
    def health(self) -> Health:
        """The server's health. A draining server's 503 is returned, with
        `status` "draining", not raised."""
    def check(
        self,
        code: str,
        *,
        detail: list[Literal["tactics", "terms", "declarations"]] = [],
        at: list[tuple[int, int]] = [],
    ) -> CheckResult:
        """Check one piece of code. `ok` is the verdict; a check that could not
        complete (timeout, policy, ...) has `error` set instead of raising.
        `detail=["tactics"]` adds `tactics`: each tactic Lean ran, with the
        goals before and after it. `detail=["declarations"]` gives every field
        of each declaration, which otherwise has its name, kind and status and
        the axioms outside the allowed ones.

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
        detail: list[Literal["tactics", "terms", "declarations"]] = [],
        at: list[tuple[int, int]] = [],
    ) -> list[CheckResult]:
        """Check several pieces of code in one request, up to 128; results
        keep the order of `codes`. `detail` and `at` apply to every one."""
    def start_session(self, code: str) -> Session:
        """Run code containing `sorry`; returns one proof state per `sorry`."""
    def apply_tactic(self, state_id: str, tactic: str) -> ApplyTacticResponse:
        """Apply one tactic. Raises LeanError if Lean rejects it and LimitError
        if it times out. Earlier states stay valid."""
    def check_session(
        self, session_id: str, code: str, *, detail: list[Literal["declarations"]] = []
    ) -> CheckResult:
        """Re-check a completed proof from scratch; `ok` also requires that it
        proves the session's original statements, with its imports.
        `detail=["declarations"]` as for `check`."""
    def create_snapshot(
        self,
        code: str,
        *,
        verify: bool | None = None,
        labels: dict[str, str] | None = None,
        key: str | None = None,
        replaces: Replaces | None = None,
        fork: bool = False,
        holes: list[str] | None = None,
        amends: Amends | None = None,
        refutes: Refutes | None = None,
        detail: list[Literal["declarations"]] = [],
        name: str | None = None,
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

        The kernel re-check runs on the code by default. `verify=True` also
        re-checks the imports, so the snapshot ends `verified` or the check has
        an error, except on a `key` replay, which can be `verified: False` with
        no error. `verify=False` skips it, but for module-system code, and the
        snapshot stays not `verified`.

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
        run. A `key` replay comes before this check.

        `holes` makes this a spec snapshot that leaves those definitions for
        the snapshots that replace it to change: each a definition of its
        module or of a snapshot it builds on, else `check["error"]` is a
        `bad_request`. Snapshots that replace it inherit them and may change
        them without `allow`, and an audit against it allows them to change.
        Not with `replaces` (a `PisaError` of kind `bad_request`).

        `amends` makes this a spec that amends the spec `snapshot_id`, for
        `reason`: `amendment` says what changed and what it adds, and
        `resubmit_snapshots` carries the amended spec's run over. `refutes`
        keeps the code only if it builds on the spec and proves `¬` the
        statement of its theorem `target`, as `theorem r : ¬ type_of% @t`
        states it: `refutation` names the theorem, and an audit against the
        spec isn't clean. Each is always re-checked, and neither goes with
        `replaces`.

        `name` names the run the snapshot starts, unique in its version, so
        that code can `import PisaRun.<name>`, which imports the run's head
        (the answer's `runs` says what each such import resolved to). Not
        with `replaces`. A name another run has is a `conflict` in
        `check["error"]`, unless the create amends that run's spec, which
        takes the name over."""
    def submit_create_snapshot(
        self,
        code: str,
        *,
        verify: bool | None = None,
        labels: dict[str, str] | None = None,
        key: str | None = None,
        replaces: Replaces | None = None,
        fork: bool = False,
        holes: list[str] | None = None,
        amends: Amends | None = None,
        refutes: Refutes | None = None,
        detail: list[Literal["declarations"]] = [],
        name: str | None = None,
    ) -> Job:
        """Like `create_snapshot`, as a job."""
    def get_snapshot(
        self, id: str, *, detail: list[Literal["declarations"]] = []
    ) -> SnapshotDetail:
        """A snapshot, with its module's code, check result and exports.
        `detail=["declarations"]` as for `check`. Raises NotFoundError for an
        unknown or deleted snapshot."""
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
    def verify_snapshot(self, id: str, *, again: bool = False) -> VerifySnapshotResponse:
        """Re-check a snapshot's module and everything it imports through the
        kernel, imports first, skipping what already passed unless `again`,
        which describes their records again, with the client's `timeout` for
        each module. A module the kernel rejects, or that runs out of time,
        is reported in `failure` and stays usable. Raises NotFoundError for
        an unknown or deleted snapshot."""
    def submit_verify_snapshot(self, id: str) -> Job:
        """Like `verify_snapshot`, as a job."""
    def list_dependents(self, id: str, labels: dict[str, str] | None = None) -> ListDependentsResponse:
        """The snapshots of every module that imports `id`'s, directly or
        through others, parents first (by level, then oldest first): those
        with every label in `labels`, or all of them."""
    def audit_snapshots(
        self,
        labels: dict[str, str] | None = None,
        *,
        against: list[str] | None = None,
        allow: list[str] | None = None,
        review: bool = False,
        review_limit: int | None = None,
    ) -> AuditSnapshotsResponse:
        """What the snapshots with every label in `labels` rest on: their
        declarations by `status` (`proved`, `conditional` on assumptions,
        `sorry`, `rejected`), each assumption (an axiom declared in snapshot
        code) with what uses it, and what isn't proved.

        With `against`, the ids of a run's spec snapshots, all of one
        version, it audits the run instead: the `heads` it reaches through
        the snapshots that replace them, the `forks` on the way, each spec
        declaration's status on each head (`targets`), the `links` that
        allowed changes, and whether the run is `clean`, with no definition
        changed but the spec's `holes` and those `allow` names (the links' own
        allows don't count);
        the counts are the heads'. With `review`, also what a reviewer reads
        on each head: the declarations the spec's statements use, each hole
        with its value: at most `review_limit` declarations over all the
        entries, 1 to 10,000 (1,000 unless given), and `review_left_out`
        counts the rest. `labels` and `against` don't combine, `against`
        can't be empty, and `allow` and `review` need `against` (ValueError)."""
    def export_run(self, against: list[str], *, allow: list[str] | None = None) -> bytes:
        """The run that spec snapshots `against` start, as a Lake project for
        `lake comparator` in `pisa-run/` of a tar.gz."""
    def accept_run(
        self,
        against: list[str],
        *,
        allow: list[str] | None = None,
        paranoid: bool = True,
        timeout: int | None = None,
        build_timeout: int | None = None,
    ) -> AcceptResponse:
        """Audit the run and run `lake comparator` on the pairs the audit
        compared, and store the result. Paranoid runs the external kernels
        Lean bundles too. `timeout` is each pair's limit and `build_timeout`
        the first pair's more for building the run, the server's (600, 1800)
        unless given: not the client's `timeout`."""
    def submit_accept_run(
        self,
        against: list[str],
        *,
        allow: list[str] | None = None,
        paranoid: bool = True,
        timeout: int | None = None,
        build_timeout: int | None = None,
    ) -> Job:
        """Like `accept_run`, as a job."""
    def get_acceptance(self, id: str) -> AcceptResponse:
        """An acceptance `accept_run` stored, by its id."""
    def list_acceptances(self, spec: str) -> ListAcceptancesResponse:
        """The acceptances that judged spec snapshot `spec`, newest first, at
        most 100; each says `stale` once the run's heads have moved on, its
        spec is amended, or a target is refuted."""
    def get_run(self, name: str) -> NamedRun:
        """The run named `name`: its root, its heads, and, when they are on
        one module, what `import PisaRun.<name>` resolves to now."""
    def name_run(self, name: str, snapshot_id: str) -> NamedRun:
        """Names `name` the run snapshot `snapshot_id` is in, as a create's
        `name` names the run it starts: a name the run had goes, and one
        another run with snapshots has is a conflict."""
    def open_document(self, code: str) -> Document:
        """Open code on Lean Beam, which keeps it elaborated for goals and
        probes until `close_document` or until it expires."""
    def edit_document(self, id: str, revision: int, code: str) -> Document:
        """Replace a document's code, which is at `revision`."""
    def close_document(self, id: str) -> None:
        """Close a document and delete its code."""
    def document_goals(
        self, id: str, at: list[dict], *, revision: int | None = None, mode: str = "before"
    ) -> DocumentGoals:
        """The goals at each position of `at`, `{"line": n}` (its first
        tactic) or `{"line": n, "column": m}`, lines from 1; `mode` is
        `before` or `after` the tactic there. With `revision`, a document at
        another revision raises a conflict."""
    def probe_document(
        self, id: str, at: dict, candidates: list[str], *, revision: int | None = None
    ) -> DocumentProbes:
        """Run each candidate tactic at `at`, `{"sorry": i}` or a position as
        for `document_goals`, on the document as it is: nothing stays."""
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
    def settle_run(
        self,
        against: list[str],
        *,
        allow: list[str] | None = None,
        dry_run: bool = False,
        verify: bool | None = None,
    ) -> RebuildSnapshotsResponse:
        """Settle the run that spec snapshots `against` start: rebuild each
        head whose targets hold only through links onto the heads they are
        read through, in one plan, as the audit's `unsettled` lists them. A
        head with blockers refuses the plan, with them in `changes`."""
    def submit_settle_run(
        self,
        against: list[str],
        *,
        allow: list[str] | None = None,
        dry_run: bool = False,
        verify: bool | None = None,
    ) -> Job:
        """Like `settle_run`, as a job; a dry run isn't one."""
    def resubmit_snapshots(
        self,
        from_: str,
        to: str,
        *,
        dry_run: bool = False,
        labels: dict[str, str] | None = None,
        verify: bool | None = None,
    ) -> ResubmitSnapshotsResponse:
        """Carry the run of spec `from_` over to `to`, the spec that amends
        it: each head of the run that keeps `to`'s statements is created
        again, replacing `to`, with its labels or `labels`. A dry run only
        compares. Sending it again replays the creates that are done."""
    def submit_resubmit_snapshots(
        self,
        from_: str,
        to: str,
        *,
        dry_run: bool = False,
        labels: dict[str, str] | None = None,
        verify: bool | None = None,
    ) -> Job:
        """Like `resubmit_snapshots`, as a job; a dry run isn't one."""
    def submit_check(
        self,
        code: str,
        *,
        detail: list[Literal["tactics", "terms", "declarations"]] = [],
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
        include_sorried: bool | None = None,
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
    include_sorried: NotRequired[bool]
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
    imports: NotRequired[str]

class Resolution(TypedDict):
    name: str
    exists: bool
    deprecated_to: NotRequired[str]
    imports: NotRequired[str]
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
