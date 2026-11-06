# API review Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rename and reshape pisa's HTTP API and its Rust, Python,
TypeScript and React clients so that every concept has one name and one
shape. Remove what no longer makes sense.

**Architecture:** The wire types live in `pisa-protocol` (`api.rs`,
`search.rs`). The router (`pisa-router`) and search (`pisa-search`) serve
them. Workers (`pisa-worker`) build most answers and send them back as
`wire::JobResult`. OpenAPI is derived from the Rust types with utoipa, and
the TypeScript types are generated from it.

Each task changes one slice end to end: protocol, server, worker, the three
clients, React, the playground, and their tests. So every task ends green.

**Tech Stack:** Rust (axum, utoipa, serde), PyO3 and maturin, TypeScript
(openapi-typescript, vitest), React with TanStack Query, jj.

**Spec:** `docs/superpowers/specs/2026-11-06-api-review-design.md`

## Global Constraints

- **No backwards compatibility** on the HTTP API or the clients. Stored data
  is reset once; nothing reads records from before this change.
- **Naming rules 1–8 of the spec apply everywhere.**
  - A resource's own identifier is `id`; a reference is `<resource>_id`.
  - No abbreviations; one word per concept.
  - Types are `<Operation>Request` and `<Operation>Response`, or a
    resource's name.
  - A client method is named after its operationId; async forms are
    `submit_<method>`; a `JobKind` is that operationId in snake_case.
- **Ops names stay:** `PISA_SEARCH_ENV_ID`, `--env-id`, the bundle
  manifest's `env_id`, the Qdrant collections `pisa-<env_id>` and
  `pisa-<env_id>-entries`.
- **Stored description formats stay.** `search::Informal`'s serde field
  names are stored keys, in the informal store, bundles (format 4), Qdrant
  payloads and `described.record`. Rename only the public type the API
  returns.
- **Internal names stay** (`entries` tables, `entry` in `pisa-db`,
  `pisa-worker` and `wire.rs`), except where a task says otherwise.
- **Doc comments are the API reference.** OpenAPI descriptions come from
  them. Rewrite them, in the surrounding style, wherever a field or type
  changes.
- **Regenerate the specs after every task that changes a wire type:**
  `PISA_UPDATE_OPENAPI=1 cargo test -p pisa-server --test openapi`, then
  `pnpm -C web generate`.
- **`_native.pyi` is written by hand.** Keep it equal to the wire, field for
  field.
- **`pisa_client` (Rust) re-exports every wire type a method takes or
  returns**, so callers can name them.
- **README.md and the package READMEs are rewritten once, in Task 7.** Until
  then, change only code docs.
- **Commits:**
  - One jj change per task: `jj describe -m "<msg>" && jj new`.
  - The message style is `refactor(api): …` or `feat(…)`.
  - Never squash, rebase or push.

## Review Focus

1. **A snapshot made through an alias.** Create with `version` set to an
   alias. The id carries the resolved label, and get, update, verify,
   dependents and delete all find it. (Task 4.)
2. **A body id from another version.** A `replaces.snapshot_id`, or rebuild
   `from`/`to`, of another version than the request's is a 400
   `bad_request` naming both versions. It is not a 404 from the wrong pool.
   (Task 4.)
3. **A malformed or old-style id in a path.** A bare 32-hex id, a module
   name, `a~b~c` or `v~..` in `/snapshots/{id}` is a 400 in the API's error
   shape. The message says an id looks like `<version>~<32 hex>`. (Task 4.)
4. **Merge-patch edge cases.** `{"labels": {"absent": null}}` and
   `{"labels": {}}` succeed and change nothing. A patch that would break the
   label rules is a 400 and changes nothing. (Task 3.)
5. **Searching by the snapshots' version.** A search pinned to
   `PISA_SEARCH_SNAPSHOT_VERSION`, where it differs from `env_id`, is
   accepted. Its hits' `snapshot_ids` then `GET` fine. (Task 5.)

---

### Task 1: Check results, batches and messages

**Files:**
- Modify: `crates/pisa-protocol/src/api.rs`: `Message`, `CheckResult`,
  `BatchRequest`/`BatchItem`/`BatchResponse`, the module doc table.
- Modify: `crates/pisa-protocol/src/wire.rs`, where `CheckOutcome` turns
  `repl::Message` into `api::Message`.
- Modify: `crates/pisa-router/src/http.rs` (`check_batch`, its
  `#[utoipa::path]`, the `ApiDoc` schemas).
- Modify: `crates/pisa-client/src/lib.rs`, `crates/pisa-python/src/lib.rs`,
  `crates/pisa-python/python/pisa_client/_native.pyi`.
- Modify: `web/packages/client/src/{client,types}.ts`, and in
  `web/apps/playground/src/` the files `components/ui.tsx` and
  `editor/diagnostics.ts` (`data` → `text`).
- Test: `crates/pisa-protocol/src/api.rs` (unit tests),
  `crates/pisa-tests/tests/{stage1,client}.rs`,
  `crates/pisa-python/tests/test_client.py`,
  `web/packages/client/test/client.test.ts`,
  `web/packages/client/test/integration/server.test.ts`.

