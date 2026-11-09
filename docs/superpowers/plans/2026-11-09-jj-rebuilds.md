# Run history for agent workloads Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a run's history behave like jj's:
- rebuilds copy heads only, each copy replacing its own original (Rule A);
- deletes splice the run (Rule B);
- search and `heads=true` listings show current versions (Rule C);
- a create replaces only a head unless it forks (Rule D);
- a delete can drop a branch;
- a rebuild's support modules aren't snapshots;
- the README documents how agents use all this.

**Architecture:**
- **Wire types** are in `pisa-protocol` (`api.rs`, `wire.rs`, `search.rs`).
- **The router** (`pisa-router`) parses and routes requests, and runs
  rebuilds (`rebuild.rs`). **Workers** (`pisa-worker`) do the work, in
  `snapshots.rs`.
- **The catalog** (`pisa-db`) stores entries and modules. Its queries are in
  `queries/catalog.rs`, included once per backend by `backend.rs`.
- **The search service** (`pisa-search`) indexes snapshots into Qdrant from
  the catalog's change log.
- **Clients:** Rust (`pisa-client`), Python (`pisa-python`), TypeScript
  (`web/packages/client`, with types generated from `openapi.json`).

**Tech Stack:** Rust (axum, utoipa, serde, diesel), Qdrant, PyO3 and maturin,
TypeScript and pnpm, and jj.

**Spec:** `docs/superpowers/specs/2026-11-09-jj-rebuilds-design.md`. Read it
first: it has the rules, the reasons, and the exact behaviour on each edge.

## Global Constraints

- **No backwards compatibility.** This ships with the release whose README
  *Upgrading* resets the database. `RebuildLink` was never on `main`.
- **Naming** follows the API review's rules
  (`docs/superpowers/specs/2026-11-06-api-review-design.md`, "Naming rules"):
  - a resource's own id is `id`, and a reference to one is `<resource>_id`;
  - no abbreviations;
  - client methods are named after operationIds.
- **Snapshot ids** in answers are `<version>~<32 hex>`. The catalog stores
  bare hex, so convert with `Snapshots::id` and `Snapshots::parse`.
- **Exact names and values from the spec:**
  - the error kind `conflict`, 409;
  - the create request's `fork: bool`, and its response's `heads: Vec<String>`;
  - the rebuild response's `replaced: [{snapshot_id, replaced_by}]`;
  - `DELETE /snapshots/{id}?with_replacers=true`, and `deleted` on every
    delete response;
  - `GET /snapshots?heads=true`;
  - Qdrant payload fields `head` (entry points) and `has_heads` (declaration
    points);
  - item keys `rebuild:{old}:{new}:{entry}`, and for support items
    `rebuild:{old}:{new}:{module}`;
  - the not-found message `the replaced snapshot was deleted while this one
    was being built`.
- **Doc comments are the OpenAPI reference.** Rewrite them wherever a field
  or behaviour changes.
- **Regenerate the specs** after each task that changes a wire type:
  `PISA_UPDATE_OPENAPI=1 cargo test -p pisa-server --test openapi`, then
  `pnpm -C web generate`.
- **`_native.pyi` is written by hand.** Keep it equal to the wire.
- **The README section for each task's slice is updated in that task.**
- **Disk.** `df -h /` before a full `cargo test --workspace`. Under 30 GB
  free (it is 29 GB now), run `cargo clean` first.
- **Qdrant.** Never point tests at 6333 or 16333, which hold real indexes.
  Use a throwaway Qdrant:
  `docker run -d --rm --name pisa-review-qdrant -p 127.0.0.1:26333:6333 qdrant/qdrant:v1.20.0`,
  then `PISA_TEST_QDRANT_URL=http://127.0.0.1:26333`.
- **Commits:**
  - one jj change per task: `jj describe -m "<msg>" && jj new`;
  - messages in the repo's style, e.g. `feat(snapshots): …`;
  - never squash, rebase or push.

## Review Focus

1. **A snapshot whose only replacer was deleted** is a head again. A create
   replacing it succeeds, with no `conflict`. (Task 4.)
2. **Two deletes racing on one chain.** In S0←P1←P2←P3, P1 and P2 are
   deleted at once. P3 ends up replacing S0 with both steps' changes,
   whichever commits first. (Task 2.)
3. **Deleting a snapshot whose own link names a deleted snapshot.** Its
   replacers inherit that link unchanged. (Task 2.)
4. **A branch delete whose branch forks further down.** Every snapshot of
   both sub-branches goes, and `deleted` lists them all. (Task 6.)
5. **A module with one head and one replaced entry** stays searchable, and
   its hit's `snapshot_ids` lists only the head. (Task 8.)

---

### Task 1: The two composition tests

These pin the claim the design rests on: links compose along a chain
(spec, *Why the composed link can be trusted*). They fail until Tasks 2
and 3 land, so they go in ignored.

**Files:**
- Modify: `crates/pisa-tests/tests/spec_audit.rs`:
  - `a_run_survives_an_allowed_edit_below_and_a_retiring_rebuild` (:402) is
    rewritten to the new expectations;
  - one new test.

**Interfaces:**
- Consumes the file's helpers: `create`, `on`, `id`, `audit`, `link`,
  `made`, `Cluster::post`.

