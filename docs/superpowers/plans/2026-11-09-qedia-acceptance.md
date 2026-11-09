# Accepting agent runs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a client accept an agent run on Pisa with one call:
- `autoImplicit` is off by default;
- `replaces` links are indexed, and their comparison is stored;
- `allow` names the definitions that may change;
- rebuilds mirror a run's links and keep `from`;
- `GET /snapshots/audit?against=…` audits a run against its spec;
- the README documents a witness convention.

**Architecture:**
- **Wire types** are in `pisa-protocol` (`api.rs`, `wire.rs`).
- **The router** (`pisa-router`) parses and routes requests. **Workers**
  (`pisa-worker`) do the work and answer with `wire::JobResult`.
- **The catalog** (`pisa-db`) stores entries and modules, on PostgreSQL or
  SQLite.
- **Clients:** Rust (`pisa-client`), Python (`pisa-python`), TypeScript
  (`web/packages/client`, with types generated from `openapi.json`) and React.

**Tech Stack:** Rust (axum, utoipa, serde, diesel), Lean 4 through the
upstream REPL and `lean -o`, PyO3 and maturin, TypeScript, and jj.

**Spec:** `docs/superpowers/specs/2026-11-09-qedia-acceptance-design.md`

## Global Constraints

- **No backwards compatibility.** Nothing uses the API yet, and this ships with
  the release whose README *Upgrading* resets the database.
- **Naming** follows the API review's rules
  (`docs/superpowers/specs/2026-11-06-api-review-design.md`, "Naming rules"):
  - a resource's own id is `id`, and a reference to one is `<resource>_id`;
  - no abbreviations;
  - client methods are named after operationIds;
  - the async form of a method is `submit_<method>`.
- **Snapshot ids** in answers are `<version>~<32 hex>`. The database stores bare
  hex.
- **Doc comments are the OpenAPI reference.** Rewrite them wherever a field or
  behaviour changes.
- **Regenerate the specs** after each task that changes a wire type:
  `PISA_UPDATE_OPENAPI=1 cargo test -p pisa-server --test openapi`, then
  `pnpm -C web generate`.
- **`_native.pyi` is written by hand.** Keep it equal to the wire.
- **The README section for each task's slice is updated in that task.**
- **Commits:**
  - one jj change per task: `jj describe -m "<msg>" && jj new`;
  - messages in the repo's style, e.g. `feat(snapshots): …`;
  - never squash, rebase or push.

## Review Focus

1. **A skeleton replaced by two parallel proofs.** The spec audit reports the
   fork, and `clean` is false. (Task 5.)
2. **A rebuild whose replaced item fails.** The items that would replace its
   copy are skipped. They are not created against a missing id. (Task 4.)
3. **`allow` naming a private definition.** It matches by normalised name, as
   the comparison does. (Task 3.)
4. **A spec spread over two snapshots, after a retiring rebuild from the first
   to its proof.** It audits clean against `[first spec, rebuilt second spec]`.
   (Task 5.)
5. **A module-system header.** `module` plus `meta import` lines still load
   with the added `set_option`. (Task 1.)

---

### Task 1: `autoImplicit` off by default

**Files:**
- Modify: `crates/pisa-worker/src/policy.rs`
  - `header_with_report_import` → `header_command`;
  - a const for the option line;
  - its unit test.
- Modify: `crates/pisa-worker/src/pool.rs`, `crates/pisa-worker/src/launcher.rs`
  (`compile`, `diagnose_header`), and every other caller of
  `header_with_report_import`.
- Modify: `README.md`, *Verification policy*.
- Modify: `scripts/e2e.py`.
- Test: `policy.rs` and `launcher.rs` unit tests.

**Interfaces:**
- **Produces:**
  - `pub const AUTO_IMPLICIT_OFF: &str = "set_option autoImplicit false";`
  - `pub fn header_command(header: &str) -> String`: today's
    `header_with_report_import` output, with `AUTO_IMPLICIT_OFF` as its last
    line. An empty header becomes `"import Lean.Elab.Command\nset_option autoImplicit false"`.
  - `compile`'s `lean` arguments are
    `["-R", root, "-o", olean, "-DautoImplicit=false", "--json", file]`. Factor
    them into a `fn compile_args(…) -> Vec<String>` so a test can see them.
- **Unchanged:** `header_key`, `split_header`, and every reported position.

