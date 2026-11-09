# Accepting an agent run: lessons from QEDIA

## Context

[QEDIA](https://arxiv.org/abs/2610.17708) builds a Lean library with LLM
agents. A leader fixes the statements, subagents prove them, and the leader
accepts work only when it passes a fixed set of checks. The user's findings doc,
[QEDIA findings for Pisa](https://claude.ai/artifact/BMWiB8KphMYNeCTxLwmJwu),
compares those checks with Pisa at `40679a1` and recommends five changes.

**Goal (user's):** build T1–T5 of that doc, so that a client can run agents on
Pisa and accept their work with one call. Defer T6–T8 (proof-dependency rules
and a vacuity probe).

### Decisions made with the user
- **Scope:**
  - T1 `autoImplicit` off;
  - T2 indexed `replaces` links;
  - T3 per-definition `allow`;
  - T4 an audit against a spec;
  - T5 a written witness convention.
- **Heads:** a snapshot stops being a head when *any* snapshot replaces it,
  whatever its labels. This is stricter: an experiment outside the project's
  labels still forks or extends its run.
- **Forks** are reported, not merged. The audit isn't clean until one branch is
  deleted. There is no merge operation.
- **Rebuilds mirror the links.** A rebuilt copy replaces the new copy of what
  its original replaced: `P1′ replaces S0′` (compared with it), and
  `S0′ replaces S0`.
  - A run is audited against its newest spec snapshot. A rebuild's `created`
    maps old ids to new.
  - An old proof that wasn't retired stays a head, and so a fork.

### Facts this rests on
- **No options are set today.** Workers pass no Lean options anywhere.
  - Lean v4.35.0 defaults `autoImplicit` and `relaxedAutoImplicit` to true
    (`Lean/Elab/AutoBound.lean`).
  - User code is elaborated in two places only: REPL body commands (checks,
    session starts and session checks), and `lean -o` snapshot builds
    (`compile` in `pisa-worker/src/launcher.rs`).
- **The REPL header command can carry options.**
  - The worker sends it as `header_with_report_import(header)`
    (`pisa-worker/src/pool.rs`, `policy.rs`).
  - Its text is built after `header_key` is computed, so adding to it changes
    no key.
  - The REPL stores the command state after the header command, scopes
    included, with the env that every body command reuses.
  - The REPL numbers lines within each command, so the body's positions don't
    depend on the header text.
- **Where options stop.** Proof states keep the options of their `sorry`'s
  context. Tactics don't bind names automatically, so this doesn't matter.
- **Opting back in already works.** `autoImplicit` and `relaxedAutoImplicit`
  are on the `set_option` allowlist (`DEFAULT_OPTIONS`, `policy.rs`).
- **`replaces` links today.**
  - They live in `entries.replaces` (bare hex) and `entries.replaces_allow`,
    with no index.
  - Only the replacing snapshot's row holds a link, so deleting it removes the
    link.
  - The comparison's `changed_definitions` and `affected` are returned by the
    create and stored nowhere.
- **`Allow` has one variant,** `Definitions`, which lets every definition's
  value change (`compare` in `pisa-worker/src/snapshots.rs`).
- **The audit ignores links.** `GET /snapshots/audit` counts the newest
  labelled snapshot on each module and never reads `replaces`.
- **Rebuilds link copies to their own originals.** Today each rebuilt copy
  replaces its own old snapshot, and only when `to` replaces `from`.
  - So a skeleton and its proof rebuilt together fork: S0 is replaced by both
    P1 and S0′.
  - `retire` then deletes P1, and with it the P1 → S0 link.
- **The fakes can't see options.** `fake-repl` and `fake-lean` ignore them. Only
  `scripts/e2e.py`, against real Lean, can show that `autoImplicit` is off.

## Design

### T1: `autoImplicit` off by default
- **REPL path.** The header command the worker builds ends with
  `set_option autoImplicit false`, after the report import. This covers module
  headers too, where it follows the `meta import`.
  - The added line isn't part of `header_key`, and the body's positions don't
    move.
  - Prewarmed headers go through the same function, so they get the option
    too.
- **`lean -o` path.** `compile` passes `-DautoImplicit=false`.
- **Opting back in.** Code can still use `set_option autoImplicit true`, as a
  command or with `… in`, because it stays on the allowlist.
- **Documentation.** The README's verification policy says what the default
  is and how to opt back in.
- **Verification** is in `scripts/e2e.py`, against real Lean:
  - a statement with an unbound name is a Lean error in `/check` and in a
    snapshot create;
  - with `set_option autoImplicit true in`, the same statement elaborates in
    both;
  - session starts behave the same.
  - Unit tests pin the header text and the `lean` arguments.
- **No new reset.** This ships in the same release as the API review's
  database reset (README *Upgrading*), so modules built under the old default
  don't survive.

### T2: indexed `replaces` links
- **A migration on both backends** adds:
  - an index `entries_replaces (version, replaces)`;
  - a column `replaces_changes`, a JSON text holding the comparison's
    `changed_definitions` and `affected` when the create allowed changes.
    These are stored so that an audit can list them later.
- **The catalog gains** `replacers(entry) -> Vec<EntryMeta>`: the snapshots
  whose `replaces` is that entry, whatever their labels.
- **No API change** in T2; T4 uses it.

### T3: `allow` names the definitions that may change
- **The new shape.** `Allow` becomes `{"definitions": ["helper", "bound"]}`.
  - Only the named definitions may change value. A name matches the
    comparison's normalised names, so private names compare as they do now.
  - The bare `"definitions"` form goes.
- **Errors** (both are `bad_request`):
  - an empty list (omit `allow` instead);
  - a name that is neither a definition of the replaced module nor a
    definition of a snapshot the code no longer builds on.

  This checks that the name exists, not that the replaced statements reach it.
  A rebuilt copy inherits `to`'s `allow`, which names `from`'s definitions,
  and most copies use only some of them. (My call.)
- **Unchanged:** types stay frozen, `changed_definitions` and `affected` are
  reported as today, and no new assumptions are allowed.
- **Storage.** `entries.replaces_allow` stores the object as JSON.
- **Clients** take the new shape. Python and TypeScript pass it through; Rust
  gets a struct.

### Rebuild links mirror the run
For each plan item that rebuilds an entry X:
1. **X replaced a Y that this plan also rebuilds.**
   - X's copy replaces Y's copy, with X's own `allow`.
   - It is compared with Y's copy, so it must still keep the rebuilt spec's
     statements.
   - Its `level` is after Y's item.
2. **X replaced a Y that this plan doesn't rebuild.** (Controller ruling
   after the final review.)
   - When `to` replaces `from`, X's copy replaces Y itself, with X's own
     `allow` and, when Y builds on `from`, `to`'s `allow` (the union of the
     names).
   - When it doesn't (an `allow_changes` rebuild):
     - Y doesn't build on `from`: X's copy replaces Y with X's own `allow`.
       `from`'s changes can't reach Y's statements, so the link is true.
     - Y builds on `from`, and every change the plan's comparison of `to`
       with `from` found is a definition's value: X's copy replaces Y with
       X's own `allow` and the plan's `changed_definitions`. They are
       definitions of `from` or of snapshots `from` built on and `to`
       doesn't, so they pass T3's name check for X's copy against Y.
     - Y builds on `from`, and some change is not a value change (such as a
       type, a kind or a missing declaration): X's copy gets no link (case
       4). Y's statements really changed, so the link would be false.
   - Case 3 would make it replace X, which `retire` deletes, and X's row held
     Y ← X: the run would be cut. This happens when a proof imports a helper
     its skeleton doesn't, and the rebuild is from the helper.
   - `to`'s part names `from`'s definitions. A Y that doesn't build on
     `from` has none of them, and the create would refuse the names (T3), so
     they are left out there. (Controller ruling 9 after the final review.)
   - Without `retire`, X and X's copy both replace Y: a fork, as below.