- [ ] **Step 1: Rewrite `a_run_survives_an_allowed_edit_below_and_a_retiring_rebuild`**,
  with `#[ignore = "Tasks 2 and 3: splices and head-only rebuilds"]`.
  - **Setup:**
    - B is `def g : Nat := 1`.
    - S0 is labelled `p:1`, on B: `def f : Nat := 0\ntheorem t : f = g := sorry`.
    - P1 is labelled `p:1`, on B: `def f : Nat := 1\ntheorem t : f = g := rfl`.
      It replaces S0 with `allow: {definitions: ["f"]}`.
    - B′ is `def g : Nat := 0 + 1`, replacing B with `allow: {definitions: ["g"]}`.
    - A rebuild runs from B to B′ with labels `p:1`, `retire: true` and
      `verify: true`.
  - **Assertions:**
    - The response has status 200 and `ok == true`.
    - `retired == [P1]`: S0 is obsolete, so it is neither rebuilt nor
      retired.
    - P1′ is `made(&r, &p1_)`, and
      `link(P1′) == {"snapshot_id": S0, "allow": {"definitions": ["f", "g"]}}`.
    - `audit([S0])` has `heads == [P1′]` and `clean == true`.
    - Its `links` is
      `[{"snapshot_id": P1′, "replaces": {"snapshot_id": S0, "allow": {"definitions": ["f","g"]}}, "replacement": {"filled": ["t"], "changed_definitions": ["f","g"], "affected": ["t"]}}]`.
- [ ] **Step 2: Add `a_rebuild_that_changes_a_statement_leaves_the_run_on_the_old_proof`**,
  with `#[ignore = "Task 3: head-only rebuilds and retire"]`.
  - **Setup:** B, S0 and P1 as in Step 1. B′ is `def g : Int := 1`, with no
    `replaces`. A rebuild runs from B to B′ with labels `p:1`,
    `allow_changes: true` and `retire: true`.
  - **Assertions:**
    - The response has status 200 and `ok == true`.
    - `changes == ["statement: g has a different type"]`.
    - The plan has one item, `snapshot_id == P1`, with no `replaces`.
    - The response has no `retired`.
    - `audit([S0])["heads"] == [P1]`.
- [ ] **Step 3: Check that they compile and are skipped**

  Run: `cargo test -p pisa-tests --test spec_audit`.

  Expected: PASS, with 2 ignored.
- [ ] **Step 4: Commit**

  `jj describe -m "test(snapshots): links compose along a run, through a retiring rebuild" && jj new`

### Task 2: A delete splices the run (Rule B)

**Files:**
- Modify: `crates/pisa-db/src/queries/catalog.rs`: `remove_entry`, `commit`,
  `add_entry`, and a new `replacers_in`.
- Modify: `crates/pisa-db/src/catalog.rs`: a new pure function `splice`, and
  tests.
- Modify: `crates/pisa-worker/src/snapshots.rs`: the comments in
  `audit_against` that say a deleted snapshot cuts the run.
- Modify: `README.md`: *Replacing → The link*, *Retention*, and the spec
  audit's *The walk*.
- Test: the `mod tests` of `crates/pisa-db/src/catalog.rs` (both backends,
  via `catalogs()`), and `crates/pisa-tests/tests/spec_audit.rs`.

**Interfaces:**
- **Produces**, in `catalog.rs`:

  ```rust
  /// The link and comparison of `replacer` once `deleted`, which it replaces, is gone: `deleted`'s link with the allowed changes of both steps. `None` if `deleted` has no link.
  pub(crate) fn splice(deleted: &EntryMeta, replacer: &EntryMeta) -> Option<(Replaces, Replacement)>
  ```

  Each list is a union, `deleted`'s names first, each once by
  `pisa_protocol::verify::normalised`. `allow` is the union of the two
  `changed_definitions`, or `None` if that is empty.
- **Produces**, in `queries/catalog.rs`:

  ```rust
  async fn replacers_in(conn: &mut Conn, v: &str, e: &str) -> Result<Vec<(String, EntryMeta)>, Fail>
  ```

- **Behaviour:**
  - `remove_entry` updates every replacer with `splice` (the columns
    `replaces`, `replaces_allow` via `allow_column`, and `replacement`)
    before it deletes the row, in the same `tx!`.
  - When E has no link, the replacers keep theirs.
  - `commit` and `add_entry` answer `NotFound`, with the Global Constraints'
    message, when `entry.replaces` names a missing entry. The check comes
    after the key check, so a key replay still wins.

- [ ] **Step 1: Write the failing tests** in `catalog.rs`
  - `a_delete_splices_what_replaces_it`:
    - S0 ← P1 ← P2. P1's replacement is `{filled: ["t"], changed_definitions: ["f"], affected: ["t"]}` with allow `["f"]`.
    - P2's is `{filled: [], changed_definitions: ["g"], affected: ["t"]}` with allow `["g"]`.
    - After deleting P1:
      - P2's `replaces == {S0, allow: ["f","g"]}`;
      - its replacement is `{filled: ["t"], changed_definitions: ["f","g"], affected: ["t"]}`;
      - `replacers(S0) == [P2]`.
  - `a_spliced_link_without_changes_has_no_allow`: neither step changed a
    definition, so P2's `allow` is `None`.
  - `a_deleted_root_leaves_its_replacers_link`: delete S0, and P1 still
    replaces S0's id.
  - `a_fork_moves_down`: S0 ← P1 ← {A, B}. After deleting P1,
    `replacers(S0) == [A, B]`.
  - `a_splice_over_a_deleted_predecessor_keeps_its_link` (Review Focus 3):
    delete S0, then P1. P2 now replaces S0's id.
  - `deletes_racing_on_one_chain_splice_both` (Review Focus 2): S0←P1←P2←P3.
    Delete P1 and P2 with the `racing` helper (multi_thread). P3 replaces S0,
    and `changed_definitions` holds all three steps' names.
  - `a_create_replacing_a_deleted_snapshot_is_not_found`:
    - `commit` and `add_entry` with `replaces` naming a removed entry answer
      kind `NotFound` with the message.
    - The same with a key already on an entry answers that entry.