**Interfaces:**
- **Produces, wire:**

  ```rust
  pub struct Message { pub severity: Severity, pub pos: Pos, pub end_pos: Option<Pos>, pub text: String }
  pub struct CheckBatchRequest { pub items: Vec<CheckBatchItem>, pub version: Option<String>, pub timeout: u64, pub detail: Vec<Detail>, pub at: Vec<Pos> }
  pub struct CheckBatchItem { pub code: String }
  pub struct CheckBatchResponse { pub results: Vec<CheckResult> } // in request order
  // CheckResult loses `id`; everything else is unchanged.
  ```
- **Produces, clients:**
  - Rust: `check_batch<I, C>(&self, codes: I) -> Result<Vec<CheckResult>,
    Error>` where `I: IntoIterator<Item = C>, C: AsRef<str>`, and
    `check_batch_with(codes, &CheckOptions)`.
  - Rust: `check_with_detail`, `submit_check_with_detail` and
    `check_batch_with_detail` are removed.
  - Python: `Client.check_batch(codes: list[str], *, detail=None, at=None)
    -> list[CheckResult]`.
  - TypeScript: `checkBatch(codes: string[], options?: CheckOptions):
    Promise<CheckResult[]>`.
  - React: `checkOptions` puts `at` in its query key, after `detail`.

- [ ] **Step 1: Update the tests to the new shapes**
  - **React key:** `web/packages/react/test/options.test.ts` asserts that
    two `checkOptions` differing only in `at` have different keys.
  - In `api.rs`, `results_are_snake_case_and_omit_absent_fields` expects
    `"text": "Nat : Type"` where it had `"data"`.
  - In `stage1.rs`, `batch_results_keep_order` posts `{"items": [{"code":
    A}, {"code": B}]}`. It asserts `results.len() == 2`, that the results
    are in order, and that no result has an `"id"` key.
  - `client.rs`, `test_client.py` and the TS tests call `check_batch` and
    `checkBatch` with plain code strings, and read `message.text`.
- [ ] **Step 2: Run them and see them fail**

  Run: `cargo test -p pisa-protocol && cargo test -p pisa-tests --test
  stage1 batch`.

  Expected: compile errors or assertion failures on `text` and `items`.
- [ ] **Step 3: Rename the types and fields above**
  - Update the handler and the clients.
  - Drop the `result.id = Some(item.id)` echo in `http.rs`.
  - Rename `repl::Message.data` to `text` only at the conversion
    (`From<repl::Message> for Message`).
- [ ] **Step 4: Regenerate the specs** (Global Constraints), then run:

  `cargo test --workspace`;
  `cd web && pnpm typecheck && pnpm test`;
  `cargo build -p pisa-server -p pisa-worker -p pisa-tests && pnpm -C web test:integration`;
  the Python tests:
  - `uv venv $SCRATCH/venv`
  - `. $SCRATCH/venv/bin/activate`
  - `uv pip install maturin pytest`
  - `(cd crates/pisa-python && maturin develop && pytest tests)`

  Here `$SCRATCH` is the session scratchpad.

  Expected: all pass.
- [ ] **Step 5: Commit:** `jj describe -m "refactor(api): batch items
  without ids, and a message's text" && jj new`.

### Task 2: Sessions, states and jobs

**Files:**
- Modify: `crates/pisa-protocol/src/api.rs`:
  - `StartRequest`/`StartResponse`/`StateInfo`/`TacticRequest`/
    `TacticResponse`/`SessionCheckRequest`;
  - `JobStatus`/`JobState`/`JobKind`/`JobOutcome`, `Accepted`.
- Modify: `crates/pisa-protocol/src/wire.rs` (`JobResult::SessionStarted`,
  `TacticApplied`).
- Modify: `crates/pisa-worker/src/sessions.rs`, which builds `Session` and
  `State`.
- Modify: `crates/pisa-router/src/{sessions,jobs,dispatch,http}.rs`
  (operationIds, `Location`, `kind_of`).
- Modify: `crates/pisa-server/src/lib.rs` (`health` docs).
- Modify: the clients: `crates/pisa-client/src/lib.rs`,
  `crates/pisa-python/src/lib.rs`, `_native.pyi`, `errors.py`,
  `web/packages/client/src/{client,types,errors,index}.ts`.
- Modify: `web/packages/react/src/hooks/proof.ts`, and in
  `web/apps/playground/src/` the file `tabs/ProveTab.tsx`.
- Test: `crates/pisa-tests/tests/{stage3,jobs,replicas,client}.rs`,
  `crates/pisa-python/tests/test_client.py`, `web/packages/client/test/**`,
  `web/packages/react/test/hooks.test.tsx` and `fake.tsx`.