3. **Otherwise, when `to` replaces `from`.** X's copy replaces X, with `to`'s
   `allow`, as today.
4. **Otherwise** it has no link, as today.

**The plan item's link.** In case 1, Y's copy doesn't exist when the plan is
made. So a plan item's `replaces` becomes a `RebuildLink`:
- `{snapshot_id, allow?}` for an existing snapshot;
- `{item, allow?}`, naming the index in `plan` of the item whose new snapshot
  it replaces.

The runner fills in that snapshot's id when it creates the item. A client
running the plan itself does the same. Keys don't change, so sending a rebuild
again still replays.

**`level`** becomes "the wave a create runs in: after every item it imports or
replaces". Its doc and the README say so.

**`retire` keeps `from`.** It deletes the old snapshots the plan rebuilt, top
down, and no longer deletes `from`. (My call.)
- The README's own example rebuilds from a skeleton to the proof that replaces
  it. Deleting `from` there deletes a spec snapshot, and then the run can't be
  audited.
- `to` replaces `from`, so `from` is history the audit reads.
- A client that doesn't need it deletes it with one more call.
- `retired` lists what was deleted, as today.

### T4: the audit against a spec
`GET /snapshots/audit?against=<snapshot id>` takes `against` repeatedly, since a
spec can span several snapshots.
- **Parameters.** Every `against` id must be of one version, which the audit
  uses. A `version` given too must be that one, else a `bad_request` naming
  both, as with a create's `replaces`. `label` and `against` don't combine:
  both together is a `bad_request`.