- [ ] **Step 2: Add the API test** `deleting_a_middle_snapshot_keeps_the_run`
  in `spec_audit.rs`.
  - S0 ← P1 ← P2 over HTTP; then `DELETE /snapshots/{P1}`.
  - `link(P2) == {"snapshot_id": S0}`.
  - `audit([S0])["heads"] == [P2]`.
- [ ] **Step 3: Run them and see them fail**

  Run: `cargo test -p pisa-db && cargo test -p pisa-tests --test spec_audit`.

  Expected: FAIL. (Run with `PISA_TEST_DATABASE_URL` too, if a PostgreSQL is
  at hand.)
- [ ] **Step 4: Implement** `splice`, `replacers_in`, the splice in
  `remove_entry`, and the checks in `commit` and `add_entry`.
- [ ] **Step 5: Update the README**
  - *The link*: a link is set by a create that passed the comparison, or
    composed when the snapshot between is deleted.
  - *Retention*: a delete re-points what replaced the snapshot.
  - *The walk*: a deletion no longer cuts the run.
- [ ] **Step 6: Run them and see them pass**

  Run: the same as Step 3.

  Expected: PASS. The Task 1 tests stay ignored.
- [ ] **Step 7: Commit**

  `jj describe -m "feat(snapshots): a delete splices the run across the snapshot it removes" && jj new`

### Task 3: Rebuilds copy heads only (Rule A, Decision 3, `retire`)

**Files:**
- Modify: `crates/pisa-protocol/src/api.rs`:
  - `RebuildItem.replaces` becomes `Option<Replaces>`;
  - `RebuildLink` goes;
  - `RebuildSnapshotsResponse.replaced`, and a new `RebuildReplaced`;
  - the docs on `level`, `replaces` and `retire`.
- Modify: `crates/pisa-worker/src/snapshots.rs`:
  - `rebuild_plan`;
  - `waves` (drop `replaced`);
  - remove `union`;
  - a new `predicted_swap`.
- Modify: `crates/pisa-router/src/rebuild.rs`:
  - `run` sends `item.replaces` as is;
  - remove `linked`;
  - `retire` deletes only items whose `replaces` is set.
- Modify: `crates/pisa-router/src/snapshots.rs:346`: the `retire`
  description in `rebuildSnapshots`' docs.
- Modify the clients:
  - `pisa-client` re-exports (drop `RebuildLink`, add `RebuildReplaced`);
  - `_native.pyi`;
  - `web/packages/client/src/types.ts`;
  - the generated specs.
- Modify: `README.md` *Rebuilding*, and the spec audit's *Parameters*.
- Test: `crates/pisa-tests/tests/rebuild.rs`, `spec_audit.rs`, `client.rs`,
  `crates/pisa-python/tests/test_client.py`, and
  `web/packages/client/test/client.test.ts`.

**Interfaces:**
- **Produces:**

  ```rust
  // api.rs
  pub struct RebuildReplaced { pub snapshot_id: String, pub replaced_by: Vec<String> }   // derives as AuditFork; doc: a labelled snapshot left out because one without every label replaces it
  // RebuildSnapshotsResponse: #[serde(default, skip_serializing_if = "Vec::is_empty")] pub replaced: Vec<RebuildReplaced>
  // RebuildItem: #[serde(default, skip_serializing_if = "Option::is_none")] pub replaces: Option<Replaces>
  ```

  ```rust
  // snapshots.rs: what a replacement building on `needs` swaps, reading each new module's declarations from its original (`originals`: new module → old module)
  async fn predicted_swap(&self, replacing: &Replacing, needs: &[String], own: &str, originals: &HashMap<String, String>) -> Result<Swapped, Error>
  fn waves(imports: &[Vec<usize>]) -> Vec<usize>
  ```

- **What the plan rebuilds.** From `entries_on`, drop each entry X with a
  replacer whose `key` isn't `rebuild:{old}:{new}:{X}`.
  - An entry is listed in `replaced` when some of its replacers lack a label
    of the request. `replaced_by` names those replacers, as versioned ids.
  - The modules of dropped entries reach the plan only through `needed`, as
    items without `snapshot_id` (as today until Task 5).
- **Each item's link.** For X on module M, rebuilt as M′:
  1. Build `Replacing { replaced: {id(X), allow: None}, entry: X, declarations: M's, needs: M's needs }`.
  2. X′'s needs are M's needs with `old` swapped for `ids(new_meta.modules)`
     and each renamed module for its new id. `originals` maps each new id
     back.
  3. `predicted_swap` gives the swap. Set `allow` to every name `allowable`
     yields, or `None` if there are none, and run `compare(&replacing, M's
     declarations, &swapped)`.
  4. If it passes: `replaces = {snapshot_id: id(X), allow: changed_definitions, or None if empty}`.
     If it fails: `None`.

  This replaces `rebuilt`, `replaced`, `outside`, `to_allow`,
  `statements_kept` and the three-way match at :2014-2034.