**Interfaces:**
- **Produces, wire:**

  ```rust
  pub struct StartSessionRequest { pub code: String, pub version: Option<String>, pub timeout: u64 }
  pub struct Session { pub id: String, pub states: Vec<State>, pub messages: Vec<Message> }
  pub struct State { pub id: String, pub goals: Vec<String>, pub pos: Option<Pos>, pub end_pos: Option<Pos> }
  pub struct ApplyTacticRequest { pub tactic: String, pub timeout: u64 }
  pub struct ApplyTacticResponse { pub state: State, pub complete: bool, pub messages: Vec<Message> }
  pub struct CheckSessionRequest { pub code: String, pub timeout: u64 }
  pub struct Job { pub id: String, pub kind: JobKind, pub status: JobStatus }
  pub enum JobStatus { Queued, Running, Done, Cancelled }
  pub enum JobKind { Check, StartSession, ApplyTactic, CheckSession, CreateSnapshot, VerifySnapshot, RebuildSnapshots }
  ```
  - A session's first states have `goals: vec![sorry.goal]` and the sorry's
    `pos`/`end_pos`. A state after a tactic has neither position.
  - The routes are `/sessions/{id}/check`, `/states/{id}/tactic` and
    `/jobs/{id}`, with operationId `applyTactic` (was `tactic`) and
    `Location: /jobs/{id}`.
  - `jobs.kind` stores the new snake_case names.
- **Produces, Rust client:**
  - `start_session(code) -> Session`, `submit_start_session`;
    `apply_tactic(state_id, tactic) -> ApplyTacticResponse`,
    `submit_apply_tactic`.
  - `check_session(session_id, code) -> CheckResult`,
    `submit_check_session`.
  - `health() -> Health`; `cancel_job(&Job<T>) -> api::Job`;
    `JobPoll::Pending(api::Job)`.
  - The `Session`/`Step` aliases go, and `Health` is re-exported.
- **Produces, Python client:**
  - The same method names; `health() -> Health`.
  - `poll_job(job, *, wait=0)` returns `{"done": True, "answer": …}` or
    `{"done": False, "job": Job}`.
  - TypedDicts `Session`, `State`, `ApplyTacticResponse`, `Job`, `Health`.
    `Step` and `StateInfo` go, and `Tactic` → `TacticInfo`.
- **Produces, TypeScript client:**
  - `startSession`/`submitStartSession`, `applyTactic(state: StateId, …)`/
    `submitApplyTactic`, `checkSession(session: SessionId, …)`/
    `submitCheckSession`.
  - `JobPoll<T> = {done: true; answer: T} | {done: false; job: Job}`.
  - The client's `Job<T>` extends the generated `Job`, imported as
    `JobSchema`.
  - `type StateId = string` (was `StateKey`).
- **Produces, React:** `useProofSession` keeps its public API and reads
  `session.id`, `response.state.id` and `response.state.goals`.

- [ ] **Step 1: Update the tests to the new shapes**
  - `stage3.rs` helpers read `["id"]` from a start, `["states"][i]["id"]`,
    `["state"]["id"]` and `["state"]["goals"]`.
    `session_is_a_tree_of_states_with_a_final_check` asserts that a first
    state has `goals == [goal]` and has `pos`, and that a tactic's state has
    no `pos`.
  - `jobs.rs` reads `["id"]` and expects the kinds `start_session`,
    `apply_tactic` and `check_session`. Its `Location` assertion becomes
    `/jobs/{id}`.
  - In the client tests, Python asserts `poll_job(job)["done"] is False`
    and `["job"]["status"] in ("queued", "running")` before the answer.
- [ ] **Step 2: Run them and see them fail**

  Run: `cargo test -p pisa-tests --test stage3 --test jobs`.

  Expected: failures on the `id`/`state` fields.
- [ ] **Step 3: Rename and reshape**
  - Rename and reshape the types above in the protocol, the worker, the
    router, the three clients, React and the playground's Prove tab.
  - Keep `JobOutcome`'s variants untagged, pointing at the new types.
- [ ] **Step 4: Regenerate the specs and run every suite** (Task 1, Step 4).

  Expected: all pass.
- [ ] **Step 5: Commit:** `jj describe -m "refactor(api): sessions, states
  and jobs answer with their id" && jj new`.

### Task 3: Snapshot shapes

**Files:**
- Modify: `crates/pisa-protocol/src/api.rs`, with every snapshot type
  replaced as below.
- Modify: `crates/pisa-protocol/src/search.rs`: add `Description` and
  `impl From<Informal> for Description`.
- Modify: `crates/pisa-protocol/src/wire.rs`:
  - `SnapshotCreateJob.replaces: Option<api::Replaces>`;
  - `SnapshotLabelsJob { entry, labels: BTreeMap<String, Option<String>> }`
    replaces `set`/`remove`;
  - the `JobResult` variants carry the new types.
- Modify: `crates/pisa-worker/src/snapshots.rs`: responses, the merge-patch
  apply, and the create answer for the `verify: true` re-check failure.
- Modify: `crates/pisa-router/src/{snapshots,rebuild}.rs` (handlers, rebuild
  runner, operationId `updateSnapshot`).
- Modify: `crates/pisa-db/src/catalog.rs`, where the labels edit takes a
  merge patch.
- Modify: the clients: `crates/pisa-client/src/lib.rs`,
  `crates/pisa-python/src/lib.rs`, `_native.pyi`,
  `web/packages/client/src/{client,types}.ts`.