- **The walk.**
  - From each spec snapshot, follow `replacers` forward, through every
    snapshot whatever its labels, to the heads: the snapshots nothing
    replaces.
  - A snapshot replaced more than once is a fork.
  - Each spec snapshot's own `replaces` chain is also followed back, as far as
    its rows exist. This finds the links that changed the spec itself, for
    example a rebuilt spec's link to the original under `to`'s `allow`.
  - A deleted snapshot on the forward path cuts the run there, since its row
    held its link. A deleted spec snapshot is a 404.
- **The answer.** It is today's `AuditSnapshotsResponse`, computed over the
  heads' modules instead of a label scope, with these fields added when
  `against` is given:
  - `clean`: there is no fork, and every target is accepted on its head.
    - Accepted means verified, and either `proved` or `conditional` with every
      assumption it uses permitted.
    - An assumption is permitted when a spec module declares it, or a snapshot
      a spec module builds on does.
  - `heads`: the run's heads.
  - `forks`: each snapshot replaced more than once, with what replaced it.
  - `targets`: one row per spec declaration and head that carries it, with
    `{name, spec, head, status, verified}`. A target is a declaration of a spec
    module that Lean didn't generate and that isn't an assumption (an `axiom`).
    A replacement must keep every declaration of what it replaces, so every
    head on a path carries all of its spec's targets. There is no "missing"
    list. (My call.)
  - `links`: every link that allowed changes, on the paths forward and on the
    spec snapshots' own chains back. Each is
    `{snapshot_id, replaces, replacement}`, for review: `replaces` is the link
    as the snapshot's own `replaces` shows it (the replaced id and the
    `allow`), and `replacement` the stored comparison (`filled`,
    `changed_definitions`, `affected`). (Changed after the final review, to
    reuse those shapes.)
  - `AuditAssumption.in_spec`: whether the assumption is permitted, as above.
- **No Lean runs.** The audit reads records only.
- **Clients:** Rust `audit_snapshots_against(ids)`, Python
  `audit_snapshots(against=[…])`, TypeScript `auditSnapshots({against})`.

### T5: the witness convention
The README's Snapshots section documents a convention:
- Next to each target, a skeleton states a witness that the target's hypotheses
  can hold, e.g. `theorem t_nonvacuous : ∃ x, Pre x`.
- `replaces` freezes it like any statement.
- If an allowed definition change makes the hypotheses impossible, the witness
  can no longer be proved, and the spec audit shows it.

### Left out (my calls)
- **`heads_only=true` on listings** (the doc's optional item). The spec audit
  covers the need.
- **A flag to turn `autoImplicit` back on server-wide.** Code opts in itself.
- **Assumptions the spec permits by name.** The spec's own `axiom`s are the
  permitted assumptions, which needs no new field.

## Verification
- **Rust:** `cargo test --workspace`, with the Qdrant-backed tests against a
  throwaway Qdrant. New tests:
  - T1: the header command text and the `lean` arguments;
  - T2: the replacers query;
  - T3: per-definition allow, an unknown name, an empty list;
  - Rebuild: mirrored links, with a skeleton and its proof rebuilt together
    and no fork from the new spec, with and without `retire`; and a proof
    whose skeleton the plan doesn't rebuild, which replaces the old skeleton
    (a fork without `retire`, a clean run with it);
  - T4:
    - a clean run;
    - a target still `sorry`;
    - a fork;
    - a run through an allowed link, and a rebuilt spec's own allowed link;
    - a `conditional` target on a spec axiom (clean) and on an axiom outside
      the spec (not clean);
    - mixed versions, a `version` that isn't the ids', and `label` with
      `against`;
    - the acceptance flow composed: an allowed edit to a skeleton's
      definition, then an allowed edit below and a retiring rebuild.
  - Rebuild: `retire` keeps `from`, and the audit of a spec spread over two
    snapshots is clean after a retiring rebuild from the first spec snapshot
    to its proof.
- **Generated types:** regenerate `openapi.json` and `openapi.ts`.
- **Other suites:** the web and Python suites.
- **End to end:** `scripts/e2e.py --mathlib --search` against a rebuilt and
  reset compose stack, with the T1 checks added. Real Lean is the only proof
  that `autoImplicit` is off.