- **`level`** is `waves(imports)`: one more than the highest among the items
  it imports. Items are sorted by `(level, created)`, as today.

- [ ] **Step 1: Write the failing tests** in `rebuild.rs`
  - Rewrite `a_skeleton_and_its_proof_are_rebuilt_as_a_run`:
    - the plan's `snapshot_id`s are `[P1]`, and `replaces == [{"snapshot_id": P1}]`;
    - after the run, `replacers(S0) == [P1]` and `replacers(P1) == [P1′]`,
      so there is no fork;
    - with `retire`, `retired == [P1]`, and P1′'s link is `{"snapshot_id": S0}`.
  - Rewrite `a_proof_whose_skeleton_isnt_rebuilt_still_replaces_it`. The item's
    `replaces == {"snapshot_id": P1, "allow": {"definitions": ["g"]}}`.
  - Rewrite the three `allow_changes` tests:
    - values only: `{P1, allow: ["g"]}`;
    - changes out of reach: `{"snapshot_id": P1}`;
    - statements change: no `replaces`.
  - Delete `an_item_that_replaces_a_failed_one_is_skipped`.
  - New `a_resent_rebuild_replays_the_copy_that_replaced_its_original`: run
    the same rebuild twice. The second has the same plan, and `created` holds
    the same new ids.
  - New `a_copy_above_a_rebuilt_module_keeps_its_link`. This exercises
    `predicted_swap` through renamed modules.
    - Use `library()`'s chain: C on B, D on C, E on D, all in `p:1`.
    - A `to` replaces B.
    - E's item has `replaces == {"snapshot_id": E}`, and so do D's and C's.
    - After the run, every copy exists with that link.
  - New `a_proof_whose_helper_uses_a_changed_type_is_unlinked`:
    - P1 adds `theorem aux : h = h := rfl` on H.
    - H′ is `def h : Int := 1` with `allow_changes`.
    - P1's item has no `replaces`.
  - New `a_run_that_leaves_the_labels_is_reported`:
    - S0 is in `p:2`, and P1 in `p:1` replaces it.
    - A `p:2` rebuild has an empty plan and
      `replaced == [{"snapshot_id": S0, "replaced_by": [P1]}]`, on a dry run
      and a run.
    - A `p:1` rebuild has no `replaced`.
  - Update `retire_keeps_from` for the new `retired`.
  - `spec_audit.rs`:
    - remove the `#[ignore]`s from Task 1;
    - `a_spec_over_two_snapshots_survives_a_retiring_rebuild` audits against
      `[S_a, S_b]` and has `retired == [P_b]`;
    - `a_run_whose_skeleton_is_outside_the_plan_survives_a_retiring_rebuild`
      has no fork without `retire`, and after it P1′'s link is
      `{"snapshot_id": S0}`;
    - `a_rebuilt_specs_own_link_is_listed` is unchanged.
  - `client.rs:315` expects `Some(Replaces { snapshot_id: above.id, allow: None })`.
  - In `client.test.ts:142-158`, the links are `Replaces`.
- [ ] **Step 2: Run them and see them fail**

  Run: `cargo test -p pisa-tests --test rebuild --test spec_audit --test client`.

  Expected: FAIL.
- [ ] **Step 3: Implement** the types, the plan, the runner and `retire`.
- [ ] **Step 4: Update the README *Rebuilding***
  - *What it rebuilds*: heads.
  - *Levels*: imports only.
  - *Links*: one rule.
  - *Retiring*: only originals their copies replace.
  - *Running it yourself*: no `item` form.
  - `replaced`.
  - *Parameters* in the spec audit: the same ids after a rebuild.
- [ ] **Step 5: Regenerate the specs and run every suite**

  Run: `cargo test --workspace`, the web suite (`cd web && pnpm typecheck && pnpm test`),
  and the Python suite (README *Python*).

  Expected: all pass, the Task 1 tests included.
- [ ] **Step 6: Commit**

  `jj describe -m "feat(snapshots): rebuilds copy heads only, each copy replacing its own original" && jj new`

### Task 4: A create replaces only a head (Rule D)

**Files:**
- Modify: `crates/pisa-protocol/src/api.rs`:
  - `ErrorKind::Conflict` (409);
  - `CreateSnapshotRequest.fork`;
  - `CreateSnapshotResponse.heads`.
- Modify: `crates/pisa-protocol/src/wire.rs`: `SnapshotCreateJob.fork`.
- Modify: `crates/pisa-db/src/catalog.rs`: `NewEntry.fork`.
- Modify: `crates/pisa-db/src/queries/catalog.rs`: the head check in `commit`
  and `add_entry`.
- Modify: `crates/pisa-worker/src/snapshots.rs`:
  - the check after `replacing` in `create` (:967-976);
  - a new `heads_from`;
  - filling `heads` on a catalog `Conflict`.
- Modify: `crates/pisa-router/src/snapshots.rs`: `prepare_create` passes
  `fork`. `rebuild.rs`'s `create` passes `fork: false`.
- Modify the clients:
  - `CreateSnapshotOptions.fork`;
  - Python `create_snapshot(..., fork=False)` and `submit_create_snapshot`;
  - `_native.pyi`: `ErrorKind` gains `"conflict"`; `heads` and `fork`;
  - TS `CreateSnapshotOptions` picks `"fork"`, and `ERROR_STATUS.conflict = 409`.
