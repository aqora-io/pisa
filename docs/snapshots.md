# Snapshots

This page is for formalizers and agents that build a Lean library chunk by
chunk on pisa, and for whoever runs the store that keeps it. It covers what
a snapshot is; how to create, label, list, replace, verify and delete one;
and what pisa keeps and what it trusts. Runs of agents on snapshots, and
their audits, are in [Runs](runs.md).

A formalizer that builds a library statement by statement would otherwise
resend, and Lean re-check, the whole library on every compile. Instead it
keeps each accepted chunk as a snapshot and builds on it:

```text
POST /snapshots {"code": "def double (n : Nat) := 2 * n", "labels": {"run": "7"}}
  → {"snapshot": {"id": "v4.36.0-54500a67~9c0e…", "version": "v4.36.0-54500a67", "module": "PisaSnap.s1f…",
                  "header": "import PisaSnap.s1f…", "imports": [], "labels": {"run": "7"},
                  "status": "proved", "verified": true, "created": 1762387200},
     "check": {"ok": true, …}, "cached": false, …}
POST /check {"code": "import PisaSnap.s1f…\n\nexample : double 2 = 4 := rfl"}
```

The examples on this page need no Mathlib. Ids carry their version: on a
`PROJECT=lean-core` stack they start `v4.36.0-ca45ccf8~`.

## Snapshots and modules

A snapshot has two names:
- **The module** (`module`, e.g. `PisaSnap.s1f…`) is what code imports. It
  comes from a hash of the code, so the same code is the same module, and
  its check, `status` and `verified` belong to it.
- **The id** (`id`, `<version>~<32 hex>`) is the snapshot's own: a
  reference to the module with its own `labels` and `created` time.

Creating the same code again adds another snapshot of the same module
without building it again. Such a create answers `cached: true`: its `check`
is the stored build's, with the build's `time` and `timing`, and the
response's own `timing` is what this request cost.

`GET`, `PATCH`, `DELETE` and `verify` take the id, which carries the
version, so they take no `version`. A module name there is a `bad_request`.

