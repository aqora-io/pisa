# Runs

This page is for whoever runs agents on snapshots: the leader that sets the
targets and judges the result, and the agents that submit proofs. It covers
the workflow, named runs, statuses, how a run is audited against its spec,
and how a spec is amended, a target refuted, and a run rebuilt and settled.
Snapshots themselves, and replacing one, are in [Snapshots](snapshots.md);
accepting a run with comparator is in [Accepting a run](acceptance.md).

A run starts from spec snapshots, skeletons whose statements are its
targets, and goes on through the snapshots that
[replace](snapshots.md#replacing-a-snapshot) them. Its heads are the
snapshots on it that nothing replaces.

## Running agents on snapshots

A leader hands targets to agents, and replacing keeps their submissions in
one run.

### The leader

The leader creates the spec snapshots:
- skeletons with `sorry` and one or a few targets each, with their
  [witnesses](#witnesses);
- with `verify: true` (see [Verifying](snapshots.md#verifying));
- where agents are to fill in definitions as well as proofs, with those
  definitions as [`holes`](snapshots.md#holes);
- with the run's [labels](snapshots.md#labels);
- each with a run name (see [Naming a run](#naming-a-run)), so that agents
  can import a lemma's run by name.

It holds the spec ids. They stay valid through
[rebuilds](#rebuilding-dependents) without `retire`.

### Definitions

A spec's targets usually state facts about definitions of its own, such as
a paper's objects. Put those definitions in a base snapshot of their own,
with no `sorry`, and have each spec import it.

A replacement must declare the spec module's own declarations itself (see
[Reusing a proof](#reusing-a-proof)), so a helper snapshot that needs a
definition declared in the spec can't be imported next to the replacement,
which declares it again. With the definitions below the spec:
- helpers import the base, never the spec;
- the replacement imports the base and the helpers, and re-declares only
  the targets;
- specs that build on another's definitions import its base too;
- a target proved from another spec's target imports that spec's run (see
  [Lemmas](#lemmas)).

```text
defs.A (definitions, proved)  ←  spec.A (targets, sorry)  ←  head of spec.A
      ↑                                                       ↑ imports
      └──────────────  helpers (lemmas, as named runs)  ──────┘
```

### Imports

Give a spec the imports its proofs will have. A replacement that imports
helpers built with `import Mathlib` sees all of Mathlib's instances. A
statement of the spec's whose instances Lean then finds by another path has
another type, and the replacement is refused although each file checks (see
[Canonical form](checking.md#the-canonical-form)). So a spec whose helpers will
import Mathlib imports it too, or helpers keep to the spec's imports.

### Agents

Each agent gets its target's spec id, and labels every snapshot with the
run's labels.
- **Scratch work** goes to `/check`, which keeps no snapshot. A check with
  `against` the spec says whether the code keeps the spec's statements
  before it is submitted (see
  [Checking against a spec](checking.md#checking-against-a-spec)).
- **To submit**, an agent creates its code with `replaces` the snapshot it
  last submitted for the target, or the spec snapshot at first.
- **Other targets.** It imports another target's run as
  `import PisaRun.<name>`, which builds on that run's head.
  [Search](search.md#snapshots-in-search) shows heads only, so what an agent
  imports from a hit is a head.

### On a conflict

A `conflict` (see [Replacing only a
head](snapshots.md#replacing-only-a-head)) means the run moved under the
agent: a rebuild copied its last version onto a new base, or another agent
extended it. Nothing was built. The agent takes a head from the answer's
`heads` and submits again, replacing that head:
- Code that imports runs by name is sent as it is, since each name resolves
  to its run's head again.
- Code that imports modules by hash moves onto the head's imports first,
  those of its `code` (see [Reading a
  snapshot](snapshots.md#reading-a-snapshot)).

### On a not_found

A `not_found` for its `replaces` means the create lost a race with a delete
(see [Retention](snapshots.md#retention)). The agent finds the run's heads
with `audit?against=<its spec id>` (see [Auditing a run against its
spec](#auditing-a-run-against-its-spec)) and goes on as for a `conflict`.

### Parallel attempts

Agents on one target pass `"fork": true` on their first submission and
extend their own branches. The leader keeps one and drops the others with
`DELETE /snapshots/{id}?with_replacers=true`, on the ids the audit's
[`forks`](#the-walk) names (see [Deleting a
branch](snapshots.md#deleting-a-branch)). Every fork in the audit is one of
these.

### Lemmas

A target proved with a lemma that another agent is still proving needs no
rebuild when that proof lands. The audit reads the target through the
lemma's run (see [Through links](#through-links)), and the leader
[settles](#settling-a-run) the run once, before accepting. Agents go on
meanwhile on the heads they hold.

Until then such a target is `sorry`. Its `own_status` says whether its own
proof is finished, and `inherits` names the lemma it waits for (see
[Statuses through other snapshots](#statuses-through-other-snapshots)).

### One target, several agents

A target too large for one agent splits through an interface: a helper
run, published first, that only defines the pieces' statements as `Prop`s
(`def PartA : Prop := …`).
- Each agent proves its piece, `theorem partA : PartA`, in a run of its own
  that imports the interface.
- One agent proves the target from `PartA → PartB → …`.

The interface is a definition base like any other. Agents import it and
leave it alone, and a change to it is a new head that the others move onto.

### Re-checks

When something below the run changes, such as a base edited, the leader
[rebuilds](#rebuilding-dependents) without `retire` while agents work:
- Agents meet the copies as `conflict`s and move onto them.
- A rebuild item that loses a race to an agent's submission fails. Send the
  rebuild again and it copies the agent's version.
- The run doesn't fork, and no submission is lost.

`retire` would delete versions agents hold, so their submissions would
answer `not_found` without `heads`.

### On a false target

An agent that finds a target false proves it: `theorem r : ¬ type_of% @t :=
…` on the spec, created with `refutes` (see [Refuting a
target](#refuting-a-target)). The audit shows the target refuted, and the
run isn't `clean`. The leader then [amends](#amending-a-spec) the spec, with
the reason, and resubmits the run onto the new spec.

### Accepting

Once the run is [settled](#settling-a-run), `POST /snapshots/accept` with
the run's spec ids accepts it when:
- its audit is `clean` (see [Auditing a run against its
  spec](#auditing-a-run-against-its-spec));
- and comparator, which shares no code with the audit, accepts every pair
  the audit compared (see [Accepting a run](acceptance.md)).

Pass in `allow` each definition the leader decided later may change, such
as one its own base edit changed; the spec's holes may change already. What
the agents' links allowed doesn't count. Before accepting, the leader reads
what each head changed in `comparisons` and `links`, and with `review=true`
what the targets' statements depend on, each hole with its value.

### Afterwards

The leader may retire and delete superseded versions. Deletes splice, so
the run stays whole, and its links merge once reviewed (see
[Retention](snapshots.md#retention)).

## Reusing a proof

The unit of reuse is a helper-only snapshot: a snapshot whose code declares
lemmas and definitions that other code may import, and none of a spec's
targets.

A replacement must declare the replaced module's own declarations itself.
So code that imports a snapshot declaring the spec's `t` is refused with
`change: missing`, even if that `t` is identical, and the refusal's
`imported_from` names the module (see [Refused
replacements](snapshots.md#refused-replacements)). An import can't stand in
for them because the comparison reads the replaced module's declarations in
the new module, and the spec is what that module states, not what it builds
on.

To reuse a retrieved complete proof:
1. Take the proof's helpers (everything but the spec's targets) into a
   snapshot of their own, and create it.
2. Import that snapshot in the code that replaces the spec's module.
3. State the targets again in that code, locally, and prove them with the
   helpers.

## Naming a run

A module's name is a hash of its code, so a proof that replaces a lemma is
a new module, and code that imports the lemma by hash would have to move. A
create that starts a run, one without `replaces`, may name it instead:
`"name": "projA.lemma1"`. Code may then `import PisaRun.projA.lemma1`, with
any of `public`, `meta` and `all`, and the server resolves it to the run's
head before anything runs:

```text
POST /snapshots {"code": "theorem lemma1 : 1 = 1 := sorry", "name": "projA.lemma1"}
POST /check {"code": "import PisaRun.projA.lemma1\n\ntheorem t : 1 = 1 := lemma1"}
  → {"ok": true, …, "runs": [{"name": "projA.lemma1", "module": "PisaSnap.s3c…", "snapshot_id": "…~9c0e…"}]}
```

Snapshots show the name of their run as `run`.

### Run names

- A name is dotted parts, each a letter or `_` then letters, digits and `_`,
  none a Lean keyword, up to 128 bytes.
- A run has one name, unique in its version and
  [scope](api.md#scopes), so dots give projects namespaces.
- `name` with `replaces`, or a malformed name, is a 400.
- A name another run has, while that run has snapshots, keeps nothing:
  `check.error` is a `conflict` that names that run's first snapshot. It has
  no `heads`, so `refused` is `request`: name the run otherwise, or amend
  that run's spec.

### Names and amendments

A create that [amends](#amending-a-spec) a named run's spec takes the run's
name for its own run, without sending `name`, and a resubmit carries the
run there. A `name` other than that one alongside is a `bad_request`, as a
run has one name.

### Naming a run later

- **`PUT /runs/{name}`** with `{"snapshot_id": …}` names the run that
  snapshot is in, from any of its snapshots. A name the run had goes. A name
  another run with snapshots has is a 409 `conflict`, a malformed name a
  400, and an unknown snapshot a 404.
- **`GET /runs/{name}`** reads a run by its name, on the request's
  `version` or as a listing picks one: its `root`, its `heads`, oldest
  first, and, when they are on one module, the `module` and `snapshot_id`
  (the newest head on it) the name resolves to now. A name no run has, or
  whose run has no snapshot left, is a 404.

Both answer `{name, root, heads, module?, snapshot_id?}`.

### How names resolve

Each `PisaRun.<name>` import becomes the module of the run's head, on the
same line, so positions don't move. A message on that import line is placed
on the line as sent. Answers list what each name resolved to in `runs`.

Errors:
- **A fork.** A run whose heads are on several modules is a `conflict` that
  names them, and says to drop all but one (see [Parallel
  attempts](#parallel-attempts)) or to import one by its module.
- **No run.** A name no run has, or whose run has no snapshot left, is a
  `not_found`.
- **Two versions of one run.** The resolved imports are checked as hash
  imports are, so a name and another version of its run by hash are a
  `bad_request` that says which module the name is.
- **Importing its own run.** A create into a run (`replaces`, forks too)
  that imports that run by name is a `bad_request`: it would import the head
  it replaces.

For `/check`, a batch and a create, these are in `check.error`, and a batch
resolves each header once. For sessions and documents they are HTTP
errors: a 409 for a fork, a 404 for no run and a 400 for the others.

### When names resolve

`/check` and creates resolve at the request. A session resolves when it
starts, and a document when it opens:
- The session's final check, and each revision of the document, import the
  same modules, and a name the code adds later resolves then.
- Their answers set a run's `moved_to` to its head when the run has moved
  to another module since.
- Once no snapshot is on the module a name was pinned to, they are a
  `not_found` that names the import.

### What a create keeps

A create's module is the code as resolved, so its hash, and all of pisa's
records, name modules by hash. The snapshot keeps the code as sent as
`source` (`GET /snapshots/{id}`), and what its names resolved to.
- A create with a `key` sent again as the same text resolves as the first
  did, and returns the first snapshot with its `runs`, whatever the run did
  since.
- Copies made by a rebuild or a settle keep `source`, its hash imports moved
  as the copy's are. A resubmit's copies keep the head's.

### When a name goes

A run keeps its name while it has snapshots, its first one deleted or not.
The name goes with the run's last snapshot.

## Witnesses

A target whose hypotheses can't hold together can be proved vacuously, and
an allowed definition change can make them so. By convention, a skeleton
states next to each such target a witness that its hypotheses can hold:

```lean
theorem t (x : Nat) (h : Pre x) : Post x := sorry
theorem t_nonvacuous : ∃ x, Pre x := sorry
```

`replaces` freezes the witness like any statement, so every replacement
keeps it, and it is a target of the [spec
audit](#auditing-a-run-against-its-spec) like `t`. If a change that `allow`
lets through makes `Pre` impossible, the witness can no longer be proved:
the audit shows it unproved, and the run isn't `clean`.

A witness is an ordinary target in every way, so it can be
[refuted](#refuting-a-target) like one: a proof of `¬ ∃ x, Pre x` shows the
spec's hypotheses can't hold.

## Existential targets

Where what an agent constructs isn't itself the result, such as an
algorithm that solves a stated problem, state the target as its existence,
and leave the construction to the proof:

```lean
theorem main : ∃ c : Circuit, Solves c problem := sorry
```

The statement names only the problem and the model. The agent defines its
candidate in its submission, which may add declarations, and nothing the
spec defines changes: no hole, no `allow`, so `clean` means what it says.

Keep [holes](snapshots.md#holes) for a spec whose answer is the value, such
as a cost a run minimises, and read their values in the audit's [review
list](#the-review-list).

## Statuses and assumptions

An `axiom` declared in snapshot code, or in the code being checked, is an
assumption: a statement taken as given, such as a lemma from the
literature. Each declaration in a check result has a `status`, and
`assumes` lists the assumptions it uses (with `detail: ["declarations"]`;
the summary's `axioms` show them too):

| `status` | Axioms it uses besides the allowed ones (`propext`, `Classical.choice`, `Quot.sound`) |
| --- | --- |
| `proved` | None |
| `conditional` | Assumptions only |
| `sorry` | `sorryAx`, with or without assumptions |
| `rejected` | Any other: the one `native_decide` declares, `Lean.trustCompiler`, a library's |

A snapshot's `status`, on creates, listings and `GET /snapshots/{id}`, is
the worst of its module's declarations', in that order. Its check is `ok`
only if that is `proved`.

### Statuses through other snapshots

A declaration on a snapshot gets axioms through the theorems of it that it
uses: a finished proof of a lemma on a sorried skeleton is `sorry`. Checks
and creates on snapshots list those theorems in `inherits` (`{name,
module}`), and say in `own_status` what its own axioms make it: `proved`
for that finished proof. An audit reads such a status through the run that
proves the theorem (see [Through links](#through-links)).

### Auditing by label

`GET /snapshots/audit?label=project:foo` says what the snapshots with the
labels rest on, from their records, without running Lean:
- **`summary`** counts their declarations by status, each module's once,
  without the assumptions themselves and what Lean generates.
  `unverified_modules` counts the modules not yet re-checked by the kernel,
  whose statuses come from the build's report.
- **`assumptions`** lists each assumption they use, wherever it is declared,
  as `{name, signature?, snapshot_id?, verified, used_by}`: a snapshot of
  its module, whether that module is verified, and the declarations that
  rest on it.
- **`not_proved`** lists the declarations that are `sorry` or `rejected`,
  each `{name, snapshot_id, status, axioms}`, with a snapshot of its module
  and its axioms outside the allowed list.

A trusted audit has no `rejected` or `sorry` and no `unverified_modules`.
Whether each assumption is reasonable is for review: `axiom oops : False`
makes everything above it `conditional`.

It takes `version` as a listing does (see [Who
answers](snapshots.md#who-answers)).

## Auditing a run against its spec

`GET /snapshots/audit?against=<id>`, repeated for a spec spread over
several snapshots, audits a run from its records:

```text
GET /snapshots/audit?against=v4.36.0-54500a67~9c0e…
  → {"summary": {"proved": 1, "conditional": 0, "sorry": 0, "rejected": 0, "unverified_modules": 0},
     "assumptions": [], "not_proved": [], "clean": true, "heads": ["v4.36.0-54500a67~4b7a…"],
     "targets": [{"name": "t", "spec": "v4.36.0-54500a67~9c0e…", "head": "v4.36.0-54500a67~4b7a…",
                  "status": "proved", "verified": true, "accepted": true}],
     "comparisons": [{"spec": "v4.36.0-54500a67~9c0e…", "head": "v4.36.0-54500a67~4b7a…",
                      "replacement": {"filled": ["t"]}}]}
```

### The walk

From each spec snapshot the audit follows the snapshots that replace it,
whatever their labels, to the run's `heads`, which nothing replaces.
- **Forks.** A snapshot replaced more than once is a fork, listed in
  `forks` as `{snapshot_id, replaced_by}`. There is no merge, so delete all
  but one branch.
- **Deleted snapshots.** A deleted snapshot on the way doesn't cut the run,
  as what replaced it now replaces what it replaced. Deleting a head makes
  what it replaced a head again, unless something else replaces it.
- **Dropping a branch.** `DELETE /snapshots/{id}?with_replacers=true` on
  the snapshot `forks` names for a losing branch takes the branch with it,
  and `deleted` lists what went. A plain delete of that snapshot moves the
  rest of the branch down, and the fork stays.

### Targets

Each declaration of a spec snapshot's module, other than its assumptions
and what Lean generated, is a target. `targets` has a row for each target
and each head reached from its spec snapshot, `{name, spec, head, status,
verified, accepted, refuted_by?, contradicted?, linked_status?, through?,
unlinked?}`, with:
- its `status` there;
- whether the head is `verified`;
- whether the target is `accepted` there (see [When a run is
  clean](#when-a-run-is-clean));
- what refutes it (see [Amendments and
  refutations](#amendments-and-refutations)), and how it reads through
  other runs (see [Through links](#through-links)).

### Comparisons

The audit doesn't take a link's word that the head keeps the spec's
statements: that word is what a create's build reported, and the code being
judged runs in that build.

For each spec snapshot and each head it reaches, `comparisons` has the
comparison that a create replacing the spec snapshot with the head's code
would run (see [Replacing a snapshot](snapshots.md#replacing-a-snapshot)).
It runs on their records once the kernel has re-checked both, allowing only
the definitions the leader names to change. Each is `{spec, head, reasons?,
changes?, replacement?}`:
- `reasons` and `changes` if the head doesn't keep the spec's statements,
  as a refused create says them;
- else `replacement`, what it filled and what allowed changes it made.

For review, these are what the head changed, whatever its links say.

### What may change

The heads may change:
- a spec snapshot's [holes](snapshots.md#holes);
- the definitions `allow=<name>`, repeated, names: those the leader lets
  the heads change, as `allow` names them in `replaces` (`_private.h` for a
  private one; an inductive type for its constructors and what Lean derives
  from it), such as one its own base edit changed.

A link's own `allow` is what its create asked for, written by whoever
submitted it, so it doesn't count: a head that changed anything else is
shown with `reasons` and isn't `clean`. A statement only names the
definitions it uses, so redefining one would change what it says.

### When a run is clean

`clean` is true when:
- there is no fork;
- no spec snapshot was copied from one that is gone (see
  [Parameters](#parameters));
- every comparison found the spec's statements kept, with no definition
  changed but its holes and those `allow` names (a spec of assumptions only
  has no targets, but statements all the same);
- and every target is `accepted` on its head.

A target is `accepted` when:
- the spec snapshot and the head are verified;
- their comparison found the spec's statements kept;
- nothing [refutes](#refuting-a-target) the target (its `refuted_by`);
- no amendment supersedes the spec;
- and the target is `proved`, or `conditional` only on assumptions the spec
  permits: those declared in a spec snapshot's module or in a module it
  builds on. Each of `assumptions` says whether the spec permits it
  (`in_spec`).

The records of a module the kernel hasn't re-checked come from its build's
report, which the code being judged could influence, so they don't count.
`unverified` lists those spec snapshots and heads, and their comparison's
`reasons` say to verify each with `POST /snapshots/{id}/verify`.

### Amendments and refutations

- **`amendments`** lists the [amendments](#amending-a-spec) of the spec
  snapshots, both those they make and those made of them.
- **`superseded`** lists each spec snapshot an amendment supersedes, as
  `{spec, by}`, with `by` the spec now current. Any one makes the run not
  `clean`: it is judged against the current spec.
- **`refutations`** lists the snapshots that [refute](#refuting-a-target) a
  spec snapshot's target, or a target of a spec it amends that it states the
  same. Any one makes the run not `clean`.
- **`contradicted`** marks a target that is refuted, and yet `proved` or
  `conditional` on a head: the spec's assumptions contradict each other.

### Links

For review, `links` lists every link that let definitions change, with an
`allow` or by filling holes:
- those on the way to the heads;
- those of each spec snapshot's own chain back through what it replaced, as
  far as those snapshots exist, such as a rebuilt spec's link to the
  original, which allows what `to` changed under it.

Each is `{snapshot_id, replaces: {snapshot_id, allow}, replacement:
{filled, changed_definitions?, affected?}}`: the link as the snapshot's own
`replaces` shows it, and what its comparison found, its create's or the
union of two if the snapshot between was deleted. Those comparisons are the
builds' reports; `comparisons` holds what the audit found from verified
records.

### The review list

With `review=true`, `review` lists, for each spec snapshot and head it
reaches, what a reviewer reads to know what the spec's statements say
there: `{spec, head, declarations, library}`.

It starts from the spec's declarations and follows, on the head, each
declaration's type and the values of definitions and inductive types,
through the head and the snapshots it builds on. It doesn't follow
theorems' proofs, so a candidate built inside a proof adds only its
statement, and axioms are leaves.

`declarations` lists the spec's, then those new in the head, then those of
the snapshots below, module by module. Each is `{name, kind, origin,
snapshot_id?, signature?, type?, hole?, value?, unprinted?, holes?}`:
- **`origin`** is `spec` (the spec's own, as the head keeps them), `head`
  (new in the head, such as what a hole's value now uses), or `snapshot` (a
  snapshot below, with its `snapshot_id`).
- **`signature`** has notation, and **`type`** has full names and no
  notation.
- **`hole` and `value`.** A hole, or a definition `allow` names, has `hole`
  and its `value` on the head: printed with full names and no notation,
  which the code can't dress up, each `sorry` as `sorry`, and cut at 4,000
  characters. So has each definition the head adds that a hole's value
  reaches: its own, a non-public one of a module-system file included, or
  one of a snapshot it builds on that the spec doesn't, as the value can
  hide behind one.
- A hole may be a definition of a snapshot the spec builds on, and is
  followed wherever the head declares it.
- **`unprinted`** says why a value or a signature is missing: it didn't
  print, its module has no export, or the export doesn't have it.
- **`holes`** names the other holes a value mentions, to read with it.

What Lean derives of a declaration, such as a recursor or `casesOn`, isn't
listed. `library` names the constants of Lean, Mathlib and the like that the
declarations use, which the version fixes.

The list holds at most `review_limit` declarations over all its entries, 1
to 10,000 and 1,000 unless given, in their order. A value outside that range
is a 400. `review_left_out` counts the declarations past the cap, and is
absent when there are none.

The text is what the builds printed, for reading. `comparisons`, from
verified records, is what acceptance rests on.

### What to look for in a review

A proof can be valid and still miss the claim:
- a budget defined as the candidate's own cost, so `cost ≤ budget` holds
  for any candidate;
- an acceptance condition set to `True`;
- an input promise narrowed until the problem is trivial;
- an operation added to the model as a native gate.

Made to a definition that isn't a hole, each is a change the comparison
refuses, and the run isn't `clean`. Made to a hole, it is allowed, and only
a reader catches it. So read each hole's value, and prefer [existential
targets](#existential-targets) where the value isn't the result.

### Through links

A head's records are what its build found. A theorem proved with a lemma of
another snapshot, where the lemma was `sorry`, depends on `sorryAx` through
it. It still does once another snapshot proves the lemma, until the head is
rebuilt onto that proof.

So the audit reads such a theorem through the lemma's run. When the run has
one head, verified, that proves the lemma with the same statement, and
changed no definition there but those that were `sorry`, the theorem
depends on what that head's lemma does, recursively:
- **`linked_status`** is the target's status read so, when it differs from
  `status`.
- **`through`** lists the lemmas it was read through, each `{name,
  snapshot_id, head}`.

A proof can be swapped for any other of the same statement, so this is the
kernel's claim. `accepted` and `clean` still rest on `status`, the records,
until the run is [settled](#settling-a-run). Where a condition fails, or
the lemmas justify each other, the lemma counts with the axioms its records
name.

The audit says what stops a target from being read so, and what a settle
would do:
- **`unlinked`** says, for a target that isn't `proved`, why each lemma it
  inherits through wasn't read so, as `{name, module, reason}`: nothing
  replaces its snapshot yet, its run reaches more than one head, the lemma's
  snapshot or its run's head isn't verified, that head doesn't keep the
  lemma's statements, changes a definition that wasn't `sorry` or doesn't
  build on all the lemma's module builds on, or the lemma is refuted. On a
  target's own head that isn't verified, each lemma's reason is that.
- **`unsettled`** lists the heads with such a target, each `{head, swaps,
  blockers?}`. `swaps` are the snapshots whose modules the head builds on,
  each `{snapshot_id, head}` with the head of its run that a settle builds
  on instead.
- **`blockers`** say why a settle can't rebuild the head as it is:
  - a swap's head that needs settling too, on a run whose spec isn't in
    `against` (add it);
  - a name a swap brings in that the head declares too;
  - a statement that uses a private name of a swapped module;
  - a spec statement that uses a definition the swap fills, which `allow`
    doesn't name.

### Counts

`summary`, `assumptions` and `not_proved` are those of the heads, each
standing for its module.

### Parameters

- **Versions.** The ids are of one version, which the audit uses. Ids of
  several versions are a 400 that names them, and so is a `version` that
  isn't theirs.
- **Combinations.** `label` doesn't combine with `against` (400). Neither
  do `allow`, which can't be empty either, and `review`, which is `true` or
  `false`: without `against` they are 400s.
- **Missing specs.** A spec snapshot that doesn't exist is a 404.
- **After a rebuild.** A [rebuild](#rebuilding-dependents) copies only a
  run's heads, so audit against the same ids after it.
- **Rebuilt specs.** A spec snapshot that was still a head, with nothing
  replacing it yet, is copied like any head, and with `retire` its old id
  is deleted if its copy replaces it. Its copy in `created` then replaces a
  snapshot that is gone, so nothing shows that it states what the original
  did, and an audit against it isn't `clean`. Rebuild a run's spec snapshots
  without `retire`.

## Amending a spec

A spec can be wrong: a target false as stated, a hypothesis missing, a
hole that should be a definition. A leader fixes it during a run by
creating the corrected spec with `amends: {snapshot_id, reason}`: the spec
it amends, and why, in 1 to 4,096 characters, not only whitespace.

### The new spec

- It is a spec of its own that replaces nothing: `replaces` with `amends`
  is a 400.
- It keeps the amended spec's `holes` unless it names its own, and
  `holes: []` leaves none.
- It is always re-checked through the kernel: `verify: false` is a 400.
- It takes the amended run's name, if it has one (see [Names and
  amendments](#names-and-amendments)).

### The record

Its create compares it with the amended spec as a replacement that may
change nothing would be compared (see [Replacing a
snapshot](snapshots.md#replacing-a-snapshot)), holes included. It records:
- what changed, as `changes`, as a refused replacement's;
- the declarations it adds, as `added`;
- the reason;
- the holes before and after, as `holes: {before, after}`, when they
  differ.

The response's `amendment` has it, as `{snapshot_id, amends, reason,
changes, added, holes?}`, and an audit against either spec lists it in
`amendments`. Deleting the new spec deletes the record; deleting the
amended one keeps it.

### Superseding the amended spec

The current spec is the amendment, followed through the amendments made of
it:
- An audit against an amended spec says so in `superseded`, and isn't
  `clean`.
- A create that replaces into its run, a fork included, is a `conflict`
  whose message names the current spec and whose `heads` are that spec's
  run's heads, so `refused` is `conflict`.
- A create that was building when the spec was amended is refused the same
  way, and its build is kept.

### Amending once

A spec is amended once. A second amendment of it is a `conflict` in
`check.error` that names the current spec, which is the one to amend. It
has no `heads`, so `refused` is `request`. Two amendments that race are
refused the same way.

### Carrying the run over

`POST /snapshots/resubmit {from, to}`, with `to` a spec that amends `from`,
compares each head of `from`'s run with `to`, as an audit against `to`
would, allowing `to`'s holes to change. A `to` that doesn't amend `from`, or
is of another version, is a 400.

The request is `{from, to, dry_run?, labels?, verify?, timeout?}`, and the
response `{ok, items}`, the heads oldest first, each `{snapshot_id, module,
key, passes, reasons?, changes?, replacement?, new_snapshot_id?, check?}`:
- **A head that keeps `to`'s statements** passes, and is created again: the
  same code, so the same module, replacing `to`, with its labels or the
  request's `labels`. Its `new_snapshot_id` is the new snapshot, and its
  `check` the create's when it kept nothing.
- **Forks.** A head comes over forked once `to` has a replacer, an earlier
  head's or another create's, so the heads of a forked run come over
  forked.
- **A head that changes one of `to`'s statements** is listed with its
  `reasons` and `changes`, and one whose record isn't verified with that
  reason. Neither is created.
- **`dry_run`** only compares.
- **Sending it again.** Each create has a key of the resubmit's, so sending
  it again replays what is done. `from` may be deleted by then.
- **Async.** A run takes `Prefer: respond-async`, as a job of kind
  `resubmit_snapshots`. A dry run answers at once.

`ok` says whether every head passes and, for a run, was created. Agents
then work on `to`'s run, and the leader audits and accepts against `to`.

## Refuting a target

A run can show that a target is false, and a snapshot records it: a create
with `refutes: {snapshot_id, target}`, the spec snapshot and its target.

### What a refutation must be

- The code imports the spec's module and proves, with no `sorry` and no
  assumption, a theorem whose statement is `¬` the target's statement
  exactly: `theorem r : ¬ type_of% @t := …` states it so.
- The theorem's `negates` is the hash of that statement's canonical text,
  every name written whole, which equals the target's `type_hash`. A
  universe parameter counts by its position, and a private name of the
  refuter never reads as the spec's.
- It is always re-checked through the kernel: `verify: false` is a 400.
- It is a snapshot of its own: `replaces`, `holes` and `amends` are 400s.

### What can't be refuted

These are a `bad_request` in `check.error`:
- a `target` that isn't a theorem the spec states;
- a target whose statement uses one of the spec's holes, which a head may
  fill, directly or through a definition of the spec's or of a snapshot it
  imports: amend the spec instead;
- a target whose statement uses a private name of the spec, which no other
  module can state.

### Refused refutations

Otherwise, when the code doesn't refute the target, nothing is kept, and
`check.reasons` says why: the code doesn't build on the spec, it isn't
verified, no theorem states `¬` the target exactly, or the one that does
isn't proved.

### The record of a refutation

- The response's `refutation` names the theorem, as `{snapshot_id, spec,
  target, theorem}`.
- An audit against the spec lists it in `refutations`, marks the target
  `refuted_by` the snapshot and not `accepted`, and isn't `clean`, so
  neither is an acceptance.
- A create that replaces into the run answers the run's `refutations` too,
  so the agents on it learn the target is false.
- The record outlives the refuting snapshot. Once that is deleted, the
  audit marks it `gone`, and it still counts.

### The way on

The way on is to [amend](#amending-a-spec) the spec. A spec that amends the
refuted one and states the target the same, by its statement's hash, is
shown the refutation, under the spec and target it was made against.

## Rebuilding dependents

A module's name is a hash of its code, imports included, so replacing B
with B′ leaves every chunk above B importing B.

`GET /snapshots/{id}/dependents?label=project:foo` lists the snapshots of
every module that imports the snapshot's module, directly or through
others, parents first: by level (1 imports it, 2 imports a level 1, and so
on, by the longest chain), then oldest first. With `label` filters, as in a
listing, it lists only the snapshots with every label.

`POST /snapshots/rebuild` copies their heads onto the replacement, or with
`"dry_run": true` only plans how:

```text
POST /snapshots/rebuild {"from": "…~9c0e…", "to": "…~4b7a…", "labels": {"project": "foo"}, "dry_run": true}
  → {"ok": true, "changes": [], "plan": [
       {"level": 1, "snapshot_id": "…", "module": "PisaSnap.sC…", "new_module": "PisaSnap.sC′…",
        "new_header": "…", "code": "…", "labels": {…}, "key": "rebuild:…"}, …]}
```

The request is `{from, to, labels, dry_run?, allow_changes?, retire?,
verify?, timeout?}`. `from` and `to` are of one version, which the rebuild
runs on; two versions are a 400.

### What a rebuild copies

- **Heads with the labels.** It rebuilds the dependents with a head that has
  every label, and the modules they build on above `from` (see [Support
  modules](#support-modules)). `labels` is required, so the caller chooses
  which projects follow the edit: none is a 400.
- **Heads only.** A head is a snapshot that nothing replaces, with the
  labels or without. A replaced one is history, and its run goes on through
  what replaced it. A snapshot that only its copies from this same rebuild
  replace stays an item, so sending the rebuild again replays them.
- **The code.** Each item's `code` is the old code with its imports of
  `from`'s module, and of the other modules the plan rebuilds, swapped for
  the new ones. Lines and columns stay where they were.

Each plan item is `{level, snapshot_id?, module, new_module, new_header,
code, labels, key, replaces?, affected?, source?}`.

### Support modules

A module that is rebuilt only because a copy builds on it, such as one
between the heads and `from`, or an obsolete snapshot's that a head still
imports, is a support item: no `snapshot_id`, no `labels` and no
`replaces`.
- The rebuild creates it so that the items above can build, then deletes
  its snapshot once every level has run.
- Its module stays only while a created snapshot builds on it, and is in no
  listing, search or audit, so old code doesn't come back as a head. One
  whose dependents all failed goes with its snapshot.
- If the server stops mid-run, the support snapshots stay until the rebuild
  is sent again, which replays and deletes them.
- A delete that fails, of a support snapshot or, with `retire`, an old one,
  doesn't lose the answer: it is in `not_deleted` as `{snapshot_id, error}`,
  `ok` is false, and sending the rebuild again deletes it.

### Runs left out

Neither of these changes `ok`, and both are listed on dry runs and runs
alike:
- **Runs that left the labels.** A snapshot with the labels that one
  without them replaces isn't copied, as its run went on elsewhere.
  `replaced` lists it as `{snapshot_id, replaced_by}`, with those replacers.
  Rebuild that run with its heads' labels.
- **Runs whose spec was amended.** A head of such a run isn't copied, as
  the run takes no more snapshots. `superseded` lists it as `{snapshot_id,
  by}`, with the spec now current.

### Levels

An item's `level` is the wave its create runs in: one more than the highest
among the items whose modules it imports, so 1 for one that imports only
`from`'s module.

### Running a rebuild

Without `dry_run`, the server runs the plan:
- a level at a time, up to 8 of a level's creates side by side once the
  level below is done;
- each create with the item's `code`, `labels`, `key` and `replaces`, and
  the request's `verify` and `timeout`;
- a create no worker takes in time is sent again, up to 5 times.

The response adds what it did:
- `created`: each old `snapshot_id` and `module` with its `new_snapshot_id`
  and `new_module`; a support item has the modules only;
- `failed`: each with the create's `check`;
- `skipped`: the items that build on one that failed or was skipped. Other
  branches go on.

`ok` says whether every item was created, and everything the rebuild was to
delete deleted.

### Retiring

With `"retire": true`, once every item is created, the server deletes the
old snapshots whose copies replace them, from the top down, and with them
the old modules nothing else needs. `retired` lists the snapshots' ids.
- Each delete splices the run (see [Retention](snapshots.md#retention)), so
  a skeleton S0 and its proof P1, with P1 copied as P1′, read S0 ← P1′.
- An old snapshot whose copy has no link stays, and its run with it.
- `from` stays: when `to` replaces it, it is part of the run's history,
  such as the skeleton a proof replaced. Delete it yourself if nothing needs
  it.

### Sending a rebuild again

The keys come from the plan, so sending the same rebuild again, after a
failure, a timeout or a lost server, replays the creates that are done and
runs the rest. An item is a create without `fork`. If an agent replaced its
snapshot after the plan was made, the item fails with a `conflict`, and
sending the rebuild again plans the agent's head instead.

### Later edits

A rebuild follows the run's heads. After one from `from` to `to` without
`retire`, the originals are replaced by their copies on `to`. So a later
edit is rebuilt from `to`, the base the heads now build on: a rebuild from
`from` would find nothing left to copy.

### Async rebuilds

A run can take minutes, so it takes `Prefer: respond-async` (see [Async
requests](api.md#async-requests-and-jobs)). The 202's job, of kind
`rebuild_snapshots`, is run by the server that answered, and
`GET /jobs/{id}` on any server answers with the response once it is done.
- If that server stops first, the job reads as lost (503 `worker_lost`,
  `resend_safe`): send the rebuild again.
- `DELETE /jobs/{id}` leaves a running rebuild running.
- A dry run answers at once.

### Running the plan yourself

Or take the plan and send each item as `POST /snapshots {code, labels, key,
replaces?}`, a level at a time, the same way. Each create's
`snapshot.module` is the item's `new_module`. A support item has no labels:
once the items above it are created, delete its snapshot with
`DELETE /snapshots/{id}`.

### Statements and changed definitions

- **Statements.** If `to` replaces `from` (see [Replacing a
  snapshot](snapshots.md#replacing-a-snapshot)), its create compared them,
  so `changes` is empty. Otherwise the rebuild compares them the same way.
  If `to` changes what `from` states, it answers `ok: false` with the
  `changes` and no plan, unless `allow_changes` is set.
- **Changed definitions.** When `to` changes a definition's value (one its
  `allow` names, or with `allow_changes`), `changed_definitions` lists them.
  Each item lists, as `affected`, its theorems whose type uses one, directly
  or through definitions here or below. That is read from what the modules'
  declarations use.

### How copies link

Each copy replaces the snapshot it copies, so a run's history carries over,
and a run's spec snapshots keep their ids.

Before anything is built, the plan runs the comparison the copy's create
will run (see [Replacing a snapshot](snapshots.md#replacing-a-snapshot)).
It takes the copy and the new modules it builds on to declare what their
originals do, and every definition to be allowed to change its value:
- **If it passes,** the item's `replaces` is `{"snapshot_id": …,
  "allow"?}`, where `allow` names the definitions it found changed, such as
  one of `to`'s that the copy's statements use. With none, there is no
  `allow`.
- **If it fails,** as when `to` changes the type of something the copy's
  statements use, the item has no `replaces`: a changed statement is a new
  spec, for a person to set. The copy then belongs to no run, and the old
  run stays as it was, even with `retire`. To start a new run, create the
  skeleton again on the new base, then the copy's code replacing it.

### Rebuild limits

A plan has at most 10,000 items, carrying at most 64 MiB of code in all.
Past either, the rebuild is a 400 that asks to narrow its labels.

## Settling a run

`POST /snapshots/settle` rebuilds the heads the audit lists in `unsettled`
onto the heads they are read through (see [Through
links](#through-links)), in one plan, after which their targets hold from
their own records:

```text
POST /snapshots/settle {"against": ["…~9c0e…", "…~5d1f…"], "dry_run": true}
  → {"ok": true, "changes": [], "plan": [
       {"level": 1, "snapshot_id": "…~4b7a…", "module": "PisaSnap.sT…", "new_module": "PisaSnap.sT′…",
        "new_header": "…", "code": "…", "labels": {…}, "key": "settle:…", "replaces": {…}}, …]}
```

### The settle request

The request is `{against, allow?, dry_run?, verify?, timeout?}`:
- `against` and `allow` are as for the [audit](#auditing-a-run-against-its-spec),
  whose `unsettled` the settle plans from. `against` of several versions,
  or empty, is a 400.
- `verify` and `timeout` are for the creates.

Only the heads of the runs `against` reaches are rebuilt.

### The settle plan

- Each item copies a head, its imports of each swapped module replaced by
  the swap's head, as a [rebuild](#rebuilding-dependents) copies it, and
  replaces it, allowing what the swaps changed.
- A module between a head and a swapped one is a support item. A swapped
  module is never rebuilt.
- A swap's head that is itself unsettled is copied first, and the heads
  that build on it build on its copy.
- Keys are `settle:<snapshot or module>:<new module>`, so sending the
  settle again replays what is done.

### When a settle refuses

- Nothing to settle is an empty plan, `ok: true`. The audit's `unlinked`
  says why a target that isn't `proved` has nothing to settle.
- A head with `blockers`, or swaps that would make a cycle, refuse the
  whole plan: `ok: false`, and `changes` says why, one line each.

### Running a settle

A settle runs, and answers, as a rebuild does, with `created`, `failed` and
`skipped`. It takes `Prefer: respond-async`, as a job of kind
`rebuild_snapshots`.

The kernel's claim covers proofs, not elaboration. A head's tactics can see
what a swap brings in besides the lemma, such as `simp` lemmas, instances
or notation, and fail. The item then fails with its create's `check`, and
the head stays as it was.

### After a settle

Audit against the same ids. The targets are read from the copies' records,
so `status` is what `linked_status` was, and `clean` follows.