- Modify: React `web/packages/react/src/{hooks/snapshots.ts,options.ts,index.ts}`.
- Modify: in `web/apps/playground/src/`, the file `tabs/SnapshotsTab.tsx`.
- Test: in `crates/pisa-tests/tests/`, the files `stage4.rs`, `listing.rs`,
  `replaces.rs`, `rebuild.rs`, `statuses.rs`, `search_records.rs` and
  `client.rs`; plus `test_client.py`, `web/packages/client/test/**` and
  `web/packages/react/test/**`.

**Interfaces:**
- **Produces, wire.** Ids are still bare 32 hex here; Task 4 versions them.

  ```rust
  pub struct Snapshot { pub id: String, pub version: String, pub module: String, pub header: String, pub imports: Vec<String>,
      pub labels: BTreeMap<String, String>, pub replaces: Option<Replaces>, pub status: ProofStatus, pub verified: bool, pub created: u64 }
  pub struct SnapshotDetail { #[serde(flatten)] pub snapshot: Snapshot, pub code: String, pub check: CheckResult, pub exports: Vec<Export> }
  pub struct Export { pub name: String, pub kind: String, pub signature: String, pub docstring: Option<String>, pub description: Option<Description> }
  pub struct Description { pub title: String, pub text: String, pub latex: Option<String>, pub aliases: Vec<String>, pub source: String }
  pub struct Replaces { pub snapshot_id: String, pub allow: Option<Allow> }
  pub struct Replacement { pub filled: Vec<String>, pub changed_definitions: Vec<String>, pub affected: Vec<String> }
  pub struct CreateSnapshotRequest { pub code: String, pub version: Option<String>, pub timeout: u64, pub verify: Option<bool>,
      pub labels: BTreeMap<String, String>, pub key: Option<String>, pub replaces: Option<Replaces> }
  pub struct CreateSnapshotResponse { pub snapshot: Option<Snapshot>, pub check: CheckResult, pub replacement: Option<Replacement> }
  pub struct ListSnapshotsResponse { pub snapshots: Vec<Snapshot>, pub next: Option<String> }
  pub struct ListDependentsResponse { pub snapshots: Vec<Snapshot> }
  pub struct UpdateSnapshotRequest { pub labels: BTreeMap<String, Option<String>> } // null removes, a string sets
  pub struct DeleteSnapshotResponse { pub removed: Vec<String> }
  pub struct VerifySnapshotResponse { pub snapshot: Snapshot, pub checked: Vec<String>, pub failure: Option<VerifyFailure> }
  pub struct VerifyFailure { pub module: String, pub reason: String }
  pub struct AuditSnapshotsResponse { pub summary: AuditSummary, pub assumptions: Vec<AuditAssumption>, pub not_proved: Vec<AuditUnproved> }
  // AuditAssumption.entry and AuditUnproved.entry → snapshot_id
  pub struct RebuildSnapshotsRequest { pub from: String, pub to: String, pub labels: BTreeMap<String, String>, pub dry_run: bool,
      pub allow_changes: bool, pub retire: bool, pub verify: Option<bool>, pub timeout: u64, pub version: Option<String> } // version goes in Task 4
  pub struct RebuildSnapshotsResponse { /* RebuildResponse's fields; retired: Vec<String> of snapshot ids */ }
  pub struct RebuildItem { pub level: usize, pub snapshot_id: Option<String>, pub module: String, pub new_module: String, pub new_header: String,
      pub code: String, pub labels: BTreeMap<String, String>, pub key: String, pub replaces: Option<Replaces>, pub affected: Vec<String>,
      pub affected_unknown: bool } // affected_unknown goes in Task 6
  pub struct RebuildCreated { pub snapshot_id: Option<String>, pub module: String, pub new_snapshot_id: String, pub new_module: String }
  pub struct RebuildFailed { pub snapshot_id: Option<String>, pub module: String, pub check: CheckResult }
  pub struct RebuildSkipped { pub snapshot_id: Option<String>, pub module: String }
  ```
  - **Removed:** `dirty` everywhere in the API; `Replaces`'s untagged
    string form; `Replaced`, `SnapshotSummary`, `SnapshotInfo`, `ModuleDecl`,
    `LabelsRequest`, `Deleted`, `SnapshotVerified`, `SnapshotFailure`,
    `SnapshotList`, `SnapshotDependents`, `SnapshotAudit`.
  - `CreateSnapshotResponse.snapshot` is `Some` exactly when a snapshot
    was created or replayed by `key`.
  - On `verify: true` with code already kept that fails the re-check,
    `snapshot` is `None` and `check.error` says why.
- **Produces, Rust client:**
  - `create_snapshot(code) -> CreateSnapshotResponse`, and
    `create_snapshot_with(code, &CreateSnapshotOptions)` (was
    `SnapshotOptions`), with `submit_create_snapshot` and
    `submit_create_snapshot_with`.
  - `get_snapshot(id) -> SnapshotDetail`; `list_snapshots(labels:
    &BTreeMap<String, String>, limit: Option<usize>, after: Option<&str>)`.
  - `update_snapshot(id, labels: &BTreeMap<String, Option<String>>) ->
    Snapshot`; `delete_snapshot(id) -> DeleteSnapshotResponse`.
  - `verify_snapshot(id) -> VerifySnapshotResponse` and
    `submit_verify_snapshot`; `list_dependents(id, labels)`;
    `audit_snapshots(labels)`.
  - `rebuild_snapshots(from, to, labels, &RebuildOptions { dry_run,
    allow_changes, retire, verify }) -> RebuildSnapshotsResponse` and
    `submit_rebuild_snapshots`.
  - `rebuild_plan`, `rebuild`, `dependents`, `audit`, `snapshot`,
    `set_snapshot_labels` and `create_snapshot_with_verify` are removed.