- [ ] **Step 1: Write the failing tests**
  - In `policy.rs`, extend the `header_with_report_import` test (now
    `header_command`) with these cases:
    - a plain header;
    - an empty header;
    - a header that already imports `Lean.Elab.Command`;
    - a module-system header (`module` + `public import Foo`), which must end
      `meta import Lean.Elab.Command\nset_option autoImplicit false`.

    In each case the last line is `set_option autoImplicit false`. (Review
    Focus 5.)
  - In `launcher.rs`, `compile_args` contains `-DautoImplicit=false` before
    `--json`.
- [ ] **Step 2: Run them and see them fail**

  Run: `cargo test -p pisa-worker header_command compile_args`.

  Expected: compile errors or failures.
- [ ] **Step 3: Implement**
  - Rename the function and append the line.
  - Pass the flag in `compile`. `diagnose_header` gets the same header text as
    the REPL.
  - In README *Verification policy*, add that `autoImplicit` is off by default,
    and that `set_option autoImplicit true` (alone or with `in`) turns it back
    on.
  - Add to `scripts/e2e.py`, as one check, `autoImplicit is off unless code
    turns it on`:
    - `theorem t : foo = foo := rfl` is not `ok`, with an error mentioning
      `foo`, in `/check` and in `POST /snapshots`, where nothing is kept;
    - `set_option autoImplicit true in\ntheorem t : foo = foo := rfl` is `ok`
      in `/check`, and is kept in `POST /snapshots`;
    - a session started on `theorem t : foo = foo := by sorry` fails with kind
      `lean`.
- [ ] **Step 4: Run every suite**

  Run: `cargo test --workspace`, `cd web && pnpm typecheck && pnpm test`, the
  Python tests, and `python3 -m py_compile scripts/e2e.py`.

  Expected: all pass. There is no wire change, so the specs don't need
  regenerating.
- [ ] **Step 5: Commit**

  `jj describe -m "feat(worker): autoImplicit is off unless code turns it on" && jj new`

### Task 2: Indexed links and stored comparisons

**Files:**
- Create: `crates/pisa-db/migrations/postgres/2026-11-09-000000_entry_replacers/{up,down}.sql`,
  and the same under `migrations/sqlite/`.
- Modify: `crates/pisa-db/src/schema.rs`, `crates/pisa-db/src/catalog.rs`
  (`EntryMeta`, `NewEntry`, `EntryRow`, `entry_from_row`), and
  `crates/pisa-db/src/queries/catalog.rs` (`ENTRY`, `add`, and the new
  `replacers`).
- Modify: `crates/pisa-worker/src/snapshots.rs`: store the comparison when it
  writes a linked entry.
- Test: `crates/pisa-db/src/catalog.rs` unit tests (SQLite, and PostgreSQL when
  `PISA_TEST_DATABASE_URL` is set).

**Interfaces:**
- **Produces:**
  - `up.sql`:

    ```sql
    CREATE INDEX entries_replaces ON entries (version, replaces);
    ALTER TABLE entries ADD COLUMN replacement TEXT;
    ```

    `down.sql` drops both.
  - `EntryMeta.replacement: Option<api::Replacement>`, and the same on
    `NewEntry`. It is the comparison of the create that made the link, stored
    as JSON. It is set exactly when `replaces` is.
  - `Catalog::replacers(&self, entry: &str) -> Result<Vec<EntryMeta>, Fail>`:
    the entries of this catalog's version whose `replaces` is `entry` (bare
    hex), oldest first.
- **Consumes:** `api::Replacement { filled, changed_definitions, affected }`.

- [ ] **Step 1: Write the failing tests** in `pisa-db` `catalog.rs`
  - `replacers_lists_every_entry_that_replaces_one`: two entries replacing
    one, a third replacing neither. `replacers(a)` gives both, oldest first,
    and `replacers(c)` gives none.
  - `a_link_keeps_its_comparison`: an entry added with `replacement:
    Some(Replacement { changed_definitions: vec!["f"], affected: vec!["t"], .. })`
    reads back equal.
- [ ] **Step 2: Run them and see them fail**

  Run: `cargo test -p pisa-db replacers comparison`.

  Expected: FAIL.
- [ ] **Step 3: Implement**
  - Add the migrations, the schema, the fields, the query and the worker
    write.
  - The worker's create already has the `Replacement` beside the entry, in
    `build` and in `add_to_kept`. Store it with the link.
- [ ] **Step 4: Run every suite**

  Run: `cargo test --workspace`.

  Expected: all pass. There is no wire change.
- [ ] **Step 5: Commit**

  `jj describe -m "feat(snapshots): links are indexed and keep their comparison" && jj new`

