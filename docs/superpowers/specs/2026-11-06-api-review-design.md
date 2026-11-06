# API review: one name per thing

## Context

pisa's HTTP API grew endpoint by endpoint, and its names drifted:
- **Ids.** A session's id comes back as `session`, the route calls it `{id}`,
  and a state's id is another kind of string again.
- **Snapshots.** `/snapshots/{entry}` takes an entry, while the field called
  `snapshot` is a module name. So a snapshot response carries two
  identifiers, and neither is the snapshot's id.
- **Version.** The project version is `version` in the core API and `env_id`
  in search. Snapshot ids aren't unique across versions, so every snapshot
  call needs `?version=`.
- **`informal`.** It is a boolean on a hit and a description object on a
  declaration.
- **Declarations.** They are `declarations`, `decls` and `decl`.
- **Clients.** Rust and Python have a separate `SearchClient`, while
  TypeScript has one `PisaClient`. Method and type names differ by language
  (`Session`, `Step` and `Snapshot` there; `StartResponse`, `TacticResponse`
  and `SnapshotResponse` on the wire).
- **Leftovers.** `dirty` repeats `status`. Search's `verify` was never
  built. Code paths exist only for records from before a field existed.

**Goal (user's):** an API that is consistent, simple and elegant. Nothing
uses it yet, so there is no backwards compatibility to keep, and anything
that no longer makes sense goes.

### Decisions made with the user
- **Ids:** a resource's own id is `id`; a reference to another resource is
  `<resource>_id`.
- **Snapshot ids carry their version**, as session and state ids do, so
  `?version=` goes from every endpoint that takes a snapshot id.
- **No URL prefix:** search moves from `/v1/…` to the root.
- **Stored data can be wiped:** records from before a field existed needn't be
  read. Those code paths go, and existing databases are reset once.
- **A hit's `status` is its declaration's own,** not its module's worst, so a
  proved lemma next to a `sorry` reads `proved`. `verified` stays per module.

### Facts this rests on
- **What session and state ids carry.** They are `version~header_key~id`.
  The router routes on the version and the header without a lookup, and the
  worker re-checks both (`pisa-protocol/src/api.rs` `Key`,
  `pisa-router/src/sessions.rs`, `pisa-worker/src/sessions.rs`).
- **Snapshot ids today.** An entry id is a random UUID (32 hex,
  `pisa-db/src/queries/catalog.rs:106`), keyed in the DB by
  `(version, id)`. The worker checks it with `is_entry_id`
  (`pisa-protocol/src/labels.rs`).
- **What is persisted.** `modules.meta` stores a whole `api::CheckResult`
  (`pisa-db/src/catalog.rs` `ModuleMeta`). `sessions.meta` stores
  `verify::Declaration`s. `jobs.result` holds `wire::JobResult`. `jobs.kind`
  holds the `JobKind` name.
- **`Informal` is stored outside the database too.** It lives in the
  informal store (descriptions keyed by content hash, which cost about $38
  and hours to make), in bundles (`decls.ndjson.zst`, format 4), in Qdrant
  payloads and in `described.record`. Its field names are the stored keys.
- **Lean's report never has `status`.** Lean prints the other `Declaration`
  fields (`pisa-worker/src/policy.rs` `AXIOM_REPORT`,
  `docker/verifier/PisaVerify.lean`); Rust fills `status` in `judge_decls`.
- **Nothing parses `reasons`:** no prefix matching anywhere.
- **Search's version.** Search's `env_id` is `project_version()` of the
  bundle's project, the same function that makes a worker's default label.
  Nothing links the two in code. Search doesn't see `PISA_ALIASES`.
- **What batch ids do.** A batch item's `id` is only echoed back
  (`pisa-router/src/http.rs:220`), and results are already in order.
- **What `complete` means.** It is the REPL's `proofStatus == "Completed"`,
  not the same as having no goals, so it stays.

## Naming rules

1. **A resource's own identifier is `id`**, in bodies and in route params.
2. **A reference to another resource is `<resource>_id` / `<resource>_ids`**,
   or a role name where the role is the point (`from`, `to`).
3. **A field named after a resource holds that resource's object**
   (`snapshot: Snapshot`, `state: State`).
4. **No abbreviations:** `decl` → `declaration`, `deps` → `dependencies`,
   `k` → `limit`, `q`/`nl` → `text`/`description`.
5. **One word per concept:**
   - `version` is the worker pool and project version, in the core API and
     search alike.
   - `module` is a Lean module name.
   - `description` is a natural-language description.
   - `check` is an embedded `CheckResult` (today's `result`).
6. **One shape per thing.** A snapshot is a `Snapshot` wherever it appears;
   `SnapshotDetail` adds `code`, `check` and `exports`.
7. **Types:**
   - A body is `<Operation>Request`.
   - An answer that is one resource is named after it (`Session`,
     `Snapshot`, `SnapshotDetail`, `Job`).
   - Any other answer is `<Operation>Response`.
   - `<Operation>` is the OpenAPI operationId.
8. **Clients use the wire.** Rust, Python and TypeScript use the protocol's
   type names. A method is named after its operationId (snake_case or
   camelCase), and its async form is `submit_<method>`. A `JobKind` is the
   operationId of the endpoint it ran.

## Design

### Ids
- **Snapshot ids** are `<version>~<32 hex>`, with the same character rules as
  `Key`.
  - The router parses one, routes to its version, and sends the bare hex
    to the worker. The DB keeps storing bare hex.
  - The API adds the version when it answers: snapshot ids in responses,
    `replaces`, rebuild items, audit and search hits.
  - A body id (`replaces`, `from`, `to`) of another version than the
    request is a `bad_request`.
  - Generalise `Key` into one parser for both shapes, rather than writing a
    second one.
- **Session and state ids** keep their format. Only the field names change.
- **Job ids** keep 32 hex. Routes become `/jobs/{id}` and `Location:
  /jobs/{id}`.
- **Version.** `version` is taken only where something is created or listed:
  checks, batch, start session, create snapshot, list, audit, search,
  resolve and get declaration.
  - Search resolves `version` through the router's default and aliases.
    Wire `Versions` into search in `pisa-server`.
  - A search server has two versions: its index's (`env_id`) and its
    snapshots' (`PISA_SEARCH_SNAPSHOT_VERSION`, by default the same). A
    request's `version` is accepted if it names either one, and otherwise
    it is a 404. Responses report the index's version. Snapshot ids in hits
    carry the snapshots' version, so they route to the workers that hold
    them. (My call.)
  - A search request without `version` searches the index, and the router's
    default isn't applied there, because the default pool may differ from the
    index.
  - Ops names stay: `PISA_SEARCH_ENV_ID`, `--env-id`, the bundle
    manifest's `env_id`.

### Endpoints

| Before | After | operationId |
| --- | --- | --- |
| `POST /check` | same | `check` |
| `POST /check/batch` | same | `checkBatch` |
| `POST /sessions` | same | `startSession` |
| `POST /sessions/{id}/check` | same | `checkSession` |
| `POST /states/{id}/tactic` | same | `applyTactic` |
| `POST /snapshots`, `GET /snapshots` | same | `createSnapshot`, `listSnapshots` |
| `GET/PATCH/DELETE /snapshots/{entry}?version=` | `/snapshots/{id}` | `getSnapshot`, `updateSnapshot`, `deleteSnapshot` |
| `POST /snapshots/{entry}/verify?version=` | `/snapshots/{id}/verify` | `verifySnapshot` |
| `GET /snapshots/{entry}/dependents?version=&label=` | `/snapshots/{id}/dependents?label=` | `listDependents` |
| `POST /snapshots/rebuild`, `GET /snapshots/audit` | same | `rebuildSnapshots`, `auditSnapshots` |
| `GET/DELETE /jobs/{job}` | `/jobs/{id}` | `getJob`, `cancelJob` |
| `POST /v1/search`, `POST /v1/resolve` | `/search`, `/resolve` | `search`, `resolve` |
| `GET /v1/decl/{name}?module=` | `/declarations/{name}?module=&version=` | `getDeclaration` |
| `GET /health`, `GET /openapi.json` | same | `health` |

`pisa-server`'s no-search fallback moves from `/v1/{*path}` to the three
search paths.

### Checks
- `CheckResult` loses `id`.
- `Message.data` → `text`. The REPL's own `data` stays in `repl.rs`.
- `CheckBatchRequest {items: [{code}], version?, timeout, detail, at}` →
  `CheckBatchResponse {results}`, in order. Items no longer have an `id`.
- `TacticInfo`, `TermInfo` and `AtPosition` are unchanged. Python's
  `Tactic` becomes `TacticInfo`.
- **`Declaration`:**
  - `status`, `assumes`, `type_refs` and `value_refs` become required.
  - Lean's report carries no `status`, so parsing goes through a
    report-shaped type without it, or a serde default that `judge_decls`
    always overwrites. That is the builder's call; the schema says required
    either way.
  - The hashes and refs stay public (see Left as is).

### Sessions and states
- `StartSessionRequest {code, version?, timeout}` → `Session {id, states:
  [State], messages}`.
- `State {id, goals: [string], pos?, end_pos?}`. `pos`/`end_pos` are the
  sorry's place, only on a session's first states. This replaces `StateInfo`
  and its single `goal`.
- `ApplyTacticRequest {tactic, timeout}` → `ApplyTacticResponse {state:
  State, complete, messages}`.
- `CheckSessionRequest {code, timeout}` → `CheckResult`.

### Snapshots
- **`Snapshot`** = `{id, version, module, header, imports, labels,
  replaces?, status, verified, created}`.
  - `module` is the old `snapshot` field.
  - `dirty` goes.
  - `status` is required.
- **`SnapshotDetail`** = `Snapshot` + `{code, check, exports}`.
  - `exports` (was `decls`) are the declarations code that imports the
    module sees: `Export {name, kind, signature, docstring?, description?}`.
  - That name keeps them apart from `check.declarations`.
- **`Replaces`** = `{snapshot_id, allow?}`, the one shape for the request,
  the stored link and rebuild items.
  - The bare-string form goes, and so does `Replaced`.
- **Create.** `CreateSnapshotRequest {code, version?, timeout, verify?,
  labels, key?, replaces?}` → `CreateSnapshotResponse {snapshot?: Snapshot,
  check, replacement?: Replacement}`.
  - `snapshot` is present exactly when one was created, or replayed by
    `key`.
  - `Replacement` = `{filled, changed_definitions, affected}`; it no longer
    repeats the replaced snapshot's id or `allow`.
  - `verify: true` on code that is already kept but fails the re-check
    answers with no `snapshot`, and the reason is in `check`. It no longer
    returns the module and header. This is my call; nothing it returned
    there was usable as verified.
- **List and dependents.** `ListSnapshotsResponse {snapshots, next?}` (was
  `entries`). `ListDependentsResponse {snapshots}`.
- **Update.** `UpdateSnapshotRequest {labels: {key: value | null}}`, as a
  JSON merge patch: `null` removes a key, and a value sets it. It answers
  `Snapshot`. This replaces `{set, remove}`.
- **Delete.** `DeleteSnapshotResponse {removed: [module]}`. It no longer
  echoes the id.
- **Verify.** `VerifySnapshotResponse {snapshot: Snapshot, checked: [module],
  failure?: {module, reason}}`.
- **Audit.** `AuditSnapshotsResponse {summary, assumptions, not_proved}`.
  Both `AuditAssumption.entry` and `AuditUnproved.entry` become
  `snapshot_id`.
- **Rebuild.**
  - `RebuildSnapshotsRequest {from, to, labels, dry_run, allow_changes,
    retire, verify?, timeout}`. There is no `version`: `from` and `to`
    carry it and must agree.
  - It answers `RebuildSnapshotsResponse`, with the same fields as today
    and `retired: [snapshot id]`.
  - `RebuildItem {level, snapshot_id?, module, new_module, new_header, code,
    labels, key, replaces?, affected}`. `affected_unknown` goes.
  - `RebuildCreated {snapshot_id?, module, new_snapshot_id, new_module}`;
    `RebuildFailed {snapshot_id?, module, check}`; `RebuildSkipped
    {snapshot_id?, module}`.
- **Stored status.** `ModuleMeta.dirty` becomes `ModuleMeta.status`, worked
  out at build and at re-check. `SnapshotDecls.dirty` goes, and each
  `SnapshotDecl` carries its own `status` (see Search).

### Jobs
- `Job {id, kind, status}` (was `JobStatus {job, kind, status}`), and
  `JobStatus` is the enum (was `JobState`).
- `JobKind`: `check`, `start_session`, `apply_tactic`, `check_session`,
  `create_snapshot`, `verify_snapshot`, `rebuild_snapshots`.

### Search
- `SearchRequest {queries, version?}` → `SearchResponse {version, results}`.
- **`Query`** fields become `text` (was `q`), `description` (was `nl`),
  `latex`, `pattern`, `goal`, `name`, `libraries`, `kinds`, `limit` (was `k`),
  `include_sorried` and `snapshots`. `verify` goes.
- **`QueryResult`** keeps `hits`, `warnings` and `dense_pending`;
  `verification` goes.
- **`Hit`.** `informal_name` → `title`. `summary`, `text` and `flags` stay.
- **A snapshot's declaration on a hit.**
  - `snapshot: {entries, dirty, verified}` becomes the flat fields
    `snapshot_ids` (absent when empty), `status?` and `verified?`.
  - `status` and `verified` are only there for snapshot declarations.
  - `status` is the declaration's own (user's decision); `verified` is the
    module's.
  - To get it, the worker gives each `SnapshotDecl` the status of the
    declaration of that name in the module's check when it writes
    `modules.decls`. The indexer then puts that status on each Qdrant point.
- **`Description`** = `{title, text, latex?, aliases, source}` is the public
  form of `Informal` (`informal_name`, `statement_nl`, `statement_latex`).
  - `Informal` itself stays internal and stored as it is, with a
    conversion at the API.
  - `informal` on `DeclarationDetail` and `Export` becomes `description`.
  - `Flags.informal` (a Physlib result stated, not proved) keeps its name,
    now that nothing else is called `informal`.
- **Resolve.** `ResolveRequest {names, version?}` → `ResolveResponse {version,
  results}`.
- **Get declaration.** `GetDeclarationResponse {version, declaration:
  DeclarationDetail}`. `DeclarationDetail` is the old `DeclDetail`, with
  `dependencies` (was `deps`) and `description`.
- **Health.** `SearchHealth.env_id` → `version`.

### Removed
- **Fields.** `dirty` everywhere; search's `verify` and `verification`;
  `CheckResult.id` and `BatchItem.id`; `affected_unknown`; `VersionQuery`;
  the `Replaces` string form.
- **Legacy code paths.** Wiping the data makes these unreachable:
  - the export fallback in `compare` and in the rebuild plan;
  - the "unknown" theorems paths;
  - `before_recheck`'s re-verify to record refs;
  - the policy fallback for a missing `status`;
  - the reasons "…verify it to record them" and "the theorems that use the
    changed definitions are unknown".

  The builder confirms each one is unreachable once refs are required.
- **Wire shim.** The `SnapshotJob.entry` alias `snapshot` goes.
  `Job::Unsupported` stays, as it guards mixed-version rollouts generally.
- **Docs.** README *Upgrading* is replaced by one entry for this release:
  reset the database and the object store's snapshots and states, and
  upgrade servers and workers together. The older entries describe
  upgrading from releases nobody runs.

### Clients
- **One client per language.** Rust `Client` and Python `Client` take in
  `SearchClient`'s methods, and `with_env`/`env_id` become the client's
  `version`. TypeScript drops `envId` from `PisaClient`.
- **Method names.** These are the same in all three languages:

  | Operation | Method | Async form |
  | --- | --- | --- |
  | checks | `check`, `check_batch`, `check_session` | `submit_check`, `submit_check_session` |
  | sessions | `start_session`, `apply_tactic` | `submit_start_session`, `submit_apply_tactic` |
  | snapshots | `create_snapshot`, `get_snapshot`, `list_snapshots`, `update_snapshot`, `delete_snapshot`, `verify_snapshot`, `list_dependents`, `audit_snapshots`, `rebuild_snapshots` (with `dry_run`; `rebuild_plan` goes) | `submit_create_snapshot`, `submit_verify_snapshot`, `submit_rebuild_snapshots` |
  | search | `search`, `search_batch`, `resolve`, `get_declaration` (now with `module` in Rust and Python) | – |
  | other | `health` (returns `Health` everywhere) | – |
  | jobs | `poll_job`, `wait_job`, `cancel_job` | – |
- **Rust.**
  - It keeps `<method>_with(&Options)` where there are options:
    `CheckOptions`, `CreateSnapshotOptions`, `RebuildOptions` (with
    `dry_run`).
  - `check_with_detail`, `check_batch_with_detail`,
    `submit_check_with_detail` and `create_snapshot_with_verify` go.
  - It re-exports every wire type, takes one labels type
    (`BTreeMap<String, String>`) and returns the wire answer
    (`delete_snapshot` → `DeleteSnapshotResponse`).
- **Python.**
  - The TypedDicts in `_native.pyi` take the wire names.
  - `poll_job` returns `{"done": True, "answer": …}` or `{"done": False,
    "job": …}`, as TypeScript does, instead of `None`.
  - The `rebuild_snapshots` positional arguments are `from_` and `to`.
- **TypeScript.**
  - The id aliases become `SessionId`, `StateId`, `SnapshotId`,
    `ModuleName` and `Cursor`, and they are used in signatures.
  - `RebuildOptions` gains `timeout`.
  - `isNotFound` → `isNotFoundError`.
  - Its `Job<T>` extends the generated `Job`, imported under another name.
- **React.**
  - `useSetSnapshotLabels` → `useUpdateSnapshot`; `useDecl` →
    `useDeclaration`; `declOptions` → `declarationOptions`.
  - Search keys use `version` instead of `envId`.
  - Fix: the check key includes `at`.
- **Playground.** Follow the renames.

### Left as is (my calls)
- **Check-result internals.** The hashes and refs on a check result's
  declarations stay public. Hiding them needs a second, internal
  check-result type, because `modules.meta` stores the public one. They are
  documented, and the replacement mechanism explains itself with them.
- **`reasons` stay strings.** Nothing parses them.
- **Internal names.** DB tables and internal types keep `entries`/`entry`
  (`pisa-db`, `pisa-worker`, `wire.rs`). Renaming them is a large internal
  diff with no API effect; it is a follow-up if wanted.
- **Per-call `version` and `timeout`** stay TypeScript-only: Rust and Python
  set them per client.
- **Snapshot `version` field.** `Snapshot.version` stays, although the id
  carries it, because ids are opaque to clients.

## Implementation

Each step is one jj change, in the repo's commit style. Each one leaves
`cargo test --workspace`, the regenerated `openapi.json` and `openapi.ts`,
`pnpm typecheck && pnpm test`, and the Python tests green. Each step changes
the protocol, router, worker, search, all clients, tests and docs for its
slice together.

1. **Checks, sessions, states and jobs.** `id` fields, `State`, `Session`,
   `ApplyTactic*`, `Job`/`JobStatus`/`JobKind`, `Message.text`, batch without
   ids, `Declaration` required fields. Client method names for these.
2. **Snapshots.**
   - Versioned ids and the shared id parser; `Snapshot`/`SnapshotDetail`/
     `Export`; `Replaces`/`Replacement`.
   - The create, verify, delete and update (merge patch) shapes; list,
     dependents, audit and rebuild.
   - `ModuleMeta.status`, and client methods for these.
3. **Search.**
   - Root paths and `/declarations`, `version` with aliases, the `Query`
     and `Hit` renames, `Description`, `DeclarationDetail`.
   - Drop `verify`; `status` in the snapshot payloads.
   - Merge `SearchClient` into `Client` (Rust and Python), and the React
     and playground search renames.
4. **Legacy removal.** The code paths and reasons above, the wire alias,
   `affected_unknown`; README *Upgrading*.
5. **Docs and scripts sweep.**
   - The README sections: API, Snapshots, Clients, Search.
   - Package READMEs, crate docs, the `pisa_client` docstring.
   - `scripts/e2e.py` and `scripts/bench_snapshots.py`; the latter also
     fixes its `GET /snapshots/{module}`.

**Critical files:**
- **Protocol:** `crates/pisa-protocol/src/{api,search,verify,labels,wire}.rs`.
- **Router:** `crates/pisa-router/src/{http,sessions,snapshots,jobs,rebuild,dispatch}.rs`.
- **Worker:** `crates/pisa-worker/src/{snapshots,sessions,policy}.rs`.
- **DB:** `crates/pisa-db/src/catalog.rs`.
- **Search:** `crates/pisa-search/src/{server,engine,points,indexer}.rs`.
- **Server:** `crates/pisa-server/src/lib.rs`.
- **Rust and Python clients:** `crates/pisa-client/src/lib.rs`,
  `crates/pisa-python/{src/lib.rs,python/pisa_client/*}`.
- **TypeScript, React and playground:**
  `web/packages/client/src/{client,types,errors}.ts`,
  `web/packages/react/src/**`, `web/apps/playground/src/**`.
- **Tests:** `crates/pisa-tests/tests/*`, `crates/pisa-server/tests/*`,
  `crates/pisa-python/tests/*`, `web/packages/*/test/**`.
- **Docs:** `README.md`.

## Verification
- **Rust:** `cargo test --workspace` (fake-repl; in-memory SQLite), plus
  `PISA_TEST_DATABASE_URL` against compose's PostgreSQL where it runs. Search
  tests need `PISA_TEST_QDRANT_URL`.
- **Generated types:** `PISA_UPDATE_OPENAPI=1 cargo test -p pisa-server
  --test openapi`, then `pnpm -C web generate`. The freshness tests then pass
  without the variable.
- **Web:** `cd web && pnpm typecheck && pnpm test`, then `cargo build -p
  pisa-server -p pisa-worker -p pisa-tests && pnpm test:integration`.
- **Python:** `cargo build -p pisa-server -p pisa-worker -p pisa-tests -p
  pisa-search && cd crates/pisa-python && maturin develop && pytest tests`.
- **End to end:** `PROJECT=lean-core docker compose up --build` on a fresh
  database volume, then `python3 scripts/e2e.py`, if Docker is available
  here.
- **Leftovers:** grep for old names (`entry`, `env_id`, `/v1/`, `decls`,
  `dirty`, `informal_name`, `JobState`, `StartResponse`, `SearchClient`) in
  the API, the clients and the docs. Only internal or ops uses should
  remain.