- **Produces, Python client:**
  - The same names as keyword-argument methods; `update_snapshot(id,
    labels: dict[str, str | None])`.
  - `rebuild_snapshots(from_, to, labels, *, dry_run=False,
    allow_changes=False, retire=False, verify=None)`.
  - `replaces` takes a `Replaces` dict.
- **Produces, TypeScript client:**
  - The camelCase versions of the same names; `updateSnapshot(id,
    {labels})`; `RebuildOptions` gains `dryRun` and `timeout`.
  - `type SnapshotId = string` (was the unused `EntryId`), used for every
    snapshot id parameter. `ModuleName` and `Cursor` are used where those
    appear.
- **Produces, React:** `useUpdateSnapshot` (was `useSetSnapshotLabels`). The
  rest keep their names and use the new methods.

- [ ] **Step 1: Update the tests to the new shapes**
  - Helpers (`entry(r)`, `module(r)`, `info`, `patch`) read
    `["snapshot"]["id"]`, `["snapshot"]["module"]` and `["check"]`.
  - Listings read `["snapshots"]`.
  - `listing.rs` `patch_sets_and_removes_labels` sends `{"labels":
    {"stage": "done", "draft": null}}`.
  - Add `listing.rs` `a_patch_of_absent_or_no_labels_changes_nothing` for
    Review Focus 4:
    - `{"labels": {"absent": null}}` and `{"labels": {}}` both 200, with
      the labels unchanged;
    - a patch adding a key of 129 bytes is 400, with the labels unchanged.
  - `stage4.rs`:
    - sorry code has `snapshot.status == "sorry"` and no `dirty` key;
    - `GET /snapshots/{id}` has `exports` (was `decls`);
    - the `verify: true` test on already-kept code that fails the re-check
      asserts that `snapshot` is absent and `check.error` is set.
  - `replaces.rs` sends `"replaces": {"snapshot_id": …}`. It asserts
    `replacement.filled` on the create, and `snapshot.replaces.snapshot_id`
    on get.
  - `rebuild.rs` reads `snapshot_id`, `module`, `new_module` and
    `new_snapshot_id`.
  - `statuses.rs` reads `snapshot_id`.
- [ ] **Step 2: Run them and see them fail**

  Run: `cargo test -p pisa-tests --test stage4 --test listing --test
  replaces`.

  Expected: failures on the new field names.
- [ ] **Step 3: Implement the types and their producers**
  - The worker builds `Snapshot` once, from the entry row, the module meta
    and the computed status, and every answer reuses it.
  - The catalog's labels edit applies the patch:
    - `None` removes the key and `Some(v)` sets it;
    - then `check_labels` runs on the result, which is stored only if it
      passes.
  - `Export.description` is `described.informal.map(Description::from)`.
    The mapping is `informal_name` → `title`, `statement_nl` → `text` and
    `statement_latex` → `latex`.
- [ ] **Step 4: Regenerate the specs and run every suite** (Task 1, Step 4).

  Expected: all pass.
- [ ] **Step 5: Commit:** `jj describe -m "refactor(api): a snapshot is a
  Snapshot, with an id and a module" && jj new`.

### Task 4: Snapshot ids carry their version

**Files:**
- Modify: `crates/pisa-protocol/src/api.rs`:
  - add `SnapshotId`, and share `Key`'s part check with it;
  - remove `VersionQuery`, and `RebuildSnapshotsRequest.version`.
- Modify: `crates/pisa-protocol/src/labels.rs`: `entry_id_error` →
  `snapshot_id_error`.
- Modify: `crates/pisa-router/src/{snapshots,rebuild}.rs` (route by id,
  check body ids).
- Modify: `crates/pisa-worker/src/snapshots.rs` (parse, check the version,
  format ids in answers).
- Modify: `crates/pisa-search/src/engine.rs` (compose hit ids with the
  snapshot version).
- Modify: the clients (drop `?version=` from id calls). React keys a single
  snapshot by its id.
- Test: `crates/pisa-tests/tests/{stage4,listing,client,rebuild,replaces}.rs`,
  `crates/pisa-protocol/src/api.rs`, and the clients' tests.