### Task 3: `allow` names the definitions that may change

**Files:**
- Modify: `crates/pisa-protocol/src/api.rs`: `Allow`, and the docs on
  `Replaces`, `Replacement` and `RebuildSnapshotsResponse`.
- Modify: `crates/pisa-worker/src/snapshots.rs`: `compare`, the name check,
  and `rebuild_plan`'s use of `allow`.
- Modify: `crates/pisa-db/src/catalog.rs`: `allow_column` and
  `entry_from_row`, which store and read the object as JSON.
- Modify: `crates/pisa-router/src/snapshots.rs`: an empty list is an HTTP 400.
- Modify the clients:
  - `crates/pisa-client/src/lib.rs` (re-export);
  - `crates/pisa-python/src/lib.rs` and `_native.pyi`;
  - the generated TypeScript types and any hand-written ones.
- Modify: `README.md` *Replacing*.
- Test: `crates/pisa-tests/tests/replaces.rs`, `rebuild.rs` and `client.rs`,
  `crates/pisa-python/tests/test_client.py`, and the worker unit tests in
  `snapshots.rs`.

**Interfaces:**
- **Produces:**

  ```rust
  /// What a replacement may change besides adding declarations: the values of the named definitions.
  pub struct Allow { pub definitions: Vec<String> }
  ```
  - `Replaces { snapshot_id, allow: Option<Allow> }` keeps its shape.
  - The wire form is `{"definitions": ["helper"]}`. The bare string
    `"definitions"` is refused.
- **Rules:**
  - A `Change::Value` on a definition is allowed exactly when the allow list
    contains its normalised name (`verify::normalised`). This holds for the
    replaced module's own definitions and for those `walk` reaches.
  - `changed_definitions` lists the allowed changes. A change that isn't
    allowed is refused as today: `statement: X has a different value`.
  - An empty list is a 400 `bad_request` from the router.
  - A name that is neither a definition of the replaced module nor a
    definition of a snapshot the code no longer builds on (normalised) is
    reported in-band, as `check.error` of kind `bad_request`. That is how a
    create reports its other request errors, and nothing is kept. The message
    is
    `allow names {name}, which isn't a definition of the replaced snapshot or of a snapshot the code no longer builds on`.
  - A "definition" is a declaration with a `value_hash`.

- [ ] **Step 1: Write the failing tests** in `replaces.rs`
  - `only_the_named_definitions_may_change`: a skeleton defines `f` and `g`.
    - A replacement changing `f` with `{"definitions": ["f"]}` is kept, with
      `replacement.changed_definitions == ["f"]`.
    - One changing `g` under the same allow is refused, with the reason
      `statement: g has a different value`.
  - `allow_must_name_a_definition`:
    - `{"definitions": []}` is a 400.
    - `{"definitions": ["nope"]}` is kept nowhere, and `check.error.kind ==
      "bad_request"`.
    - The bare `"definitions"` is a 400.
  - `a_private_definition_is_allowed_by_its_name` (Review Focus 3): a
    `private def h` that changes is allowed by `{"definitions": ["h"]}`, the
    normalised name the comparison reports.
  - Update the existing allow tests in `replaces.rs`, `rebuild.rs`,
    `client.rs` and `test_client.py` to the list form.
- [ ] **Step 2: Run them and see them fail**

  Run: `cargo test -p pisa-tests --test replaces`.

  Expected: FAIL.
- [ ] **Step 3: Implement**
  - The router rejects an empty list.
  - The worker filters `compare`'s value changes by name and validates the
    names against the replaced declarations and the lost modules'
    declarations. `swapped()` already loads both.
  - Storage is JSON.
  - `rebuild_plan`, which compares `to` with `from` under `to`'s `allow`, uses
    the list.
  - Update the clients, and README *Replacing*: the list form, what may be
    named, and the errors.
- [ ] **Step 4: Regenerate the specs and run every suite**

  Run: `cargo test --workspace`, the web and Python suites.

  Expected: all pass.
- [ ] **Step 5: Commit**

  `jj describe -m "feat(snapshots): allow names the definitions that may change" && jj new`

### Task 4: Rebuilds mirror a run's links and keep `from`

**Files:**
- Modify: `crates/pisa-protocol/src/api.rs`: `RebuildItem.replaces` becomes a
  `RebuildLink`, plus the docs on `level` and `retire`.
- Modify: `crates/pisa-worker/src/snapshots.rs`: `rebuild_plan`, for the
  mirror rule and the levels.
