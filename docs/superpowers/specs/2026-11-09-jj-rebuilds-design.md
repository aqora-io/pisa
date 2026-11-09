# Run history for agent workloads: lessons from jj

## Context

A run's history is two graphs over two kinds of node:
- **Modules** are content-addressed: `PisaSnap.s<hash>`, where the hash covers
  the code with its imports. Imports (`module_imports`) form a Merkle DAG, like
  git commits and their parents.
- **Entries** (snapshot ids) are random ids, each on one module, with labels,
  a key, and optionally a `replaces` link that a create checked: the new code
  keeps every statement of the replaced module, with only the definitions
  `allow` names changing their value.

The [acceptance design](2026-11-09-qedia-acceptance-design.md) made rebuilds
mirror a run's links, so that a skeleton S0 and its proof P1, rebuilt
together, don't fork. That took four link cases with sub-cases
(`rebuild_plan`, `pisa-worker/src/snapshots.rs`), links to other plan items
(`RebuildLink { item }`, resolved by the runner in
`pisa-router/src/rebuild.rs`), cycle-breaking in `waves`, a `union` of
allows, and a `retire` that must keep `from`. Most of that works around two
things jj never does.

**Goal (user's):** simplify how rebuilds and deletes treat a run's history by
taking rules from jj, record the rationale from jj, and keep everything
aligned with agentic proof workloads.
- **The workload** is the QEDIA loop from the
  [findings doc](https://claude.ai/artifact/BMWiB8KphMYNeCTxLwmJwu):
  - a leader fixes the statements in spec snapshots;
  - subagents prove them, in parallel and over many attempts;
  - the leader accepts their combined work against the spec, and repeats
    the affected checks after any later change.
- **What agents need from run history:**
  - ids that stay valid while they work;
  - forks only from real parallel work;
  - cheap re-checks when something below changes;
  - a way to find the current version of a lemma rather than a superseded
    one.

| What agents need | What gives it |
|---|---|
| Ids that stay valid while they work | Rule A: spec ids survive rebuilds. Rule B: a delete doesn't cut a run. Rule D: a stale submission is told the run's heads. |
| Forks only from real parallel work | Rule A: rebuilds make no forks. Rule D: a fork needs `fork: true`. Decision 4: a losing branch goes in one call. |
| Cheap re-checks | Rule A: a rebuild builds one copy per head, not one per attempt. |
| Finding current versions | Rule C: search shows heads only. Decision 7: rebuilds leave no stale snapshots behind. |

### Decisions made with the user
1. **A rebuild that changes what a skeleton states ends the run there.**
   Consider an `allow_changes` rebuild where, for example, a base
   definition's type changes and the skeleton's statements use it.
   - *Today*, with S0 and P1 both labelled, it creates a fresh skeleton S0′
     with no link, and P1′ replaces S0′: a new run on the new statements.
   - *Now*, S0 is obsolete and isn't rebuilt, and P1′ can't keep P1's
     statements, so P1′ has no link. The old run is unchanged, even with
     `retire`, which keeps P1.
   - *To start a new run*, create the skeleton again on the new base and
     P1′'s code with `replaces` it. Both are cheap, as the modules exist.
   - The same happens, more narrowly, when only the proof's own extra
     statements (helper lemmas) use what changed. Today P1′ then links
     straight to S0.
   - *Why:* a changed statement is a new spec, and a person should set it.
     Keeping today's fresh run would keep the item links and cycle handling
     for this case alone.
2. **Deleting splices links; there are no hidden rows.** Splicing S0 ← P1
   ← P1′ leaves one link listing both steps' changes: the agent's `f` and
   the base edit's `g` together, as `allow: [f, g]`. The audit no longer
   says which step changed which, and the merged link may name a helper's
   definition the skeleton never used.
   - *Why:* the audit asks what changed between the spec and the head.
     Tombstones would keep each step, at the cost of the schema, every entry
     query, retention and key freeing.
3. **A labelled snapshot that something outside the labels replaced is
   reported, not rebuilt.** The rebuild's response lists it with what
   replaced it, so the caller can rebuild that run through its head's
   labels. The rebuild doesn't follow a run into labels the caller didn't
   name.
4. **A delete can drop a whole branch.**
   - `DELETE /snapshots/{id}?with_replacers=true` deletes the snapshot and
     everything that replaces it, directly or through others. This is jj's
     `abandon X::`.
   - *Why:* parallel attempts at one target are normal, so the leader drops
     losing branches often.
   - The audit's `forks.replaced_by` names each branch's first snapshot.
     With splicing, a plain delete of that snapshot moves the rest of the
     branch down, and the fork stays.
5. **Superseded versions are hidden from search, and can be hidden from
   listings, in this design.**
   - *Why:* search returns every entry with the labels, skeletons and
     superseded attempts included. An agent that finds a lemma in a
     skeleton imports the `sorry` version, and its work then needs a
     rebuild.
   - jj shows only visible commits by default.
6. **A create may replace only a head, unless it forks on purpose.**
   - A create whose `replaces` names a snapshot that something already
     replaces is refused before building, and its response names the run's
     heads from there. `fork: true` lets it through.
   - *Why:* after a rebuild, an agent's next submission against its old
     version would otherwise fork the run every time, until the agent moved
     to the new base.
   - With this rule, every fork the leader sees was intended, such as
     parallel attempts at one target.
7. **What a rebuild makes only to support its heads isn't a snapshot.**
   - A module the plan rebuilds because a head imports it, with no labelled
     head of its own, ends up a module kept by what imports it. It appears
     in no listing, search or audit.
   - Today it becomes a labelled snapshot. For an obsolete helper, that
     brings old code, `sorry` and all, back as a head under the run's
     labels.

This design supersedes *Rebuild links mirror the run* and *`retire` keeps
`from`* in the acceptance design, and the README's *Links* under Rebuilding.
T1–T5 are otherwise unchanged.

## What jj does, and why it fits

### The vocabulary

| Pisa | jj | git |
|---|---|---|
| module (hash covers imports) | commit id | commit hash (covers parents) |
| import edge | parent edge | parent edge |
| entry | one version of a change | a tag on a commit |
| `replaces` link | predecessor edge (`jj evolog`) | — |
| a spec snapshot and what replaces it | a change, whose id survives rewrites | — |
| head of a run | the visible commit of a change | — |
| fork | divergent change | — |
| rebuild | automatic rebase of descendants | `git rebase --onto to from` |
| retire | the rewritten commits being hidden | deleting old branches |
| delete with `with_replacers` | `jj abandon X::` | deleting a branch |
| replacing a non-head is refused unless `fork` | the working copy following its rebased change | a non-fast-forward push refused |
| `heads=true`, search's scopes | `jj log`, which shows visible commits | `git log --branches` |

Mercurial's changeset evolution, which jj took this from, names the same
things *obsolete* (an entry something replaces), *successor*, *orphan* (a
head that imports an obsolete entry's module) and *content-divergent*.

### Rule A: rewrite only what's visible, and each new version's predecessor is its own old one

- **In jj**, rewriting a commit hides the old version and records it as the
  new one's predecessor. jj then rebases the *visible* descendants. Each
  rebased commit's predecessor is its own pre-rebase version, never another
  commit's. A hidden commit is history and is never rebased: rebasing it
  would bring back a second visible version of its change, which jj calls
  divergence.
- **In Pisa today**, a rebuild takes every labelled entry on a dependent
  module, obsolete or not. With S0 ← P1 (P1 replaces S0) both on `from`, it
  makes S0′ and P1′. S0′ is a rebased copy of a version that was already
  superseded: a second line out of S0 next to P1, the divergence jj avoids.
  The four link cases exist to stitch that back together (P1′ → S0′, P1′ →
  S0, and so on).
- **The rule:** a rebuild rewrites only heads, entries that nothing
  replaces. Each copy X′ replaces its own original X. S0 isn't rebuilt, so
  there is no S0′ and nothing to stitch.
- **What it buys:** the spec snapshot's id stays the same across rebuilds,
  as a jj change id does. Audit against it before and after; no remapping
  through `created` (except for a spec snapshot that was itself still a head,
  see Rule B).

### Rule B: removing a version never breaks the history across it

- **In jj**, `jj abandon` takes a commit out of the visible graph and rebases
  its children onto its parent. The evolution history stays whole: abandoned
  and rewritten versions remain in the operation log.
- **In Pisa today**, a link lives only on the replacing entry's row.
  Deleting P1 from S0 ← P1 ← P1′ removes the P1 → S0 link, so the run is cut
  at P1′ and S0 becomes a head again. Rebuild case 2 (P1′ replaces S0
  directly) and "retire keeps `from`" exist to route around this.
- **The rule:** deleting an entry E re-points every entry that replaces E at
  what E replaced, with the two links composed. This is jj's abandon applied
  to the evolution graph instead of the parent graph. With Rule A, `retire`
  deletes P1 and the run reads S0 ← P1′.
- **Why not keep hidden rows instead**, as jj keeps hidden commits?
  Tombstones would keep every step, but they touch the schema, every entry
  query, the foreign key to modules, key freeing and module retention.
  Splicing keeps the row model and changes one transaction. The cost is that
  two links merge into one (*Decision 2*).
- **Dropping a branch** is jj's `jj abandon X::`: X and everything after it
  in its line go, and nothing is re-pointed. In Pisa that is a delete with
  `with_replacers` (*Decision 4*).

### Rule C: show the current versions

- **In jj**, `jj log` shows visible commits only. A rewritten version is
  there if you ask for it (`jj evolog`), but it isn't what you build on.
- **In Pisa today**, search and listings return every entry with the
  labels. An agent can't tell the skeleton from the proof that replaced it,
  so it may build on the version with `sorry`.
- **The rule:** search covers heads only, and listings can ask for heads
  only (`heads=true`).
- **Nothing is lost:** a replacement keeps every declaration of what it
  replaces, so a head carries every statement its predecessors made. A
  superseded version adds only old proofs and old definition values.

### Rule D: extend the current version, or fork on purpose

- **In jj**, rebasing a change moves your working copy along with it: you
  keep editing the new version, never the one it replaced. Git refuses a
  push that doesn't extend the branch's tip ("fetch first").
- **In Pisa today**, an agent is the working copy, and Pisa can't move its
  unsubmitted code. A create that replaces an old version forks the run
  silently.
- **The rule:** a create may replace only a head. Otherwise it is refused
  and told where the run went, before anything is built, as git does.
  Divergence stays possible, as in jj, but only when asked for (`fork:
  true`).

### Why the composed link can be trusted

A link means "keeps the statements of". It composes: if Y ← X and X ← X′
both passed the comparison (`compare`), then X′ keeps Y's statements, and the
definitions that changed are those in either step's `changed_definitions`.
- **Types:** X declares each of Y's declarations with the same kind and type
  hash, and X′ each of X's, so every statement of Y is in X′ with the same
  hash.
- **What hashes name:** a hash names constants without saying what they are.
  Every constant Y's statements reach falls into one of three cases at each
  step:
  - one of the replaced module's own declarations, compared by `statement_changes`;
  - in a snapshot both sides build on, so identical;
  - in a snapshot the new side no longer builds on, compared by `walk`.

  X has Y's statements as the same terms, so whatever they reach in step
  two, `walk` follows from X's declarations.
- **Assumptions:** neither step adds one (`added_assumptions`), so the chain
  doesn't either.

This is argued from the code, not proved. The tests in *Verification* pin
it.

### What doesn't carry over from jj

- **Rebasing everything on every rewrite.** In jj a rebase is free. Here each
  rebuilt module is a Lean build, so rebuilds stay explicit and
  label-scoped.
- **Conflicts as data.** jj keeps a rebased commit with conflict markers. A
  module that doesn't compile can't be imported, so a failed item and its
  skipped dependents, resent by key, remain the right answer.
- **The operation log and undo.** Not needed for this simplification.
- **Divergence** already matches jj: a fork is reported, never merged, and
  resolved by deleting one side. Under *Rule D* it also happens only when
  asked for.

## Facts this rests on

- `entries_on(modules, labels)` returns every labelled entry on the
  dependents, replaced or not; the plan uses it as is.
- `remove_entry` (`pisa-db/src/queries/catalog.rs`) deletes the row,
  link included. `replacers` reads the `entries_replaces` index.
- `commit` doesn't check that the entry a create replaces still exists, so a
  create racing a delete can link to a deleted entry today.
- Writes are serializable and retried (`pisa-db/src/db.rs`), so concurrent
  deletes that splice the same chain are safe.
- A create with `replaces` whose comparison fails keeps nothing, so the plan
  must know each item's link before it runs (as today).
- `RebuildLink` and the `item` form aren't on `main` (`40679a1`). Removing
  them breaks no released client.
- **Search's snapshot index** (`pisa-search`) follows the catalog's change
  log. Each `changes` row names an entry and/or a module; the indexer
  re-reads that entry or module and upserts its points (`apply` in
  `indexer.rs`).
  - Entries are points in an `entries` collection
    (`{entry, module_id, created, labels}`, `points::entry_point`). A label
    scope is a facet of that collection's `module_id`s (`scopes` in
    `engine.rs`).
  - `"all"` filters declaration points on `has_entries`, a per-module flag
    that `refresh` keeps current.
  - Nothing there knows about `replaces`. Creating R, which replaces E, logs
    R only, so nothing tells the indexer that E stopped being a head.
- **A second replacement forks silently today.** Neither the create path nor
  the catalog checks whether the replaced snapshot is still a head.
  `ErrorKind` has no conflict kind.
- **A rebuild's modules without a labelled head** are created today as
  snapshots with the request's labels (`rebuild_plan`, the `None` arm). Each
  such snapshot is a head under the run's labels.
- A rebuilt copy's code differs from the original only in import lines.
  Hashes name constants, not modules, so its declarations hash as the
  original's, unless the new imports change how its code elaborates, such as
  a new instance. The create then refuses the link and the item fails, as
  today's case 3 does.

## Design

### Rule A: the plan

1. **What it rebuilds.** As today, but an entry that another entry replaces
   (whatever its labels, as for the audit's heads) isn't an item.
   - **Resending.** An entry whose replacers are all its own copies from
     this same rebuild (key `rebuild:{old}:{new}:{entry}`) stays an item, so
     sending the rebuild again replays its create by key.
   - **Support items** (*Decision 7*). A module between the items and
     `from` is a support item, as are the modules of obsolete entries that a
     head imports. It has no `snapshot_id`, no labels and no link (see
     *Support modules*).
   - **Runs that leave the labels** (*Decision 3*).
     - The response gains `replaced: [{snapshot_id, replaced_by}]`, on dry
       runs and runs alike: each labelled entry left out because an entry
       without every label replaces it, with those replacers.
     - An entry whose replacers all have the labels isn't listed: its run
       goes on through them, and they are items or listed themselves.
     - It doesn't change `ok`, which still means every item was created.
     - (The field's name and shape, which match `forks` in the audit, are
       my call.)
2. **Each item's link.** For an item on entry X, the plan runs the same
   comparison the create will run, X against X′, before anything is built:
   - X′'s declarations are taken to be X's.
   - What X′ builds on is computed from X′'s imports, as the create will
     compute it: the rebuilt modules' new ones, and `to` with its closure
     in place of `from`.
   - The new modules' declarations are taken to be their originals'.

   Every definition is allowed to change its value (`allowable`). Then:
   - **It passes:** `replaces: {snapshot_id: X, allow?}`, where `allow`
     names the definitions the comparison found changed (`changed_definitions`).
     If none changed, there is no `allow`.
   - **It fails:** no link.

   This one rule replaces cases 1–4, `statements_kept`, `on_from`, `to_allow`
   and the `outside` block. When `to` replaces `from`, the comparison can only
   find changes `to`'s comparison already allowed. A link's `allow` now says
   what changed, rather than repeating `to`'s `allow`, so it never names
   something the create would refuse. (My call.)
3. **`level`** is the wave by imports alone: one more than the highest among
   the items whose modules it imports. `waves` loses `replaced` and the cycle
   handling.
4. **`changes`, `changed_definitions`, `affected`, `allow_changes`, limits
   and keys** are unchanged.

### Rule A: the API and the runner

- `RebuildItem.replaces` becomes `Option<Replaces>` (`{snapshot_id, allow?}`).
  `RebuildLink` goes, from `pisa-protocol`, `openapi.json`, the TypeScript
  types, the Python stub and `pisa-client`.
- `RebuildSnapshotsResponse` gains `replaced`, empty and left out when there
  is none, like `forks`.
- The runner sends each item's `replaces` as is. `linked`, and skipping an
  item whose replaced item wasn't made, go. An item is skipped only when it
  builds on one that failed or was skipped.
- **Items lose races.** An item's create is held to *Decision 6* like any
  other, without `fork`. If an agent replaced X after the plan was made,
  X′'s create is refused and the item fails. Sending the rebuild again then
  plans the agent's new head instead of X. This is self-healing, and no fork
  is made.
  - A replay by key comes before that check, so a resent rebuild still
    replays the copies it already made.
- `retire` deletes the old snapshots the plan rebuilt whose copies replace
  them. They are all heads now, so each deletion splices (Rule B), and the
  run goes on through the copies.
  - An original whose copy has no link is kept and isn't in `retired`.
    Deleting it would make the run's earlier snapshot its head again, `sorry`
    and all, and the copy belongs to no run. Today's `retire` deletes it.
    (My call.)
  - `from` isn't a dependent, so it is never retired. Neither is an
    obsolete entry.

### Support modules (*Decision 7*)

A support item ends up a module that only what imports it keeps. Existing
creates and deletes do this, with no new create mode. (The mechanism is my
call; the outcome is the user's.)
- **The runner** creates a support item as a snapshot without labels, keyed
  as today (`rebuild:{old}:{new}:{module}`), so that it exists while the
  items above are built. Once every level has run, it deletes those
  snapshots.
  - A support module that something created imports stays, kept by that
    import, with no snapshot. Search's `has_entries` and `has_heads` are
    false for it, and listings don't show it.
  - One whose dependents all failed or were skipped goes with its snapshot,
    through the usual cleanup. Nothing leaks.
  - If the rebuild's server stops mid-run, the support snapshots stay until
    the rebuild is sent again. That run replays them by key, then deletes
    them. Until then they are unlabelled heads.
- **The plan.** A support item carries no `labels` and no `replaces`. It
  keeps its `key`, and its `level`, `code`, `module` and `new_module` are as
  today.
- **`created`** lists a support item with `module` and `new_module` only:
  its snapshot is gone by the time the response is read.
- **Running the plan yourself:** create each support item as given, without
  labels. Delete its snapshot once the items above it are created.
- **`retire`** doesn't touch support items: they have no old snapshot of
  their own.

### Replacing only heads (*Decision 6*)

- **The check.** A create whose `replaces` names a snapshot that another
  snapshot already replaces is refused. It runs in two places:
  - when the create resolves what it replaces (`replacing`), before
    anything is built, so a stale submission costs no build;
  - again in `commit`'s and `add_entry`'s transactions, which already check
    that the replaced snapshot still exists (*Rule B*). A create that loses
    a race during its build is refused there, and its build's files go, as
    with any refused commit.
- **The refusal** is a new error kind, `conflict` (409 when not in-band), in
  `check.error`, so agents can tell it from a malformed request. Agents
  branch on it. (My call.)
  - The create's response gains `heads`: the snapshots nothing replaces,
    reached from the replaced one by following what replaces it, as the
    audit's walk does. The agent moves onto one and submits again.
  - The message names them too, e.g. `…~9c0e is replaced by …~4b7a; the
    run's heads from there are …~4b7a. Replace a head, or set fork`.
- **`fork: true`** on the create request skips the check. It sits on the
  request, not inside `replaces`, because `replaces` is also the shape
  stored and listed for a link.
- **What stays as it is:**
  - A key replay comes first and returns the snapshot it made, whatever has
    replaced it since.
  - Splices and branch deletes re-point or remove links without a create.
- **Clients:** `fork` on the create in Rust, Python and TypeScript; the
  `conflict` kind in each client's error types; `heads` on the create
  response.

### Rule B: deleting splices

In `remove_entry`'s transaction, after reading E's row and before deleting
it, for each entry R whose `replaces` is E:
- **If E has a link:** R's link becomes E's `snapshot_id`. R's `allow`
  becomes the union of E's and R's `replacement.changed_definitions` (E's
  first, each once), or none if that is empty. R's `replacement` becomes the
  union of the two, field by field (`filled`, `changed_definitions`,
  `affected`).
  - `allow` is what changed along the way, not the union of the two allows.
    It is set by the server, not by a create, so it isn't held to *What may
    be named*. It can name a definition the older snapshot never built on.
    - For example: P1 adds a helper lemma whose statement uses H's `hd`,
      which S0 doesn't build on, and a rebuild from H changes `hd`. P1′ ←
      P1 lists `hd`, and so does the spliced P1′ ← S0.
    - That is true of the run, if not of S0. Limiting it to names S0's
      comparison would accept needs module records inside the splice. (My
      call, within *Decision 2*.)
    - Names are kept once each by their normalised form.
  - `filled` can list a declaration that a later step left `sorry` again.
    The audit's `targets` gives the status on the head. (My call.)
- **If E has no link** (a root, such as a spec snapshot), R keeps its link
  to E, which now names a deleted snapshot, as today.
- Each changed R, and what it now replaces, gets a row in `changes`. Search
  needs them (*Heads*, below).

`commit`, and `add_entry` for code already kept, also check in their
transactions that the entry a create replaces still exists. If it doesn't,
the create answers `not_found` ("the replaced snapshot was deleted while
this one was being built"), as it does for a deleted import. So a link to a
deleted entry arises only when a root is deleted. (My call.)

**Forks.** Deleting a snapshot in the middle of a branch now moves the rest
of the branch down instead of dropping it. To drop a branch, use the next
section's option.

### Dropping a branch (*Decision 4*)

- **The call.** `DELETE /snapshots/{id}?with_replacers=true`, run in one
  catalog transaction:
  - It collects the snapshot and every entry that replaces it, directly or
    through others (through `replacers`), and deletes all of their rows.
  - Nothing is spliced, since everything a splice would re-point goes too.
  - Their modules are then cleaned up as any delete cleans up its module.
- **What it leaves.** If the snapshot replaced D, the branch no longer
  counts among D's replacers. If none are left, D is a head again, as when
  a head is deleted today.
- **A create racing it.** A create that commits first is collected with the
  rest. One that commits later finds what it replaces gone and answers
  `not_found` (*Rule B*).
- **The response.** `DeleteSnapshotResponse` gains `deleted`: the ids of
  every snapshot deleted, the one asked for first. It is on every delete,
  so a plain delete lists one. (My call.) `removed` lists modules, as today.
- **Without the option**, a delete splices (*Rule B*).
- **Clients and plumbing.** The option goes through the delete job and the
  router. Clients take it as an optional argument: Rust, Python
  `delete_snapshot(id, with_replacers=True)`, TypeScript
  `deleteSnapshot(id, {withReplacers: true})`. (Names are the builder's
  call.)

### Heads in listings and search (*Decision 5*, *Rule C*)

A **head** is an entry that no other entry replaces: the audit's definition.
A link to a deleted entry doesn't count, as its row is gone.
- **Listings.** `GET /snapshots?heads=true` lists heads only. It combines
  with `label`, `limit` and `after` as today.
  - The catalog's `list` adds a not-exists condition on the
    `entries_replaces` index.
  - The default stays every snapshot: listings also serve cleanup and
    history.
- **Search.** Every snapshot scope, `"all"` and a label scope, covers heads
  only. There is no opt-out, since a head carries every statement its
  predecessors made (*Rule C*). (My call.)
  - Entry points gain `head: bool`. A label scope facets `module_id` over
    the entries that have the labels and `head: true`.
  - Declaration points gain `has_heads: bool`, a per-module flag like
    `has_entries`. `refresh` keeps it current, and the module's `rev`
    includes it. `"all"` filters on it. The catalog gains
    `has_heads(module)`.
- **The change log** gets rows for both ends of every link a transaction
  adds, removes or re-points, each with its module:
  - a create with `replaces` (`commit`, `add_entry`) logs the replaced entry
    too;
  - a delete logs what the deleted entry replaced, and each re-pointed
    replacer with its new target;
  - a branch delete logs what the branch's first snapshot replaced.

  The indexer's `apply` sets an entry's `head` from `replacers`, and the
  module refresh sets `has_heads`.
- **Existing points** get the fields from the indexer's `reconcile` pass,
  which rewrites entry points from the catalog. It must set `head` too.
- **A hit's `snapshot_ids`** lists heads only, so an agent isn't pointed at
  a version it can't replace. (My call.)
- **Two heads with the same statements**, such as P1 and an unlinked P1′
  after *Decision 1*, both show in search. That is no worse than today.

### Agent runs (a README section; my call)

A short section, "Running agents on snapshots", next to *Auditing a run*.
It describes the protocol this design assumes:
- **The leader** creates the spec snapshots: skeletons with `sorry`, one or a
  few targets each, with their witnesses. It labels the run and holds the
  spec ids. Those ids stay valid through rebuilds.
- **Agents** are given their target's spec id. They label every snapshot
  with the run's labels, and keep scratch work in `/check`.
  - To submit, an agent replaces the snapshot it last submitted for its
    target, or the spec snapshot at first.
  - Search shows current versions only, so what an agent imports is a head.
- **A `conflict` answer** means the run moved under the agent: a rebuild
  copied its last version onto a new base, or another agent extended it.
  Nothing was built. The agent:
  1. takes a head from the response's `heads`;
  2. moves its code onto that head: its imports become those of the head's
     `code` (`GET /snapshots/{id}`);
  3. submits again, replacing the head.
- **Parallel attempts** at one target pass `fork: true` on their first
  submission, then extend their own branch. The leader keeps one branch and
  drops the others with `with_replacers=true`, using the ids in `forks`.
  Every fork in the audit is one of these.
- **Re-checks.** When something below changes, such as a helper proved or a
  definition fixed, the leader rebuilds without `retire` while agents work.
  - Agents meet the copies as `conflict`s and move onto them.
  - A rebuild item that loses a race to an agent's submission fails; the
    leader sends the rebuild again, and it rebuilds the agent's version.
  - The run doesn't fork, and nothing an agent submitted is lost.
  - `retire` during a run would delete the versions agents hold. Their
    submissions would then answer `not_found` without `heads`.
- **Accepting.** The run is accepted when `audit?against=<spec ids>` is
  `clean`, after the leader has reviewed each link in `links`.
- **Afterwards**, the leader may retire and delete superseded versions.
  Deletes splice, so the run stays whole, and its links merge once they have
  been reviewed (*Decision 2*).
- **A create that answers `not_found`** for its `replaces` lost a race with a
  delete. The agent finds the run's heads with `audit?against=<its spec
  id>` and goes on as for a `conflict`.

### Docs

- **README, Replacing:**
  - *The link*: a link is set by a create that passed the comparison, or
    composed from two such links when the snapshot between them is deleted.
  - A new bullet: only a head can be replaced. Covers `conflict`, `heads`
    and `fork`.
- **README, Errors table:** `conflict`, 409.
- **README, Retention:** what a delete does to links.
- **README, Rebuilding:**
  - *What it rebuilds*: heads only, and support modules that become no
    snapshot.
  - *Levels*: imports only.
  - *Links*: one rule.
  - *Retiring*: only originals their copies replace.
  - *Running it yourself*: no `item` form.
  - The response's `replaced`, and that a run is rebuilt through its head's
    labels.
- **README, listings, delete and search:**
  - `heads=true`;
  - `with_replacers` and `deleted`;
  - that snapshot scopes in search cover heads only (*Snapshot search*).
- **README, Running agents on snapshots:** the new section.
- **README, spec audit:**
  - *The walk*: a deletion splices; drop a branch with `with_replacers`.
  - *Parameters*: audit against the same ids after a rebuild. A spec
    snapshot that was still a head was rebuilt like any head; with `retire`,
    find its copy in `created`.
- **Doc comments** on `RebuildItem::replaces` and `level`, the retire
  description in `pisa-router/src/snapshots.rs`, and `audit_against`'s
  comment on deleted snapshots.
- **The acceptance design** gets a one-line note at the top of the
  superseded sections, pointing here.

## Left out

- **Change ids** as a column. The spec snapshot's id now does that job.
- **Orphan reporting** (a head importing an obsolete entry's module) in
  listings or the audit. It's cheap to add later. With search showing heads
  only, agents rarely create orphans.
- **`heads=true` on the label audit.** The spec audit covers runs.
- **A bulk cleanup of superseded versions** after acceptance. Deletes one at
  a time splice correctly.
- **Search over superseded versions.** Add an opt-out if a use appears.
- **Linking to the newest version a copy still keeps**, when it can't keep
  its own original's statements. That is the fallback that would close the
  narrow case in *Decision 1*.
- **Undo and an operation log.**

## Verification

- **Rust:** `cargo test --workspace`, with Qdrant-backed tests against a
  throwaway Qdrant.
- **The two composition tests** (from this design's first discussion):
  - A skeleton and its proof on a base; an allowed definition edit in the
    proof (`f`), then a base that replaces the old one allowing `g`, and a
    retiring rebuild. The audit against S0 is clean, its head is P1′, and
    its one link lists `f` and `g`.
  - Same setup, but the new base changes `g`'s type, with `allow_changes`.
    P1′ has no link. The audit against S0 still has P1 as its head, with
    `retire` too, which keeps P1 and leaves it out of `retired`.
- **`rebuild.rs`:**
  - *A skeleton and its proof*: the plan holds P1 alone, and P1′ replaces
    P1. There is no fork without `retire`; with it, P1′ replaces S0 and S0
    stays.
  - *The proof whose skeleton isn't rebuilt*: P1′ replaces P1 with
    `allow: [g]`.
  - *The `allow_changes` tests*: P1′ replaces P1 (values only: `[g]`; out of
    reach: no `allow`) or has no link (statements change).
  - Remove *an item that replaces a failed one is skipped*: it can't happen
    any more.
  - New: a resent rebuild replays the item whose copy replaced it.
  - New, support modules:
    - an obsolete entry's module that a head imports is rebuilt as a
      support item;
    - after the run it has no snapshot, is kept by the copy that imports it,
      and isn't in `heads=true` or a label listing;
    - with the copy above it failed, the support module is gone too.
  - New: an agent replaces X after the plan is made. X′'s item fails with
    `conflict`, and sending the rebuild again rebuilds the agent's version.
  - Existing tests that fork on purpose pass `fork: true`.
  - New: a proof whose helper lemma's statement uses a changed type gets no
    link. This pins *Decision 1*'s narrow case.
  - New: *Decision 3*. S0 is in `p:2` and P1, which replaces it, in `p:1`.
    - A `p:2` rebuild plans nothing and lists S0 in `replaced`, with
      `replaced_by: [P1]`, on a dry run and a run.
    - A `p:1` rebuild lists nothing in `replaced`.
- **`spec_audit.rs`:**
  - *Spec over two snapshots*: audit against the original `[s_a, s_b]` after
    the retiring rebuild; `retired` is `[p_b]`.
  - *Allowed edit below*: one spliced link, as above.
  - *Skeleton outside the plan*: no fork without `retire`; after it, P1′
    replaces S0 with no `allow`.
  - *Rebuilt spec's own link*: unchanged.
- **Catalog, both backends:**
  - a middle delete re-points with composed links;
  - a root delete leaves a link to a deleted snapshot;
  - a fork moves down;
  - a create racing the delete of what it replaces is `not_found`.
- **Replacing only heads** (`replaces.rs`):
  - replacing a replaced snapshot answers `conflict` with the run's `heads`,
    and builds nothing (the fake worker sees no build);
  - `fork: true` forks;
  - a replacement landing during another's build makes the second's commit
    answer `conflict`;
  - a key replay still returns its snapshot;
  - `spec_audit.rs`'s *two proofs fork the run* passes `fork: true`.
- **Branch delete** (catalog and API): dropping one side of a fork leaves
  one replacer. Dropping a branch whose first snapshot was the only replacer
  makes the predecessor a head again. `deleted` lists every id, and a plain
  delete lists one.
- **Heads, catalog and API:** `GET /snapshots?heads=true` with labels and
  pages: a skeleton drops out once its proof lands, and comes back when the
  proof is deleted.
- **Heads, search** (`crates/pisa-search/tests/snapshots.rs`, against a throwaway Qdrant):
  - a skeleton's declarations stop matching a label scope and `"all"` once
    a proof replaces it, and match again after the proof is deleted;
  - a splice and a branch delete move `head` correctly;
  - `reconcile` sets `head` on existing points.
- **The change log:** a create with `replaces`, a splice and a branch delete
  each log both ends of the links they touch.
- **Unit:** the plan-time comparison against predicted declarations, for a
  chain with a rebuilt module between the item and `from`.
- **Clients:** regenerate `openapi.json` and `openapi.ts`; update
  `client.rs`, `client.test.ts`, the Python stub and `pisa-client`; run the
  web and Python suites.
- **End to end:** `scripts/e2e.py` doesn't use rebuild links. With
  `--search`, add one step: a skeleton's search hit disappears once its
  proof replaces it. Run it once against a rebuilt compose stack.