**Interfaces:**
- **Produces:**

  ```rust
  /// A snapshot id, `<version>~<32 lowercase hex>`; the hex is the entry's row id.
  pub struct SnapshotId { pub version: String, pub entry: String }
  impl SnapshotId { pub fn parse(s: &str) -> Option<SnapshotId> } // same part rules as Key, entry must be 32 lowercase hex
  impl Display for SnapshotId // "{version}~{entry}"
  ```
  - **Router.** `/snapshots/{id}` and its `/verify` and `/dependents`
    parse `SnapshotId`, and an unparseable one is 400 `snapshot_id_error`.
    The router routes to `id.version` and sends the full id string in the
    wire job.
  - **Router, body ids.** On a create, `replaces.snapshot_id` must parse
    and have the request's resolved version. On a rebuild, `from` and `to`
    must parse and share one version, which the rebuild then uses.
    Otherwise it is a 400 naming both versions.
  - **Worker.** It parses the id again, requires `version ==
    self.version` (400 `snapshot {id} belongs to version {v}`), and uses
    `entry` with the catalog. Every snapshot id it answers with is
    `SnapshotId { version: self.version, entry }.to_string()`.
  - **Internal ids stay bare hex:** idempotency keys, cursors, the
    `entries.replaces` column.
  - **Clients:** `get_snapshot`, `update_snapshot`, `delete_snapshot`,
    `verify_snapshot` and `list_dependents` no longer send `version`, and
    `rebuild_snapshots` doesn't either. `list_snapshots` and
    `audit_snapshots` still do.

- [ ] **Step 1: Write the failing tests**
  - **`api.rs` `snapshot_ids_round_trip_and_reject_old_and_odd_ids`:**
    - it round-trips `v4.35.0-1a2b3c4d~` + 32 hex;
    - `parse` is `None` for a bare 32-hex id, `PisaSnap.s…`, `a~b~c`,
      `v~..`, `v~` + 31 hex, and `v~` + 32 uppercase hex.
  - **`stage4.rs` `snapshot_ids_carry_their_version`:**
    - a create's `snapshot.id` starts with the version + `~`;
    - get, patch, verify, dependents and delete with no `?version=` find
      it;
    - a bare-hex path is 400 with kind `bad_request`, and its message
      contains `<version>~`. (Review Focus 3.)
  - **`stage4.rs` `an_alias_resolves_into_the_id`:** with the router's
    `alias("stable=<v>")` (`common::router_config` has the field), a create
    with `version: "stable"` gets an id starting with `<v>~`, and get finds
    it. (Review Focus 1.)
  - **`replaces.rs` `a_replaced_id_of_another_version_is_a_bad_request`,
    and `rebuild.rs` `from_and_to_must_share_a_version`:** each is a 400
    whose message names both versions. (Review Focus 2.)
  - Remove `client.rs` `verify_snapshot_uses_the_clients_version`. Replace
    it with `ids_route_whatever_the_clients_version`: a client with another
    `with_version` can get and verify a snapshot by its id.
- [ ] **Step 2: Run them and see them fail**

  Run: `cargo test -p pisa-protocol snapshot_ids && cargo test -p
  pisa-tests --test stage4 ids`.

  Expected: FAIL.
- [ ] **Step 3: Implement `SnapshotId`, the routing, the worker check and
  the id formatting**
  - Factor the `[A-Za-z0-9._-]`, non-empty, no-leading-`.` part rule out of
    `Key::parse` into one private function, and use it in both parsers.
- [ ] **Step 4: Regenerate the specs and run every suite** (Task 1, Step 4).

  Expected: all pass.
- [ ] **Step 5: Commit:** `jj describe -m "feat(api): snapshot ids carry
  their version, as session ids do" && jj new`.

### Task 5: Search

**Files:**
- Modify: `crates/pisa-protocol/src/search.rs`: `SearchRequest`,
  `SearchResponse`, `Query`, `QueryResult`, `Hit` (`SnapshotHit` goes),
  `ResolveRequest`, `ResolveResponse`, `GetDeclarationResponse`,
  `DeclarationDetail`, `SearchHealth`.
- Modify: `crates/pisa-search/src/{server,engine,points,indexer}.rs`
  (routes, version check, payload `status`, no `verify`).
- Modify: `crates/pisa-db/src/catalog.rs`: `SnapshotDecl.status:
  ProofStatus`, and `SnapshotDecls.dirty` goes.
- Modify: `crates/pisa-worker/src/snapshots.rs`, which sets each
  `SnapshotDecl.status` from the module check's declaration of the same name
  when writing `modules.decls`.
- Modify: `crates/pisa-router/src/{http,lib}.rs`: make `Versions` public, as
  `pisa_router::Versions` with `resolve(&self, Option<&str>) ->
  Option<String>`.
- Modify: `crates/pisa-server/src/lib.rs`: pass the router's `Versions` to
  search; the no-search fallback covers `/search`, `/resolve` and
  `/declarations/{*name}`.
- Modify: the clients:
  - Rust and Python: merge `SearchClient` into `Client`;
  - TypeScript: drop `envId`;
  - React: `useDeclaration` and `declarationOptions`, with `version` in
    search keys;
  - the playground: `tabs/SearchTab.tsx`, `components/HealthBadge.tsx`.
- Test: `crates/pisa-search/tests/search.rs`,
  `crates/pisa-server/tests/server.rs`,
  `crates/pisa-tests/tests/search_records.rs`, `test_search.py`, and the TS
  `search.test.ts`, `options.test.ts` and `client.test.ts`.