A snapshot also shows `replaces` and `holes` when it has them (see
[Replacing a snapshot](#replacing-a-snapshot)), and `run`, the name of its
run when the run has one (see [Naming a run](runs.md#naming-a-run)).

## When a create keeps nothing

A create that keeps nothing has no `snapshot`. `refused` says what to do
next, and its `check` why:

| `refused` | What to do |
| --- | --- |
| `proof` | Change the code. Lean or the kernel rejected it, the policy refused it, its header failed, it changed a statement of what it replaces (`check.changes`), or it doesn't refute its target. |
| `limit` | It ran out of time or memory, or crashed Lean. It may be right: give it more `timeout`, or make it cheaper. |
| `request` | Change the request. It breaks a rule (`bad_request`), names what doesn't exist in its scope (`not_found`), or conflicts in a way no resend fixes (a `conflict` with no `heads`), as a run name another run has. |
| `conflict` | What it replaces is replaced already. Send the same code again replacing one of `heads`. |
| `retry` | Nothing is known to be wrong. No worker took it, or one failed: send it again later, with the same `key`. |

Code with `sorry` is kept: a refusal is never about what isn't proved yet.

A create is also refused when:
- **Kept code fails the re-check.** Code that is already kept but fails the
  re-check that `verify: true` asks for is refused, as new code that fails
  the re-check isn't kept. `check.error` has the reason.
- **Lean reads the header differently.** A header whose imports Lean reads
  differently from its lines, such as an import after a block comment on the
  same line, is a `bad_request` that asks for each import on its own line:
  the record would miss what the module builds on.
- **Two imports declare one name.** A header that imports two snapshots
  declaring the same name is a `bad_request` that names a snapshot of each.
  Lean loads both when the name is a theorem of one statement in each, as a
  skeleton and its fill are, and the code then uses whichever comes first
  in its imports, so what it proves would hang on their order. When the two
  are versions of one run, the error names the run's head to import
  instead. When both declare it alike, as when one file's code is pasted
  into each, it says to make that code a snapshot of its own (see
  [Several files](#several-files)). `/check` answers such a header with the
  same `bad_request` in `error`, and opening a document or starting a
  session on it is a 400.

## Who answers

The servers read the catalog themselves. `GET` of a snapshot, listings,
dependents, label edits, audits and their review lists, and the plans of
rebuilds, settles and resubmits need no worker. They answer at once even
while builds keep every Lean slot busy, and even when no worker of the
version is live.

Creates, deletes, verifies, exports and comparator runs go to a worker.

A listing or a label audit that names no `version` reads the server's
default (`PISA_DEFAULT_VERSION`), or else the one version the live workers
serve. When there is no default and the live workers serve several
versions, or none, it is a 400 that asks for a `version`.

The servers take `PISA_ALLOWED_AXIOMS` as the workers do, for what an audit
lists of a declaration that isn't proved. Set it on both, or on neither (see
[Configuration](configuration.md)).

## Reading a snapshot

`GET /snapshots/{id}` answers the snapshot with:
- `code`, the module's code;
- `source`, the code as its create sent it, when it imported runs by name:
  `code` imports the heads those names resolved to in their place (see
  [Naming a run](runs.md#naming-a-run));
- `check`, the module's check, with the kernel re-check's axioms once it is
  verified. Its declarations are summaries unless the request has
  `?detail=declarations`;
- `exports`, the declarations that code importing it sees, in order.

Each export is `{name, kind, signature?, error?, docstring?, description?}`,
with `error` in place of `signature` for one that didn't print.
`description` is `{title, text, latex?, aliases, source}`. Search writes it
a little after the create, when it has a describe model
(`PISA_SEARCH_DESCRIBE_MODEL`; see
[what snapshot search sends to a model](search.md#what-snapshot-search-sends-to-a-model)).

## Labels

A snapshot's labels are string key/value pairs:
- at most 64 of them;
- a key is 1–128 bytes of letters, digits, `.`, `_`, `/` and `-`;
- a value is up to 512 bytes of UTF-8 without NUL characters.

Labels that break these rules are a `bad_request`: in `check.error` on a
create, which then keeps nothing, and a 400 on a `PATCH`, which then changes
nothing. A request's scope adds its own labels to what it creates (see
[Scopes](api.md#scopes)).

### Editing labels

`PATCH /snapshots/{id}` with `{"labels": {"stage": "done", "draft": null}}`
is a JSON merge patch. It sets `stage`, removes `draft`, keeps the other
labels, and returns the snapshot as a listing shows it.

### Filtering a listing

`GET /snapshots?label=project:foo&label=kind:lemma` lists the snapshots
that have every label given:
- Each `label` is an exact match of `key:value`, split on the first `:`, so
  a value can contain `:`.
- Without `label` it lists every snapshot of the version.
- There is no OR or prefix matching. Two values for the same key match
  nothing, as a snapshot has one value per key.
- `heads=true` lists only heads, the snapshots that nothing replaces (see
  [Replacing a snapshot](#replacing-a-snapshot)). `heads=false`, the
  default, lists every snapshot.

On PostgreSQL, labels are matched through an index (GIN), so the order of
the `label`s doesn't matter.

### Order and pages

Snapshots come newest first, as summaries: what `GET /snapshots/{id}`
returns without `code`, `source`, `check` and `exports`.
- `limit` is 1–1000 (default 100), and a listing takes up to 64 `label`s.
- Keep requesting while a page has `next`, an opaque cursor, passing it as
  `after`. A full page has `next`, and the page after it may be empty.
- A cursor is a position, so it keeps working when its snapshot is deleted.

## Retrying a create

A create can carry an idempotency `key`, 1–256 bytes of letters, digits,
`.`, `_`, `:`, `/` and `-`, so that a retry is safe. Any other key is a
`bad_request` in `check.error`.
- **A replay.** While a snapshot made with the key exists, creating the
  same code with it returns that snapshot as it is, without building
  anything. The retry's other fields are ignored, `labels` and `verify`
  included, so a retry with `verify: true` can return `verified: false` with
  no error.
- **Other code.** Different code under a used key is a `bad_request` in
  `check.error`, and nothing is kept.
- **Unused keys.** A create that keeps nothing, e.g. when Lean reports
  errors or a `verify: true` re-check fails, doesn't use the key up, so a
  later create can. Deleting the snapshot frees its key.
- **The server's keys.** A create without a key gets one of its own, `job:`
  and 32 hex digits, so that when its worker is lost after keeping the
  snapshot, the retry of its job returns that snapshot instead of adding
  another. Keys that start with `job:` are the server's: a create that sends
  one is a 400.
- **The clients' keys.** The clients send a create that has no key with one
  of theirs, `client:` and 32 hex digits, so that sending it again after a
  lost answer returns the snapshot it kept (see [Clients](clients.md)).

Keys are unique in a scope, so two tenants can use the same ones (see
[Scopes](api.md#scopes)).

## Replacing a snapshot

A create can name the snapshot of its version that it `replaces`, e.g. the
skeleton with `sorry`s that a proof fills in. The code is kept only if it
still declares everything the replaced snapshot's module declares, each the
same kind of declaration with the same type and, for definitions, the same
value. It may add declarations, such as helper lemmas, and may still use
`sorry`. So filling a `sorry` can't quietly change what is proved:

```text
POST /snapshots {"code": "…theorem t : 1 = 1 := rfl", "replaces": {"snapshot_id": "v4.36.0-54500a67~9c0e…"}}
  → {"snapshot": {"id": "v4.36.0-54500a67~4b7a…", …, "replaces": {"snapshot_id": "v4.36.0-54500a67~9c0e…"}},
     "check": {"ok": true, …}, "replacement": {"filled": ["t"]}}
```

A create that names no `version` runs on the version of the snapshot it
replaces. A `version` other than that snapshot's is a `bad_request`.

### No new assumptions

A replacement can't fill a `sorry` by assuming it. A theorem stays a
theorem, and every `axiom` that the code declares, or that a snapshot it
builds on declares and the replaced module doesn't, must already be
declared, with the same statement, by the replaced module or by a snapshot
it no longer builds on. Otherwise the reason is `statement: the code adds
the assumption helper`.

The replaced module's own assumptions may stay. To add one on purpose,
create the code without `replaces`.

### Refused replacements

Code that changes a statement is not kept, and has no `snapshot`.
`check.ok` is false, and `check.reasons` says what changed, e.g.
`statement: t has a different type`. `check.changes` says the same of each
declaration, as `{name, module?, imported_from?, change, was?, now?,
was_explicit?, now_explicit?, explicit_alike_chars?, includes?}`.

`change` is one of:

| `change` | The declaration |
| --- | --- |
| `missing` | isn't declared by the code |
| `kind` | is another kind of declaration, e.g. an `axiom` where it was a `theorem` |
| `type` | has a different type |
| `value` | has a different value, as a definition; its type is the same |
| `derived` | has different constructors, or Lean makes different declarations from it, as an inductive type; its own type is the same |
| `added_assumption` | is an assumption the code declares, or a snapshot it newly builds on does, that the original didn't |
| `assumption` | is an assumption of the original, with a different statement |
| `ambiguous` | is a constant of a snapshot the code no longer builds on, which more than one declaration of the code and what it builds on matches |

The other fields:
- **`name`** is the declaration as the original code, or the snapshot
  `module`, names it; an added assumption's is as the new code names it.
- **`module`** names the snapshot module a constant is from, when the code
  no longer builds on it (see [Imports in a
  replacement](#imports-in-a-replacement)).
- **`imported_from`** is set for a `missing` declaration when a snapshot
  module that the code newly builds on declares an identical copy: the same
  name, kind, type and value. It names that module, the first when several
  do, and the reason says so too. An import doesn't stand in for the spec's
  own declarations: see [Reusing a proof](runs.md#reusing-a-proof).
- **`was` and `now`** are its signature before and after, for a changed type
  or assumption, from the modules' search exports. Those are what the builds
  printed; the comparison reads the declarations' hashes.
- **`was_explicit` and `now_explicit`** are set where `was` and `now` print
  alike, as when an instance is found by another path (see
  [Canonical form](checking.md#the-canonical-form)). They are the types before
  and after printed with every argument shown, instances too, cut to where
  they differ with 40 characters around it, and `…` where cut. The reason
  says so too. They need that text in both builds' exports.
- **`explicit_alike_chars`** is set where the types print alike even with
  every argument shown, as far as the exports print them: it is how far
  that is, 4,000 characters, and the types differ after that. The reason
  says so too.
- **`includes`**: what Lean makes for an inductive type (its constructors,
  recursor, `casesOn`, `injEq`, projections and the like) is one change, to
  that type, whose `includes` lists them. It is `derived` unless the type
  itself changed.

To change a statement on purpose, create the code without `replaces`.

The build of a refused create is kept all the same, with no snapshot.
Nothing lists or searches it, and creating the same code again, without
`replaces` or replacing another snapshot, builds nothing. It goes once it is
a day old with no snapshot on it (see [Retention](#retention)). So does a
build refused for what it [refutes](runs.md#refuting-a-target).

### Replacing only a head

A create may replace only a head, a snapshot that nothing replaces.
Otherwise nothing is built or kept:
- `check.error` is a `conflict` that names what replaces it;
- `heads` lists the snapshots to replace instead: those nothing replaces
  that are reached from the replaced one through what replaces it, oldest
  first.

A create that loses a race to another during its build is refused the same
way, and its build is kept, as for a refused comparison; the message ends by
saying so. A create whose replaced snapshot was deleted meanwhile is a
`not_found` in `check.error`, and its build is kept too.

`"fork": true` skips the check, for parallel attempts at one target. The
[spec audit](runs.md#auditing-a-run-against-its-spec) reports a fork. A key
replay comes first and returns its snapshot, whatever replaced it since.

A run whose spec was amended takes no more replacements, forks included:
see [Amending a spec](runs.md#amending-a-spec).

### Imports in a replacement

The code may import more, and may build on other snapshots than the
replaced snapshot's module does. A type names the constants it uses, such as
a definition from an imported snapshot, without saying what they are, so
swapping that snapshot could change what a statement means while every hash
stays the same.

So for each snapshot the code no longer builds on, the comparison follows
what the replaced module's declarations use there, through types and
definitions' values. Each constant it reaches must be the same, by name and
hashes, in what the code builds on now. Otherwise the reason is like
`statement: foo, from PisaSnap.s3c…, has a different value`. The walk stops
at Lean's and Mathlib's constants, which the version fixes.

### What declarations use

Each declaration in a check result records the constants of the code and of
its snapshots that its type and value use (`type_refs`, `value_refs`, with
`detail: ["declarations"]`). The comparison reads them:
- Private names are compared without the module they were declared in.
- What Lean makes on demand, such as equation lemmas (`generated`), needn't
  stay.

### Changing definitions with `allow`

`"replaces": {"snapshot_id": "…~9c0e…", "allow": {"definitions": ["helper",
"bound"]}}` lets the named definitions change their value but not their
type, e.g. to add a missing hypothesis. Any other definition that changes is
refused (`statement: g has a different value`).

`allow` may also name an inductive type, structure or class. Its
constructors, and what Lean makes from them, may then change, e.g. to add a
field, but not its own type. What Lean makes from it is told by name and
shape (a constructor, `casesOn`, `injEq`, a projection named after its
field), so a declaration the code names `S.below` isn't one.
`changed_definitions` lists, after the type, each of them that changed.

A theorem's type only names a definition, so its meaning can change while
its type matches. `replacement` in the response lists the
`changed_definitions`, and, as `affected`, the theorems of the new module
whose type uses one, directly or through another definition.

What `allow` may name:
- a definition (a declaration with a value) or an inductive type of the
  replaced snapshot's module;
- or one of a snapshot the code no longer builds on (see [Imports in a
  replacement](#imports-in-a-replacement)).

A name is compared without a private one's module, so `private def h` is
`_private.h`, or the full name a refusal gives, `_private.PisaSnap.s….0.h`,
but not `h`.

Errors:
- An empty list is a 400 `bad_request`: to change no definition, leave
  `allow` out.
- A name that isn't such a declaration is a `bad_request` in `check.error`,
  found before the code is built, and nothing is kept: `allow names nope,
  which isn't a definition or inductive type of the replaced snapshot or of
  a snapshot the code no longer builds on`. For `h` where a `private def h`
  would match, it adds `(for a private definition, write _private.h)`.

### Holes

A spec snapshot may leave definitions for the snapshots that replace it to
change: `"holes": ["program", "cost"]` at its create, named as `allow` names
them. Each is a definition or inductive type of its module or of a snapshot
it builds on. Otherwise the create keeps nothing, and `check.error` is a
`bad_request`: `holes names nope, which isn't a definition or inductive type
of the snapshot or of a snapshot it builds on`.

A snapshot that replaces it inherits them, as `holes` in
`GET /snapshots/{id}` shows, and may change them without `allow`.
`changed_definitions` lists them as it lists what `allow` lets change. The
[spec audit](runs.md#what-may-change) allows them to change, and allows
nothing else the leader didn't.

`holes` is a 400 with `replaces`, since a replacement inherits the replaced
snapshot's, and when it is empty (but see
[Amending a spec](runs.md#amending-a-spec)).

### The link

`replacement.filled` lists the declarations that used `sorry` and no longer
do. The new snapshot records `replaces: {snapshot_id, allow?}`, which
listings and `GET /snapshots/{id}` show. It is set by a create that passed
the comparison, or composed when the snapshot between is deleted (see
[Retention](#retention)).

That comparison ran on what the create's build reported, so acceptance
doesn't rest on it: the
[spec audit](runs.md#auditing-a-run-against-its-spec) compares again from
verified records. A key replay returns the snapshot as it is, without
`replacement`.

## How it works

- **Building.** A module is its code compiled to the module
  `PisaSnap.s<hash>`, where `hash` is a hash of the code, by `pisa build`
  (`docker/pisa-lean`). It writes what `lean -o` writes, and in the same
  process reports the module's declarations and their exports.
- **Using.** Later code imports it like any module, in `/check`,
  `/sessions` or another snapshot. A chunk can import several snapshots,
  which merges parallel branches.
- **Hits.** The worker that built a snapshot keeps a Lean process with the
  returned `header` imported, and code that starts with exactly that header
  goes there. It then costs only its own elaboration. Other workers fetch
  the module from the store and import it (seconds at Mathlib scale).

## What carries over

Snapshots follow Lean's module semantics. Children see a parent's
declarations, instances, `@[simp]` lemmas and notation. They don't see its
`private` declarations, `open`s, `variable`s, `set_option`s or unclosed
sections. Re-open what a chunk needs.

## Several files

A request is one module, so send a project's files one at a time, imports
first, each as a snapshot or a [named run](runs.md#naming-a-run) that the
files using it import.

Pasting them into one request instead makes them one file: one's `private`
names, `open`s and scoped notation reach the next. And two snapshots that
each paste a file's code declare its names twice, so no code can import
both.

## Module-system code

A chunk can be a module-system file (`module`, `public import`,
`@[expose]`):
- **Visibility.** Children then see what the module system exports:
  `public` declarations, with bodies only for `@[expose]` definitions.
- **Imports between kinds.** A `module` file can import only module-system
  snapshots; plain files can import either.
- **The returned `header`.** It keeps `module` and imports the new snapshot
  with the modifiers the chunk used for its snapshot imports. If the chunk
  had none, it uses `public import`, so chains stay transitive.

## Results

- Code is kept only if Lean reports no errors and, when it runs, the kernel
  re-check passes (see [Verifying](#verifying)). `check` is a check result,
  as for `/check`, without `sorries`.
- A chunk with `sorry` or another axiom outside the allowed list is kept,
  with that `status` (see [Statuses and
  assumptions](runs.md#statuses-and-assumptions)), and checks that use it
  report the axiom.
- Imports of an unknown or deleted snapshot are `not_found`.

## Verifying

The kernel re-check runs in a clean process (`pisa verify`). It imports the
snapshot's dependencies again and re-runs the kernel over its declarations,
so its statuses don't depend on the build being honest. It adds about 2 s to
a small chunk on `import Mathlib`.

`verified` means the snapshot and everything it imports have passed the
re-check. So a chunk re-checked on top of unverified snapshots isn't
`verified` yet. A module that loses it, as one the kernel rejects when
verified `again` does, takes it from everything built on it, and its
`check.reasons` say why.

Check results don't say whether the snapshots they import are verified:
look at the snapshot, or verify it, when that matters.

### At creation

Every create runs the re-check, unless `verify` in the request says
otherwise:

| `verify` | Kernel re-check at creation |
| --- | --- |
| omitted | This chunk, or kept code that isn't verified yet; if the kernel rejects it, it isn't kept |
| `true` | This chunk and any unverified imports; the snapshot is `verified`, or `check` has an `error` |
| `false` | None, and the statuses are the build's own report; module-system code is re-checked all the same, as only the re-check can describe it |

The re-check has the request's `timeout` for each module, once their
imports are loaded. When it runs out of time or memory:
- Code outside the module system with no macros, its own or an import's, is
  kept, not `verified`, with the statuses of the build's own report. The
  response's `unverified` says why, and
  [verifying it later](#verifying-later) finishes the re-check.
- Other code, and code asked to be verified, isn't kept, and `check.error`
  is a `timeout` or `out_of_memory`. Module-system code exports its theorems
  as axioms to what imports it, so only the kernel re-check can say what it
  proves, and macros could expand into what the policy forbids.

A re-check that fails, rather than run out of time or memory, keeps
nothing: `check.error` is a `crash` with what the verifier said. A create
with `verify: true` whose imports' re-check doesn't finish answers with that
kind, `timeout` included, and `lean` only when the kernel rejected one.

### Verifying later

`POST /snapshots/{id}/verify` re-checks the snapshot's module and
everything it imports, imports first, in one process that loads their
imports once, and records each module as it passes. A later run covers only
new chunks, and a retry resumes where the last one stopped. `verified` and
`status` belong to modules, so every snapshot of a module shows the result.

The body is optional, `{"timeout": 60, "again": false}`:
- `timeout` (default 60) is the seconds each module may take once the
  imports are loaded.
- `again` (default `false`) re-checks the modules that are verified already
  too, and describes their records anew.

A missing or `null` body takes the defaults; the endpoint reads no query
parameters. It takes `Prefer: respond-async`, as a job of kind
`verify_snapshot` (see [Async requests](api.md#async-requests-and-jobs)).

The response is `{snapshot, checked, failure?}`:
- `snapshot` is the snapshot as the re-check left it, and `checked` lists
  the modules this run re-checked and passed, in order.
- If the kernel rejects a module or the re-check doesn't finish, the run
  stops, still with a 200, and `failure` is `{module, kind, reason}`. `kind`
  is `lean` when the kernel rejected it; `timeout` or `out_of_memory`, worth
  a retry with more time or memory; or `crash` when the verifier failed.
- Nothing is deleted on a failure, because other code may import the
  module: it stays usable and not `verified`. Delete its snapshots if you
  don't want it.
- `status` and the module's `check` are updated from the re-check, whose
  axioms don't depend on the build being honest.

## Retention

A snapshot is kept until it is deleted. A module is kept while a snapshot
of it exists or a kept module imports it.

### Deleting a snapshot

`DELETE /snapshots/{id}` deletes the snapshot:
- **Modules.** It removes its module once nothing else needs it, then each
  of that module's imports on the same terms, and `removed` lists them. So
  deleting a snapshot whose module others build on removes nothing yet, and
  the code on top keeps working.
- **Links.** Deleting a snapshot that [replaced](#replacing-a-snapshot)
  another re-points what replaced it at that one, with the two links
  composed: `allow` names what either link allowed, and `replacement` holds
  the changes both steps found, so a run isn't cut.
- **Unlinked snapshots.** A snapshot with no link, such as a spec snapshot,
  leaves its replacers' links as they are, pointing at a snapshot that is
  gone: the [audit](runs.md#auditing-a-run-against-its-spec) of a spec
  copied from it isn't `clean`.
- **Creates meanwhile.** A create that replaces a snapshot deleted while it
  was being built answers with a `not_found` in `check.error`.
- **Workers.** Each worker checks a module it has cached against the store
  at most every `PISA_SNAPSHOT_CONFIRM_SECS` (60), so a removal takes effect
  everywhere within that time.

`deleted` lists the snapshots that went: a plain delete lists one.

### Deleting a branch

`DELETE /snapshots/{id}?with_replacers=true` takes the snapshot and every
snapshot that replaces it, directly or through others, in one step, and
splices nothing. What the snapshot replaced is a head again if nothing else
replaces it. `deleted` lists every snapshot that went, the asked one first.

### Sweeps

Past a snapshot's own entry, a delete is best effort: a worker that stops
partway leaves modules nothing needs, and files in the store. Once an hour
one worker of each version sweeps them, a worker that has just started
included:
1. The modules no snapshot is on and no module imports go, as a delete
   would remove them. A module goes only once it is a day old, as a build
   that a create's link refused is kept with no snapshot for the create that
   follows (see [Refused replacements](#refused-replacements)).
2. Then the files no module's record names go, once a day old, as a build
   uploads its files before its record.

Files are swept only from a store that names the worker's own catalog.
Each version's catalog has a random id, which a worker writes to
`catalogs/<version>` in the store with its first upload, unless the store
names one already, or holds files more than an hour old of builds its
catalog has no record of.

A worker on an empty, scratch or other database leaves the store's files
alone and logs an error. So does a sweep that would delete most of them (16
or more). `/health`'s `sweeps_refused` says why, until a sweep goes through
(see [Operations](operations.md)). A new store, which names no catalog until
the first upload, has nothing to sweep, which isn't a refusal.

## A database reset on a kept store

The store names the old catalog, or none, and holds builds the new one has
no record of, so no worker sweeps it. To have the new catalog own the store:
1. Start one worker with `PISA_CLAIM_STORE=1`, which makes the store's
   marker name its catalog.
2. Unset it again.

That catalog's sweeps then delete the old builds' files, unless they are
most of the store's: delete those by hand, under `snapshots/<version>/`.

## Trust

A snapshot that isn't `verified` has the same guarantee as a `/check`: the
policy scan and an axiom report from the process that imported it. Macros
go beyond that, because what they expand into isn't in the text the scan
sees, so with `verify: false`, `ok` and `status` can be wrong until the
snapshot is verified. A `verified` snapshot's own declarations and those of
everything it imports have been re-checked through the kernel, so a forged
proof is caught.

Either way, the `.olean` is imported without re-checking, as with
Mathlib's cache:
- Whoever can write the store can still install an unchecked module.
- The recorded SHA-256 catches only corruption, not a malicious write.
- The oleans of the version's own libraries, such as Mathlib's, and the
  kernel are the trusted base.

Acceptance judges these same files, exported, not the stored code (see
[Accepting a run](acceptance.md)): what it accepts is what the store holds.

## Searching them

The servers can search snapshot declarations, by label, next to Mathlib's;
see [Snapshots in search](search.md#snapshots-in-search).