- Modify: `crates/pisa-router/src/rebuild.rs`:
  - fill in an item link from what was created;
  - skip the items that replace a failed or skipped item;
  - `retire` no longer deletes `from`.
- Modify the clients: Rust types, the Python stub, the generated TypeScript
  types, and README sections on running a plan yourself.
- Modify: `README.md` *Rebuilding*.
- Test: `crates/pisa-tests/tests/rebuild.rs`, `client.rs`,
  `crates/pisa-python/tests/test_client.py`, and the TS client tests.

**Interfaces:**
- **Produces:**

  ```rust
  /// What a rebuilt snapshot replaces: a snapshot that exists (`snapshot_id`) or the new snapshot of another item of the plan (`item`, its index in `plan`). Exactly one is set.
  pub struct RebuildLink { pub snapshot_id: Option<String>, pub item: Option<usize>, pub allow: Option<Allow> }
  // RebuildItem.replaces: Option<RebuildLink>
  ```
- **Plan rule,** for an item rebuilding entry X:
  1. X's stored link replaces a Y that this plan also rebuilds:
     - `replaces = {item: <Y's item index>, allow: X's own link's allow}`;
     - the item's `level` is above Y's item's level.
  2. Otherwise, when `to` replaces `from`: `replaces = {snapshot_id: X, allow:
     to's allow}`, as today.
  3. Otherwise none.
- **Levels and order.** `level` is the wave: one more than the highest level
  among the items it imports or replaces. Items are sorted by `(level, created)`.
- **Runner.**
  - A create for an `item` link uses `created[that item].new_snapshot_id`.
  - If that item failed or was skipped, this item is skipped.
  - `retire` deletes the plan's old entries top down, and not `from`.

- [ ] **Step 1: Write the failing tests** in `rebuild.rs`
  - `a_skeleton_and_its_proof_are_rebuilt_as_a_run`:
    - Base B; skeleton S0 on B, with a `sorry`; proof P1 replaces S0 on B. B′
      replaces B, and a rebuild from B to B′ runs.
    - The plan has S0's item at some level L, with `replaces.snapshot_id ==
      S0`.
    - P1's item is at level L+1, with `replaces.item == <S0's index>`.
    - Afterwards P1′'s `replaces.snapshot_id` is S0′'s id, and S0′'s is S0.
  - `an_item_that_replaces_a_failed_one_is_skipped` (Review Focus 2): make
    S0's rebuild fail, for example with code that no longer elaborates on B′.
    P1's item is in `skipped`, not `failed`.
  - `retire_keeps_from`:
    - The existing retire test now expects `from` alive (GET 200).
    - `retired` holds only the old dependents, top down.
  - Update the expectations on `level`, `retired` and `replaces` in the other
    rebuild tests, and in `client.rs`, `test_client.py` and the TS tests.
- [ ] **Step 2: Run them and see them fail**

  Run: `cargo test -p pisa-tests --test rebuild`.

  Expected: FAIL.
- [ ] **Step 3: Implement** the type, the plan rule, the runner and `retire`.
  - Rewrite the `level` and `retire` docs, and README *Rebuilding*:
    - mirrored links;
    - running a plan yourself (map `item` to the snapshot created for it);
    - `retire` keeps `from`.
- [ ] **Step 4: Regenerate the specs and run every suite.**

  Expected: all pass.
- [ ] **Step 5: Commit**

  `jj describe -m "feat(snapshots): rebuilds mirror a run's links, and retire keeps from" && jj new`

### Task 5: The audit against a spec, and the witness convention

**Files:**
- Modify: `crates/pisa-protocol/src/api.rs`: the new audit fields and types,
  and `AuditAssumption.in_spec`.
- Modify: `crates/pisa-protocol/src/wire.rs`:
  `SnapshotAuditJob.against: Vec<String>`.
- Modify: `crates/pisa-router/src/snapshots.rs`, the audit handler:
  - parse repeated `against`;
  - a version taken from the ids;
  - the 400s.
- Modify: `crates/pisa-worker/src/snapshots.rs`, `audit`:
  - factor today's counting over a set of `(module, representative entry)`;
  - add the walk, the targets and the links.
- Modify the clients: Rust `audit_snapshots_against`, Python
  `audit_snapshots(…, against=)`, TS `auditSnapshots({against})`.
- Modify: `README.md` *Snapshots*:
  - the spec audit, under *Statuses*;
  - the witness convention (T5), next to *Replacing*.
- Test: a new `crates/pisa-tests/tests/spec_audit.rs`, plus `client.rs`, the
  Python tests and the TS client test.