**Interfaces:**
- **Produces, wire:**

  ```rust
  pub struct SearchRequest { pub queries: Vec<Query>, pub version: Option<String> }
  pub struct SearchResponse { pub version: String, pub results: Vec<QueryResult> }
  pub struct Query { pub text: Option<String>, pub description: Option<String>, pub latex: Option<String>, pub pattern: Option<String>,
      pub goal: Option<String>, pub name: Option<String>, pub libraries: Vec<String>, pub kinds: Vec<String>,
      pub limit: usize /* default 5, at most 50 */, pub include_sorried: bool, pub snapshots: SnapshotScope }
  pub struct QueryResult { pub hits: Vec<Hit>, pub warnings: Vec<String>, pub dense_pending: bool }
  pub struct Hit { pub name, pub kind, pub library, pub module, pub signature, pub title: Option<String>, pub summary: Option<String>,
      pub flags: Flags, pub score: f64, pub source_url: Option<String>, pub text: String,
      pub snapshot_ids: Vec<String> /* skipped when empty */, pub status: Option<ProofStatus>, pub verified: Option<bool> }
  pub struct ResolveRequest { pub names: Vec<String>, pub version: Option<String> }
  pub struct ResolveResponse { pub version: String, pub results: Vec<Resolution> }
  pub struct GetDeclarationResponse { pub version: String, pub declaration: DeclarationDetail }
  pub struct DeclarationDetail { /* DeclDetail's fields, with */ pub description: Option<Description>, pub dependencies: Vec<String>, pub used_by: Vec<String> }
  pub struct SearchHealth { pub ready: bool, pub version: String, pub qdrant: bool, pub leader: bool, pub error: Option<String> }
  ```
  - **Routes:** `POST /search`, `POST /resolve`, and `GET
    /declarations/{name}?module=&version=`, with operationId
    `getDeclaration`.
  - **Version check.** Resolve through `Versions`. Accept the index's
    `env_id` or the snapshot version; otherwise 404 `no index for version
    {v}`.
  - **Hits.** A hit's `status` is that declaration's own, from its point.
    `verified` is the module's, and `snapshot_ids` carry the snapshot
    version.
- **Produces, Rust client:**
  - `Client::search(&Query) -> QueryResult`, `search_batch(Vec<Query>) ->
    Vec<QueryResult>`, `resolve(names) -> Vec<Resolution>`.
  - `get_declaration(name, module: Option<&str>) -> DeclarationDetail`.
  - `SearchClient` and `with_env` are removed, and the client's `version`
    is sent instead.
- **Produces, Python client:**
  - `Client.search(text=None, *, description=None, latex=None,
    pattern=None, goal=None, name=None, libraries=None, kinds=None,
    limit=5, include_sorried=False, snapshots=None)`.
  - `search_batch(queries)`, `resolve(names)`, `get_declaration(name, *,
    module=None)`; `SearchClient` goes.
- **Produces, TypeScript client:** `search`, `searchBatch`, `resolve`,
  `getDeclaration(name, {module})`; `ClientOptions.envId` goes.
- **Produces, React:** `useDeclaration(name, module?)` and
  `declarationOptions`. The keys are `["pisa", url, "search", version |
  null, …]`.

- [ ] **Step 1: Write the failing tests**
  - `search.rs` posts to `/search` with `{"queries": [{"text": …,
    "limit": 3}]}` and asserts `version`. The `verify`/`verification`
    test (`:513-519`) is deleted.
  - `server.rs` checks that a server without search answers 404 on
    `/search`.
  - Add `search_records.rs` `a_hit_has_its_own_status_and_its_modules_ids`:
    a module with a proved lemma and a sorry theorem indexes the lemma with
    `status: proved` and the theorem with `status: sorry`. This reads
    `SnapshotDecls` from the catalog, as the file's other tests do.
  - Add `search.rs` `the_snapshot_version_is_accepted` (needs
    `PISA_TEST_QDRANT_URL`, as its neighbours do), for Review Focus 5:
    - a server with `snapshot_version` ≠ `env_id` accepts `version:
      <snapshot_version>` and answers `version: <env_id>`;
    - another version is 404.
  - `options.test.ts` expects `["pisa", url, "search", null, "declaration",
    "x", null]`.
- [ ] **Step 2: Run them and see them fail**

  Run: `cargo test -p pisa-search && cargo test -p pisa-server && cargo
  test -p pisa-tests --test search_records`.

  Expected: FAIL.
- [ ] **Step 3: Implement the renames, the version check, the
  per-declaration status and the client merge**
  - `Hit.title` is `informal.informal_name`, and the public
    `DeclarationDetail.description` is `Description::from(informal)`.
    Stored payloads keep `Informal`.
- [ ] **Step 4: Regenerate the specs and run every suite** (Task 1, Step 4).
  Also run `pnpm -C web test:integration` with `PISA_TEST_QDRANT_URL` if a
  Qdrant is reachable.

  Expected: all pass.
- [ ] **Step 5: Commit:** `jj describe -m "refactor(search): at the root,
  by version, with descriptions and each hit's own status" && jj new`.

### Task 6: Records from before a field existed

**Files:**
- Modify: `crates/pisa-protocol/src/verify.rs`:
  - `Declaration.status: ProofStatus`; `assumes`, `type_refs` and
    `value_refs` become `Vec<String>`.
  - Add a report type for what Lean prints.