- Modify: `README.md`: *Replacing* (a new bullet) and the errors table.
- Test: `crates/pisa-tests/tests/replaces.rs`, `rebuild.rs`, `spec_audit.rs`
  and the `catalog.rs` tests.

**Interfaces:**
- **Produces:**

  ```rust
  // api.rs
  ErrorKind::Conflict   // "conflict", status 409; doc: the snapshot a create replaces is already replaced
  // CreateSnapshotRequest: #[serde(default, skip_serializing_if = "std::ops::Not::not")] pub fork: bool
  // CreateSnapshotResponse: #[serde(default, skip_serializing_if = "Vec::is_empty")] pub heads: Vec<String>
  // wire::SnapshotCreateJob: #[serde(default, skip_serializing_if = "std::ops::Not::not")] pub fork: bool
  // catalog::NewEntry: pub fork: bool
  ```

  ```rust
  // snapshots.rs: the versioned ids of the snapshots nothing replaces, reached from `entry` through `catalog.replacers`, oldest first, each once
  async fn heads_from(&self, entry: &str) -> Result<Vec<String>, Error>
  ```

- **Message:** `{replaced} is replaced by {replacers, comma-separated}; the
  run's heads from there are {heads}. Replace a head, or set fork`, all as
  versioned ids.
- **Order in `create`:** the key replay (:957), then `replacing`, then this
  check. So a replay still returns its snapshot, and a refusal builds
  nothing.

- [ ] **Step 1: Write the failing tests** in `replaces.rs`
  - `replacing_a_replaced_snapshot_is_a_conflict`:
    - S, then P replaces S, then Q replaces S. The code of Q ends in `CRASH`,
      so a build would answer `crash`.
    - Q's response has no snapshot, `check.error.kind == "conflict"` and
      `heads == [P]`.
  - `a_fork_replaces_a_replaced_snapshot`: Q with `fork: true` is kept, and
    `replacers(S) == [P, Q]`.
  - `a_snapshot_whose_replacer_was_deleted_can_be_replaced` (Review Focus 1):
    S, then P replaces S, then P is deleted. Q replaces S, with no conflict.
  - `a_replacement_landing_during_a_build_wins`:
    - Q replaces S with `SLEEP 800`, and is spawned.
    - P replaces S at once.
    - Q answers `conflict` with `heads == [P]`.
  - Keep the key-replay test (:245) passing as is.
  - Catalog (`catalog.rs`): `commit` and `add_entry` with `fork: false` on a
    replaced entry answer kind `Conflict`; with `fork: true` they succeed.
  - `rebuild.rs`: new `an_item_that_loses_to_an_agent_fails_and_a_resend_heals`.
    - Two levels in `p:1`: L on `from`, with `SLEEP 800`; X on L.
    - Start the rebuild in a spawned task. While L′ builds, create A in
      `p:1`, on L, replacing X.
    - X′'s item is in `failed`, with `check.error.kind == "conflict"`.
    - Sending the rebuild again plans A, not X.
  - Make the tests that fork on purpose pass `fork: true`, starting with
    `spec_audit.rs`'s `two_proofs_fork_the_run`. Find the rest by running the
    suite.
  - Every `NewEntry` literal gains `fork: false`:
    - `spec_audit.rs:297`;
    - `labelled` in `crates/pisa-search/tests/snapshots.rs:358`;
    - the helpers in `catalog.rs`'s tests.
- [ ] **Step 2: Run them and see them fail**

  Run: `cargo test -p pisa-db && cargo test -p pisa-tests --test replaces --test rebuild --test spec_audit`.

  Expected: FAIL.
- [ ] **Step 3: Implement** the types, the two checks, `heads_from`, and the
  clients.
- [ ] **Step 4: Update the README**
  - *Replacing*: only a head can be replaced; `conflict`, `heads` and `fork`.
  - The errors table: `conflict`, 409.
- [ ] **Step 5: Regenerate the specs and run every suite.**

  Expected: all pass.
- [ ] **Step 6: Commit**

  `jj describe -m "feat(snapshots): a create replaces only a head, unless it forks" && jj new`

### Task 5: A rebuild's support modules aren't snapshots (Decision 7)

**Files:**
- Modify: `crates/pisa-protocol/src/api.rs`:
  - `RebuildCreated.new_snapshot_id` becomes `Option<String>`;
  - the doc of `RebuildItem` says that an item without `snapshot_id` is a
    support item.
- Modify: `crates/pisa-worker/src/snapshots.rs`: `rebuild_plan` gives
  entry-less items empty `labels`.
- Modify: `crates/pisa-router/src/rebuild.rs`:
  - `run` deletes each support item's snapshot once every level has run,
    before `retire`, with `Job::SnapshotDelete`, as `retire` does (Task 6
    later changes both to its new job);
  - its `created` entry has `new_snapshot_id: None`.
- Modify the clients' types, `_native.pyi`, the generated specs, and
  `README.md` *Rebuilding*.
- Test: `crates/pisa-tests/tests/rebuild.rs`.

**Interfaces:**
- **Produces:** `RebuildCreated { snapshot_id: Option<String>, module: String, new_snapshot_id: Option<String>, new_module: String }`.
  `new_snapshot_id` is `None` exactly for support items.