**Interfaces:**
- **Produces:**

  ```rust
  // AuditSnapshotsResponse gains, set only with `against`:
  pub clean: Option<bool>,
  pub heads: Vec<String>,                 // snapshot ids, skipped when empty
  pub forks: Vec<AuditFork>,              // { snapshot_id, replaced_by: Vec<String> }
  pub targets: Vec<AuditTarget>,          // { name, spec, head, status: ProofStatus, verified: bool }
  pub links: Vec<AuditLink>,              // { snapshot_id, replaces, allow: Allow, changed_definitions, affected }
  // AuditAssumption gains `in_spec: Option<bool>` (set only with `against`)
  ```
- **Query:** `GET /snapshots/audit?against=<id>&against=<id>`.
  - The ids must parse and share a version, which is used. Otherwise 400 naming
    the versions.
  - `against` together with `label` is a 400.
  - An unknown spec id is a 404 `unknown snapshot <id>`.
- **Walk.**
  - Forward with `Catalog::replacers` (Task 2), over all labels, to the heads.
    A snapshot with two or more replacers is a fork.
  - Backward from each spec through its rows' `replaces`, as far as rows
    exist.
- **Targets.** For each spec module declaration that is neither `generated`
  nor an assumption (`kind == "axiom"`), one row per head reached from that
  spec. The status is that head module's declaration of the same normalised
  name. `verified` is the head module's `verified`.
- **Permitted assumptions.** The axioms declared in a spec module or in any
  module of a spec module's `meta.modules` closure.
- **Accepted target:** `verified && (status == Proved || (status ==
  Conditional && every assumes ∈ permitted))`.
- **`clean`:** `forks.is_empty()` and every target is accepted.
- **Links.** Every link on the forward paths and the backward chains whose
  `allow` is set, with the stored `replacement`'s `changed_definitions` and
  `affected`.
- **Counts.** `summary`, `assumptions` and `not_proved` are today's counts over
  the heads' modules.

- [ ] **Step 1: Write the failing tests** in `spec_audit.rs`
  - `a_finished_run_is_clean`: skeleton S on base A, with a `sorry`. Proof P
    replaces S and is verified.
    - Against S, `clean == true`.
    - `heads == [P]`.
    - Each target is `proved` on P.
  - `a_sorry_target_is_not_clean`: against S alone, `clean == false`, and the
    target is `sorry` on S.
  - `two_proofs_fork_the_run` (Review Focus 1): P and Q both replace S.
    - `forks == [{S, [P, Q]}]`.
    - `clean == false`.
    - Targets have a row per head.
  - `allowed_links_are_listed`: S defines `f`. P replaces S with `{"definitions":
    ["f"]}` and changes `f`. `links` holds that link with
    `changed_definitions == ["f"]`.
  - `a_rebuilt_specs_own_link_is_listed`: audit against a spec rebuilt from an
    edit whose `to` allowed a change. The backward link appears in `links`.
  - `assumptions_permitted_by_the_spec`:
    - A target `conditional` on an `axiom` the spec declares: `clean == true`,
      and `in_spec == true`.
    - On an axiom a proof adds in a newly imported snapshot: `clean == false`.
  - `a_spec_over_two_snapshots_survives_a_retiring_rebuild` (Review Focus 4):
    - S_a, and S_b importing S_a. P_a replaces S_a; P_b replaces S_b and
      imports S_a.
    - A rebuild from S_a to P_a with `retire: true`.
    - Then, against `[S_a, S_b′]`: `clean` once P_b′ proves S_b's targets, and
      `heads == [P_a, P_b′]`.
  - `bad_queries`:
    - mixed versions is a 400;
    - `label` with `against` is a 400;
    - a deleted spec is a 404.
- [ ] **Step 2: Run them and see them fail**

  Run: `cargo test -p pisa-tests --test spec_audit`.

  Expected: FAIL.
- [ ] **Step 3: Implement**
  - The query, the walk and the fields, and the clients.
  - README: the spec audit (what it walks, what `clean` means, an example
    request and answer). Also T5's witness convention: next to each target, a
    skeleton states `theorem t_nonvacuous : ∃ x, Pre x`; it is frozen like any
    statement; the spec audit shows it unproved if an allowed change makes the
    hypotheses impossible.
- [ ] **Step 4: Regenerate the specs and run every suite,** with the
  Qdrant-backed tests against a throwaway Qdrant.

  Expected: all pass.
- [ ] **Step 5: Commit**

  `jj describe -m "feat(snapshots): audit a run against its spec" && jj new`