- Modify: `crates/pisa-worker/src/{policy,snapshots}.rs`: remove these paths.
  - the export fallback (`exported_uses` and its callers in `compare` and
    in the rebuild plan);
  - the unknown-theorems paths and `affected_unknown`;
  - `records_refs` and `before_recheck`'s re-verify to record refs;
  - the status fallback in `module_status`;
  - the reasons "…verify it to record them" and "the theorems that use the
    changed definitions are unknown".
- Modify: `crates/pisa-db/src/catalog.rs`: `ModuleMeta.dirty` →
  `ModuleMeta.status: ProofStatus`, set at build and at re-check.
- Modify: `crates/pisa-protocol/src/wire.rs`: drop `#[serde(alias =
  "snapshot")]` on `SnapshotJob.entry`.
- Modify: `crates/pisa-protocol/src/api.rs`: drop
  `RebuildItem.affected_unknown`. Update the clients and stubs to match.
- Modify: `README.md` *Upgrading*. Replace it with one entry for this
  release: reset the database and the store's `snapshots/` and `states/`,
  rebuild the image, and upgrade servers and workers together.
- Test: `crates/pisa-worker/src/policy.rs` unit tests,
  `crates/pisa-tests/tests/replaces.rs`, `crates/pisa-tests/src/lib.rs`
  (fake Lean always prints `assumes`, `type_refs` and `value_refs`).

**Interfaces:**
- **Produces:**

  ```rust
  /// What Lean's report prints for a declaration; `judge_decls` turns it into a `Declaration`.
  pub struct ReportedDeclaration { pub name, pub kind, pub axioms: Vec<String>, pub assumes: Vec<String>, pub type_hash: String,
      pub value_hash: Option<String>, pub type_refs: Vec<String>, pub value_refs: Vec<String>, pub generated: bool }
  pub struct Declaration { /* the same fields */ pub status: ProofStatus }
  ```
  - `parse_report` and the verifier's decoder return
    `Vec<ReportedDeclaration>`, and `judge_decls` maps them to
    `Declaration`.

- [ ] **Step 1: Update the tests**
  - Delete `replaces.rs`
    `a_record_that_doesnt_say_what_it_uses_is_verified_to_say_it`.
  - Delete `policy.rs`'s test of `assumes: None`, around `:504`.
  - Add `policy.rs` `a_report_without_refs_does_not_parse`: a
    `PISA_DECLS` line missing `type_refs` is an error from `parse_report`.
- [ ] **Step 2: Run them and see them fail**

  Run: `cargo test -p pisa-worker policy`.

  Expected: FAIL (the field is optional today).
- [ ] **Step 3: Implement**
  - Before deleting each legacy path, confirm it is unreachable with the
    refs and `assumes` required: no other caller, no other trigger. Note
    any that is reachable another way in the commit description, and keep
    it.
- [ ] **Step 4: Regenerate the specs and run every suite** (Task 1, Step 4).

  Expected: all pass.
- [ ] **Step 5: Commit:** `jj describe -m "refactor(snapshots): no paths
  for records from before refs, assumptions and statuses" && jj new`.

### Task 7: Docs and scripts

**Files:**
- Modify: `README.md`: *API* (endpoint table, async, check result,
  positions, errors), *Snapshots* (every subsection), *Clients* (Rust,
  Python, TypeScript and React examples), *Search* (API table, hits,
  snapshots in search, examples).
- Modify: `web/packages/client/README.md`, `web/packages/react/README.md`,
  the crate doc in `crates/pisa-client/src/lib.rs:1-37`, and the docstring
  in `crates/pisa-python/python/pisa_client/__init__.py`.
- Modify: `crates/pisa-python/python/pisa_client/errors.py`:
  `NotFoundError`'s doc names snapshots and jobs too.
- Modify: `web/packages/client/src/client.ts`: remove the stray "Removes the
  labels…" comment above `auditSnapshots`; `isNotFound` → `isNotFoundError`
  in `errors.ts`.
- Modify: `scripts/e2e.py`, `scripts/bench_snapshots.py`. The latter `GET`s
  `/snapshots/{id}` with the snapshot's id, not its module.

- [ ] **Step 1: Rewrite the sections above to match the code**
  - Every example is code that runs against the new clients.
  - The README's Rust example compiles as written: copy it into a scratch
    test and `cargo check` it.
- [ ] **Step 2: Grep for leftovers**

  Run: `rg -n '\bentry\b|env_id|/v1/|\bdecls\b|\bdirty\b|informal_name|JobState|StartResponse|SearchClient|StateKey|EntryId|rebuild_plan|set_snapshot_labels' README.md web/packages crates/pisa-client crates/pisa-python scripts web/apps`.

  Expected: only ops uses (`PISA_SEARCH_ENV_ID`, `--env-id`, the manifest,
  the release commands) and internal ones.
- [ ] **Step 3: Run every suite once more** (Task 1, Step 4).

  Expected: all pass.
- [ ] **Step 4: Commit:** `jj describe -m "docs: the API, its snapshots,
  clients and search under their new names" && jj new`.
- [ ] **Step 5: End to end, with the user's go-ahead**
  - The local compose stack holds data, and this release needs it reset.
    Ask before running `docker compose down -v` and `PROJECT=lean-core
    docker compose up --build`, then `python3 scripts/e2e.py`.
  - Report the result as it is.