- [ ] **Step 1: Write the failing tests** in `rebuild.rs`
  - Rewrite `a_module_that_links_the_labelled_ones_is_rebuilt_with_the_labels`
    as `a_module_between_is_rebuilt_as_a_support_module`.
    - The item has no `labels`.
    - Its `created` entry has no `new_snapshot_id`.
    - Afterwards `GET /snapshots?label=p:1` doesn't list it.
    - `POST /check` with the labelled copy's `header` succeeds, which shows
      the support module is still there.
  - New `an_obsolete_helper_a_head_imports_is_a_support_module`:
    - H is labelled `p:1`, and H2 replaces it.
    - P is labelled `p:1` and imports H.
    - A rebuild from H's base makes H′ a support module, and only P′ is a
      snapshot.
  - New `a_support_module_whose_dependents_failed_goes`. This uses fake-lean's
    `needs`, as the old `an_item_that_replaces_a_failed_one_is_skipped` did.
    - B declares `b`. M is unlabelled, on B, and needs nothing. Q is in
      `p:1`, on M, and contains `needs b`.
    - The rebuild runs with `allow_changes` to a `to` that declares
      `not_b`. M′ builds, and Q′ fails.
    - After the run, `c.stored_rows(v)` has no module row for M's
      `new_module`.
- [ ] **Step 2: Run them and see them fail**

  Run: `cargo test -p pisa-tests --test rebuild`.

  Expected: FAIL.
- [ ] **Step 3: Implement** the plan, the runner, and the type.
- [ ] **Step 4: Update the README *Rebuilding***
  - *What it rebuilds*: support modules.
  - *Running it yourself*: create a support item without labels, and delete
    its snapshot once the items above it exist.
- [ ] **Step 5: Regenerate the specs and run every suite.**

  Expected: all pass.
- [ ] **Step 6: Commit**

  `jj describe -m "feat(snapshots): a rebuild's support modules aren't snapshots" && jj new`

### Task 6: A delete can drop a branch (Decision 4)

**Files:**
- Modify: `crates/pisa-protocol/src/api.rs`: `DeleteSnapshotResponse.deleted`.
- Modify: `crates/pisa-protocol/src/wire.rs`: `Job::SnapshotDelete` takes a
  new `SnapshotDeleteJob`.
- Modify: `crates/pisa-db/src/catalog.rs` and `queries/catalog.rs`: a new
  `remove_branch`.
- Modify: `crates/pisa-worker/src/snapshots.rs`: `delete` gains
  `with_replacers`. `lib.rs:145` passes it.
- Modify: `crates/pisa-router/src/snapshots.rs`: the `delete` handler takes
  the query and its utoipa param. Also `rebuild.rs`'s `retire` and Task 5's
  support deletes.
- Modify the clients:
  - `DeleteSnapshotOptions` and `delete_snapshot_with`;
  - Python `delete_snapshot(id, *, with_replacers=False)`;
  - TS `deleteSnapshot(id, options: DeleteSnapshotOptions = {})`, with
    `withReplacers?: boolean`;
  - `_native.pyi`.
- Modify: `README.md`: deletes, and the spec audit's *The walk*.
- Test: `catalog.rs` tests and `crates/pisa-tests/tests/spec_audit.rs`.

**Interfaces:**
- **Produces:**

  ```rust
  // api.rs DeleteSnapshotResponse: #[schema(required = true)] #[serde(default)] pub deleted: Vec<String>   // the asked id first
  // wire.rs
  pub struct SnapshotDeleteJob { pub entry: String, #[serde(default, skip_serializing_if = "std::ops::Not::not")] pub with_replacers: bool }
  // catalog.rs: deletes `e` and every entry replacing it, directly or through others, in one transaction, splicing nothing; the deleted ids (`e` first) and each module that went with them
  pub async fn remove_branch(&self, e: &str) -> Result<(Vec<String>, Vec<(String, ModuleMeta)>), Error>
  // snapshots.rs
  pub async fn delete(&self, entry: &str, with_replacers: bool) -> Result<DeleteSnapshotResponse, Error>
  // pisa-client
  pub struct DeleteSnapshotOptions { pub with_replacers: bool }   // Default
  pub async fn delete_snapshot_with(&self, id: &str, opts: &DeleteSnapshotOptions) -> Result<DeleteSnapshotResponse, Error>
  ```

- **The worker's `delete`** cascades `cleanup` from every removed module, as
  it does now for one.
- **The router** reads `with_replacers=true|false` with a `Query` extractor.
  Any other value is a 400.

- [ ] **Step 1: Write the failing tests**
  - Catalog `a_branch_delete_takes_what_replaces_it` (Review Focus 4):
    - S0 ← A1 ← {A2, A3}, plus S0 ← B1.
    - `remove_branch(A1)` returns `[A1, A2, A3]` (A1 first), and
      `replacers(S0) == [B1]`.
  - Catalog `a_branch_that_was_the_only_replacer_leaves_a_head`: S0 ← A1.
    After `remove_branch(A1)`, `replacers(S0)` is empty.
  - `spec_audit.rs` `dropping_a_branch_resolves_a_fork`:
    - A1 and B1 both replace S0, B1 with `fork: true`. A2 replaces A1.
    - `DELETE /snapshots/{A1}?with_replacers=true` answers
      `deleted == [A1, A2]`.
    - `audit([S0])` has no `forks` and `heads == [B1]`.
  - A plain `DELETE` answers `deleted == [its id]`.
  - `?with_replacers=maybe` is a 400.
- [ ] **Step 2: Run them and see them fail**

  Run: `cargo test -p pisa-db && cargo test -p pisa-tests --test spec_audit`.

  Expected: FAIL.
- [ ] **Step 3: Implement** the job, the catalog, the worker, the router and
  the clients.
- [ ] **Step 4: Update the README**
  - Deletes: `with_replacers` and `deleted`.
  - *The walk*: drop a losing branch with `with_replacers`.
- [ ] **Step 5: Regenerate the specs and run every suite.**

  Expected: all pass.
- [ ] **Step 6: Commit**

  `jj describe -m "feat(snapshots): a delete can take the snapshots that replace it" && jj new`

### Task 7: Listings can show heads only (Decision 5)

**Files:**
- Modify: `crates/pisa-protocol/src/wire.rs`: `SnapshotListJob.heads`.
- Modify: `crates/pisa-db/src/catalog.rs` and `queries/catalog.rs`: `list`
  takes `heads`.
- Modify: `crates/pisa-worker/src/snapshots.rs`: `list` takes `heads`.
  `lib.rs:151` passes it.
- Modify: `crates/pisa-router/src/snapshots.rs`: `list_query` reads `heads`,
  and `listSnapshots` gains the utoipa param.
- Modify the clients:
  - `pisa-client` `list_snapshots`;
  - Python `list_snapshots(labels=None, *, limit=None, after=None, heads=False)`;
  - TS `ListSnapshotsParams.heads?: boolean`;
  - `_native.pyi`.
- Modify: `README.md`: *Labels → Filtering*.
- Test: `crates/pisa-tests/tests/listing.rs`.

**Interfaces:**
- **Produces:**

  ```rust
  // wire.rs SnapshotListJob: #[serde(default, skip_serializing_if = "std::ops::Not::not")] pub heads: bool
  // catalog.rs
  pub async fn list(&self, labels: &[(String, String)], limit: usize, after: Option<&str>, heads: bool) -> Result<(Vec<(String, EntryMeta)>, Option<String>), Error>
  // snapshots.rs
  pub async fn list(&self, labels: &[(String, String)], limit: usize, after: Option<&str>, heads: bool) -> Result<ListSnapshotsResponse, Error>
  // pisa-client
  pub async fn list_snapshots(&self, labels: &BTreeMap<String, String>, limit: Option<usize>, after: Option<&str>, heads: bool) -> Result<ListSnapshotsResponse, Error>
  ```

- **The query** adds a not-exists condition: no entry of the version whose
  `replaces` is this entry's id. Use diesel's `alias!` for the inner
  `entries`, so it reads the `entries_replaces` index.
- **The router:** `heads=true|false`. Anything else is
  `heads {value:?} must be true or false`, a 400.

- [ ] **Step 1: Write the failing tests** in `listing.rs`
  - `heads_only_lists_what_nothing_replaces`:
    - S, then P replaces S, both labelled.
    - `?label=…&heads=true` lists `[P]`.
    - After `DELETE P`, it lists `[S]`.
  - `heads_only_pages_like_a_listing`:
    - A; B replaces A; C; D replaces C.
    - With `heads=true&limit=1`, the pages are `[D]`, then `[B]`, then
      empty, each with `next` while full.
  - `heads_must_be_a_boolean`: `?heads=yes` is a 400 with that message.
- [ ] **Step 2: Run them and see them fail**

  Run: `cargo test -p pisa-tests --test listing`.

  Expected: FAIL.
- [ ] **Step 3: Implement** the job, the query, the worker, the router and
  the clients. The React callers at `options.ts:163` keep compiling
  unchanged.
- [ ] **Step 4: Update the README *Filtering*:** `heads=true`.
- [ ] **Step 5: Regenerate the specs and run every suite.**

  Expected: all pass.
- [ ] **Step 6: Commit**

  `jj describe -m "feat(snapshots): listings can show heads only" && jj new`

### Task 8: Search covers heads only (Rule C)

**Files:**
- Modify: `crates/pisa-db/src/queries/catalog.rs`:
  - `commit` and `add_entry` also `log` the replaced entry and its module;
  - `remove_entry` logs what the removed entry replaced, and each re-pointed
    replacer;
  - `remove_branch` logs what its first snapshot replaced;
  - new `has_heads` and `replaced_among`.
- Modify: `crates/pisa-db/src/catalog.rs`: the two new methods.
- Modify: `crates/pisa-search/src/points.rs`:
  - `entry_point` takes `head`;
  - `PointPayload.has_heads`;
  - `INDEXES` gains `("has_heads","bool")`;
  - `ENTRY_INDEXES` gains `("head","bool")`.
- Modify: `crates/pisa-search/src/indexer.rs`:
  - `Module.has_heads`;
  - `read_module`, `rev`, `module_points`, `apply` and `reconcile`;
  - `lead` ensures the two indexes.
- Modify: `crates/pisa-search/src/engine.rs`: `scopes`, `filter` and
  `hit_entries`.
- Modify: `crates/pisa-protocol/src/search.rs`: the docs on `snapshots` and
  `snapshot_ids`.
- Modify: `README.md`: *Snapshots in search* (Scope, Hits) and the hit
  summary at :1035.
- Test: `crates/pisa-search/tests/snapshots.rs` (throwaway Qdrant) and
  `crates/pisa-tests/tests/search_records.rs`.

**Interfaces:**
- **Produces:**

  ```rust
  // catalog.rs
  pub async fn has_heads(&self, m: &str) -> Result<bool, Error>          // an entry on `m` that no entry replaces
  pub async fn replaced_among(&self, entries: &[String]) -> Result<HashSet<String>, Error>   // those of `entries` some entry replaces, one query
  // points.rs
  pub fn entry_point(e: &str, module: &str, created: u64, labels: &BTreeMap<String, String>, head: bool) -> Value   // payload gains "head"
  // PointPayload: #[serde(default)] pub has_heads: bool
  ```

- **The indexer:**
  - `apply` sets `head` from `catalog.replacers(e).is_empty()`.
  - `reconcile` uses one `replaced_among` per page of `entries_after`.
  - `rev` hashes `has_heads` after `has_entries`.
  - `lead` calls `q.index(&engine.entries, "head", "bool")` and
    `q.index(&engine.collection, "has_heads", "bool")` before
    `reconcile`. Both are idempotent, and `load` isn't rerun on upgrade.
- **The engine:**
  - `scopes` adds `is("head", true)` to the facet filter.
  - `filter`'s `All` uses `is("has_heads", true)` in place of `has_entries`.
  - `hit_entries` adds `is("head", true)` for every scope.
  - `detail` keeps `has_entries`: it opens a module by name, not a scope.

- [ ] **Step 1: Write the failing tests**
  - `search_records.rs` `links_are_logged_at_both_ends`:
    - S, then P replaces S.
    - `changes` holds `{entry: S, module: S's}` after P's own row.
    - After `DELETE P`, S is logged again.
  - `snapshots.rs`, with `PISA_TEST_QDRANT_URL` set to the throwaway Qdrant:
    - `a_replaced_snapshot_leaves_search`:
      - S on module `sa1` declares `doubling()`, with labels `run:1`.
      - P on `sa2` replaces S, with the same declarations.
      - `until` the label scope and `All` hit only `sa2`, with
        `snapshot_ids == [P]`.
      - After `remove_entry(P)`, `until` `sa1` is found again.
    - `a_module_with_a_head_and_a_replaced_entry_stays` (Review Focus 5):
      - e1 and e2 are on `sa1`; e3 on `sa2` replaces e1.
      - `sa1`'s hit has `snapshot_ids == [e2]`.
    - `a_splice_and_a_branch_delete_move_heads`:
      - S ← P ← Q. `remove_entry(P)` leaves Q the only hit.
      - `remove_branch(Q)` brings S back.
    - `reconcile_sets_heads`: create S and P (replacing S) before
      `setup.index`. The first search already omits S.
- [ ] **Step 2: Run them and see them fail**

  Run: `cargo test -p pisa-tests --test search_records && PISA_TEST_QDRANT_URL=http://127.0.0.1:26333 cargo test -p pisa-search --test snapshots`.

  Expected: FAIL.
- [ ] **Step 3: Implement** the logging, the catalog methods, the points, the
  indexer and the engine.
- [ ] **Step 4: Update the README** *Snapshots in search*: scopes cover heads
  only, and `snapshot_ids` lists heads. Update the `snapshot_ids` doc in
  `search.rs` to match.
- [ ] **Step 5: Run every suite,** the Qdrant-backed ones included
  (`PISA_TEST_QDRANT_URL`; web and Python search tests too).

  Expected: all pass. Stop the throwaway Qdrant afterwards.
- [ ] **Step 6: Commit**

  `jj describe -m "feat(search): snapshot scopes cover heads only" && jj new`

### Task 9: Agent runs in the README, and the end-to-end check

**Files:**
- Modify: `README.md`: a new section, "Running agents on snapshots", after
  *Auditing a run*.
- Modify: `docs/superpowers/specs/2026-11-09-qedia-acceptance-design.md`: a
  one-line note at the top of *Rebuild links mirror the run* and of the
  `retire` paragraph, pointing to the new spec.
- Modify: `scripts/e2e.py` `snapshot_search`.

- [ ] **Step 1: Add the e2e step.** In `snapshot_search`, after the label
  step:
  - create a proof that replaces the snapshot, with the same labels;
  - `until` the label scope's hits for `e2e_triple` name only the proof's
    module;
  - `step("snapshot search: a replaced snapshot gives way to what replaces it")`.
- [ ] **Step 2: Write the README section**, following the spec's *Agent runs*
  bullets:
  - the leader holds spec ids;
  - agents label everything, use `/check` for scratch work, and replace
    their last submission;
  - on `conflict`, move onto a head from `heads`;
  - parallel attempts pass `fork: true`, and the leader drops losing
    branches with `with_replacers`;
  - while agents work, rebuild without `retire`;
  - accept on a clean `audit?against=`, then clean up;
  - on `not_found`, `audit?against=<spec id>`.

  Each bullet links to the README section that defines what it uses.
- [ ] **Step 3: Add the note** to the acceptance design.
- [ ] **Step 4: Run everything**
  1. `df -h /`; run `cargo clean` if under 30 GB.
  2. `cargo test --workspace`, with `PISA_TEST_QDRANT_URL` on the throwaway
     Qdrant.
  3. The web and Python suites.
  4. Rebuild the compose stack (`docker compose build`, with `.envrc`
     sourced), reset it as README *Upgrading* says, and run
     `PISA_URL=http://localhost:9000 python3 scripts/e2e.py --mathlib --search`.

  Expected: all pass, with the new e2e step printed.
- [ ] **Step 5: Commit**

  `jj describe -m "docs(snapshots): running agents on snapshots" && jj new`
