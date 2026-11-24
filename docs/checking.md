# Checking code

This page is for anyone who sends Lean code to pisa to be checked or worked
on: `/check` and batches, what a check result holds, checking against a
spec, goals and types at positions, documents and tactic sessions on Lean
Beam, the verification policy every check runs under, and the limits. The
endpoints' request and response shapes are summed up in the
[HTTP API](api.md#endpoints).

## Checks and batches

`POST /check` runs code through Lean on a worker and answers a verdict:

```sh
curl -s localhost:8000/check -H 'content-type: application/json' \
  -d '{"code": "theorem t : 2 + 2 = 4 := by decide"}'
```

```text
→ {"ok": true, "time": …, "timing": {…}, "version": "v4.36.0-ca45ccf8", "messages": [], "sorries": [],
   "reasons": [], "declarations": [{"name": "t", "kind": "theorem", "axioms": [], "status": "proved"}]}
```

That example needs only Lean, so it runs on a `PROJECT=lean-core` stack,
whose version is `v4.36.0-ca45ccf8`. Code that imports Mathlib needs a
stack built with it, the default.

- **The request** is `{code, version?, timeout?, detail?, at?, against?}`.
  `version` picks the worker pool (see [Versions](api.md#versions)), and
  `timeout` is Lean's limit in seconds, 60 unless given. `detail` asks for
  more: `tactics` (see [Tactics](#tactics)), `terms` (see
  [Goals and types at positions](#goals-and-types-at-positions)) and
  `declarations` (see [The check result](#the-check-result)). `at` takes
  positions, and `against` a snapshot (see
  [Checking against a spec](#checking-against-a-spec)).
- **Failures are in-band.** A check that times out, crashes or is refused
  by the policy still answers 200, with `ok: false`, the `error` and the
  `reasons`. Code with errors has them in `messages`, and `ok: false`.
- **Batches.** `POST /check/batch` takes `{items: [{code}], version?,
  timeout?, detail?, at?}`, up to 128 items, and answers `{results}`, a
  check result per item, in order. `detail` and `at` apply to every item.
  It queues 32 items at a time, so that an item's wait for a worker starts
  when it is queued, not when the batch arrives. Each item's failure is in
  its own `error`, running out of capacity included, so one failed item
  never fails a batch.
- **Long checks.** `/check` runs as a job with `Prefer: respond-async` (see
  [Async requests and jobs](api.md#async-requests-and-jobs)). A batch
  doesn't: send its items to `/check` instead.

Nothing is kept between checks: each check elaborates its code again. To
ask about one proof many times, open it as a [document](#documents).

## The check result

A check result is `{ok, time, timing, version?, error?, messages, sorries,
reasons, changes?, declarations, tactics?, at?, warnings?, runs?}`:
- `ok` is the verdict: the code elaborated without errors, and passed every
  rule of the [verification policy](#the-verification-policy).
- `time` is the seconds Lean spent on the code.
- `version` is the version of the worker that ran it (see
  [Versions](api.md#versions)): the one the request named, the server's
  default, or, with neither, whichever pool took it. It is absent only when
  no worker ran the check.
- `error` says why the check couldn't complete: a timeout, a crash, the
  policy, and the like, with its [kind](api.md#error-kinds).
- `messages` are Lean's, each `{severity, pos, end_pos?, text}`, with
  positions in the code as sent.
- `sorries` are the code's `sorry`s, each `{goal, pos?, end_pos?}`.
- `reasons` say why `ok` is false, e.g. `axiom: t uses sorryAx` or
  `error: Lean reported errors`.
- `changes` lists, for a [replacement](snapshots.md#replacing-a-snapshot),
  a session's final check or a check `against` a snapshot, the
  declarations that don't keep the statements they must, each also one of
  `reasons`: `{name, module?, imported_from?, change, was?, now?,
  was_explicit?, now_explicit?, explicit_alike_chars?, includes?}`.
  - `change` is `missing`, `kind`, `type`, `value`, `derived`,
    `added_assumption`, `assumption` or `ambiguous`.
  - `was` and `now` are the signature before and after. Where they print
    alike, as when an instance is found by another path, `was_explicit` and
    `now_explicit` print the types with every argument shown, around where
    they first differ, and `explicit_alike_chars` says how many characters
    they still print alike when even those don't differ.
  - `imported_from` names, for a missing declaration, a snapshot module the
    code imports that declares an identical one: an import can't stand in
    for the spec's own declarations (see
    [Reusing a proof](runs.md#reusing-a-proof)).
  - `includes` lists, for a `derived` change, the declarations Lean makes
    from an inductive type that changed.
- `declarations` lists each new declaration as `{name, kind, status,
  axioms}`, its axioms being those it uses besides the allowed ones (see
  [Statuses and assumptions](runs.md#statuses-and-assumptions)). One that
  gets axioms through theorems of other snapshots also has `inherits`, those
  theorems, and `own_status`, what its own axioms make it. With
  `detail: ["declarations"]` each has every field: all its `axioms`, the
  assumptions it uses (`assumes`), the hashes and references the
  comparisons run on (`type_hash`, `value_hash?`, `type_refs`,
  `value_refs`, `generated?`, `parent?`), and for a theorem of `¬ X`,
  `negates`, the hash of `X` (see [Refuting](runs.md#refuting-a-target)).
  `/sessions/{id}/check` and `POST /snapshots` take
  `detail: ["declarations"]` too, and `GET /snapshots/{id}` takes
  `?detail=declarations`, whose records also have `own_axioms`.
- `tactics` and `at` answer `detail: ["tactics"]` and `at` (see
  [Tactics](#tactics) and
  [Goals and types at positions](#goals-and-types-at-positions)).
- `warnings` says what went wrong with goal reporting: that it failed or
  ran past `timeout`, so the result is a plain check's, or that it took
  over three times as long as the check, and a second or more longer.
- `runs` says what each import by run name resolved to (see
  [Naming a run](runs.md#naming-a-run)).

`timing` says where the time went, in seconds: `{queue, load, execution,
tracing, verification, total}`. Every operation that runs Lean has one, in
the same shape, and a phase that doesn't apply is 0.
- `queue` is the wait for a worker, from queueing the job to a worker
  claiming it, and `total` the worker's wall clock from claiming it to
  answering.
- A check's `load` is the header import, 0 when a process had it warm,
  `execution` its body (`time`), `tracing` the run that reads its `tactics`
  and `at` (0 when it asks for neither), and `verification` the axiom
  report.
- A create's `execution` is the compile, imports included, and its
  `verification` the kernel re-check.
- A session's start reports its open as `execution`. A tactic's `load` is
  the reopen and the replay of earlier steps, and `execution` its step.
- A probe's `load` is the reopen and `execution` its candidates; goals'
  `load` is the reopen and `tracing` the asking.

## Checking against a spec

`against` names a snapshot, usually the spec the code is meant to
[replace](snapshots.md#replacing-a-snapshot), and the check compares the
statements the code shares with it: each declaration both declare must be
the same kind with the same type. One that isn't is in `changes` and
`reasons`, with the snapshot's signature as `was`, and `ok` is false, so a
file that a replacement would refuse says so before any create.
Declarations only one of them has aren't compared, nor definitions' values,
which a create judges with the spec's holes and `allow`. The check runs on
the snapshot's version unless it names one, which must be that version
(else a 400); an unknown snapshot is a 404.

## Tactics

With `detail: ["tactics"]`, a check result also has `tactics`, the goals at
every step of the proof. Each entry is one run of a tactic, in the order
Lean ran them, as `{tactic, goals_before, goals_after, pos, end_pos}`:
- `tactic` is its source text, and `pos` and `end_pos` locate it.
- `goals_before` and `goals_after` list the goals as Lean prints them.
- Structure such as `by`, `·`, `<;>` and parentheses has no entry of its
  own, but the tactics inside it do.
- A tactic run on several goals, e.g. under `all_goals` or `<;>`, appears
  once per run, and so does every alternative `first` tried. One that
  failed has the same goals after as before.

## Goals and types at positions

`at` takes up to 64 positions in the code, as `{line, column}` like those of
`messages`: lines count from 1, and columns from 0 in Unicode code points.
A check result then has `at`, one entry per position in request order, as
`{pos, tactic?, term?}`:
- `tactic` is the tactic at the position, as an entry of `tactics`: its
  `goals_before` and `goals_after` are the goals there. There is none
  outside a tactic proof, nor on structure such as `by` or the space
  between two tactics; the earlier one's `goals_after` are the goals in
  that space.
- With `detail: ["terms"]` too, `term` is the term at the position that Lean
  elaborated against an expected type, as `{term, expected_type, expr, pos,
  end_pos}`, where `term` is the source text and `expr` the term as Lean
  elaborated it. Lean gives the head of an application the application's
  expected type: on `f` in `f x`, `term` is `f`, expecting what `f x` was
  expected to be.
- Both are the innermost of their kind written in the code: the range that
  starts last, then ends first. Of two nodes on one range, such as `rfl`
  and its head `@rfl`, the outer one is reported.
- On a batch, `at` applies to every item. `terms` without `at`, or more than
  64 positions, is a `bad_request`.

Goals and tactics come from a second run of the code, after the plain check
that gives `messages`, the verdict and `time`. It has a `timeout` of its
own, and its time is `timing.tracing`:
- Without `tactics`, Lean prints only what contains one of the positions,
  terms included. On a 542-line failing file, goals at one position took
  6.5 s, against 6 s for the check and 150 s for the whole tree. A position
  under `all_goals` gets the goals of each of its runs: on a 110-line file
  with 81 branches, 12 s against the check's 6 s.
- `tactics` need the whole tree, and with `terms` every term of the code
  too: several times the check, and minutes for a 1,000-line chunk.
- When the second run fails or runs past `timeout`, the result is the plain
  check's, without `tactics` or `at`. `warnings` says so, e.g. `goal
  reporting ran past 90s; diagnostics are from a plain check`. It also says
  when goal reporting took over three times as long as the check, and a
  second or more longer.

Nothing is kept between checks, so ask about every position in one: each
check elaborates the code again.

## Documents

A check elaborates its code again each time. To ask about one proof many
times, open it as a document on Lean Beam, which keeps the elaborated file.
Then ask for the goals at positions, and try candidate tactics at a
`sorry`:

```text
POST /documents {"code": "theorem t (n : Nat) : n + 0 = n := by\n  sorry"}
  → {"id": "v4.36.0-ca45ccf8~77b2…", "revision": 1, "messages": […],
     "sorries": [{"goal": "n : Nat\n⊢ n + 0 = n", …}], "timing": {…}}
POST /documents/v4.36.0-ca45ccf8~77b2…/probes {"at": {"sorry": 0}, "candidates": ["rfl", "simp", "omega"]}
  → {"revision": 1, "pos": {…}, "results": [{"ok": …, "closed": …, "goals": […], "messages": […]}, …],
     "reopened": false, "timing": {…}}
DELETE /documents/v4.36.0-ca45ccf8~77b2…
  → 204
```

- **Opening.** `POST /documents` opens the code, and answers Lean's errors
  and warnings (`messages`) and the `sorries`, each with its goal. The `id`
  carries the version, as a snapshot's does. Its `timing` says where the
  open's time went: `load` is the startup (waiting for the document's
  lock, a Lean Beam process starting, the file), `execution` Lean
  elaborating the code and its header import, and `tracing` the
  diagnostics and the goals of its `sorry`s. An open that fails, a 422
  `timeout` when it runs out of its `timeout` included, has the phases up
  to the failure in the error's `timing`.
- **Goals.** `/documents/{id}/goals` takes 1 to 64 positions, as checks do.
  A position with only a `line` is that line's first tactic, after any `·`
  or `.` bullet, and the answer gives the column it used. There are no
  goals in the imports or outside a proof. `mode: "after"` gives the goals
  after the tactic at the position. Each answer is `{pos, line, in_proof,
  goals}`: `line` is the text of the line it was asked at, and `in_proof`
  says whether Lean has a proof there. No goals with `in_proof: true` is a
  finished proof, and with `false` a position outside any proof, as a line
  off by one can be. Lean counts a blank or comment line right after a
  proof as in it.
- **Probes.** `/documents/{id}/probes` runs 1 to 64 candidates at one
  place: a `sorry` (`{sorry: n}`, an index into `sorries`), or a position
  in the body.
  - Each runs on the document as it is, in turn, with `timeout` seconds of
    its own, within what is left of the request's `budget` (300 s unless
    given, at most 3,600). Those it doesn't reach are a `timeout` that says
    the budget ran out. Nothing a candidate does stays.
  - When Lean Beam restarts during the request, after three timeouts in a
    row or a failure, the document reopens in the new process and the rest
    run there, with `reopened: true`. The candidate Beam failed on has its
    error. If the document can't reopen, the rest have that error.
  - The policy refuses a candidate as it refuses code, in its `error`, and
    the others still run. A candidate that reaches Lean's recursion or
    heartbeat limit has a `recursion_limit` or `heartbeat_limit` error,
    with `limit` naming the option (`maxRecDepth` or `maxHeartbeats`), its
    `value` and the `phase` Lean was in: it is the candidate's own, as it
    comes again on a retry. A candidate Lean Beam fails to run in any other
    way has an `unavailable` error, which isn't Lean's answer.
  - `closed` means the candidate ran without errors, left no goals, and
    doesn't say `sorry` or `admit`. It is no verdict: Lean Beam checks
    neither termination nor what a tactic elaborated to. Check the finished
    proof with `/check`.
- **Editing.** `PATCH` replaces the code. It names the `revision` it
  replaces, and is a 409 `conflict` if the document is at another. Goals and
  probes may name a `revision` too.
- **Closing.** `DELETE` closes the document and deletes its code. An unused
  one expires after `PISA_STATE_TTL_SECS` (a day in Compose and the chart;
  never, by default).
- **Where it runs.** A document's jobs run one at a time, on the worker
  that holds it open, while that worker is live. If it goes, another
  reopens the stored code, which takes as long as elaborating it. A reopen,
  there or after the document was closed to make room or its Lean Beam
  restarted, has the `timeout` of the open or last edit, not the
  request's, and the request's own `timeout` starts after it. Goals and
  probes then answer `reopened: true`, and an error after it says the
  document was opened again first. A reopen that fails says it was opening
  the document again.
- **Placement.** A document no live worker holds, as a new one, opens on
  the Beam worker with the fewest documents open per Beam slot: the others
  leave it to that worker for up to two seconds. A Lean Beam process that
  times out three times in a row is restarted, as what timed out may still
  run in it, and its documents reopen. A request in flight on another of
  its documents answers `unavailable`, naming the restart, rather than
  `crash`.
- **Documents answer synchronously:** they don't take
  `Prefer: respond-async`.

### Beam workers

Only workers with Beam slots serve documents and sessions. Without one,
the endpoints answer 503 `unavailable`; a 429 means no Beam worker took the
job in time. Lean Beam ships with toolchains from Lean v4.36, so a pool on
an earlier one serves checks and snapshots only. The worker settings (see
[Worker settings](configuration.md#worker)):
- `PISA_BEAM_SLOTS` (default 1): `lean-beam-mcp` processes beside the REPL
  slots, each in a sandbox and cgroup of its own. 0 serves no documents or
  sessions.
- `PISA_BEAM_DOCUMENTS` (default 4): the documents each keeps open. To open
  another, it closes the least recently used one that no job holds, and
  logs it at `info`. The worker then no longer holds that one, whose next
  job opens it on the Beam worker with room.
- `PISA_BEAM_MEMORY_MAX_MB` (default 0, none): each one's memory limit. Out
  of memory, the kernel stops the whole process (`memory.oom.group`), and
  the request answers `out_of_memory`.
- `PISA_BEAM_BIN`: `lean-beam-mcp`, by default the one in Lean's
  toolchain.

## Tactic sessions

A session is a [document](#documents) for building proofs one tactic at a
time, as a tree of states:

```text
POST /sessions {"code": "theorem t (n : Nat) : n + 0 = n := by\n  sorry"}
  → {"id": "v4.36.0-ca45ccf8~5e1a…",
     "states": [{"id": "v4.36.0-ca45ccf8~5e1a…~0c3f…", "goals": ["n : Nat\n⊢ n + 0 = n"], "pos": {…}, "end_pos": {…}}],
     "messages": […], "timing": {…}}
POST /states/v4.36.0-ca45ccf8~5e1a…~0c3f…/tactic {"tactic": "simp"}
  → {"state": {"id": "…", "goals": []}, "complete": true, "messages": [], "reopened": false, "replayed": 0, "timing": {…}}
POST /sessions/v4.36.0-ca45ccf8~5e1a…/check {"code": "theorem t (n : Nat) : n + 0 = n := by\n  simp"}
  → {"ok": true, …}
DELETE /sessions/v4.36.0-ca45ccf8~5e1a…
  → 204
```

- **Starting.** `POST /sessions` opens the code, and answers a state for
  each `sorry`, with its goals, and Lean's errors and warnings
  (`messages`). The session's `id` is its document's, so
  `/documents/{id}/goals` and `/probes` work on it, but its code stays: a
  `PATCH` is a 400 `bad_request`.
- **Tactics.** `POST /states/{id}/tactic` runs a tactic on a state, in the
  context of the session's code, so it sees what the code declares before
  the `sorry`.
  - It answers the new state, which can be continued; a tactic that fails
    is a 422 `lean`, with Lean's errors in its message. A tactic that
    reaches Lean's recursion or heartbeat limit is a 422 `recursion_limit`
    or `heartbeat_limit`, with `limit` as for probes. The policy refuses a
    tactic as it refuses code, a 422 `policy`. Lean Beam failing on its own
    is not Lean's answer: the state is replayed and the tactic run once
    more, and a second failure is a 503 `unavailable`.
  - A state stays valid after others are made from it, so a client can try
    alternatives from one state.
  - `complete` means no goals are left and no tactic on the way from the
    `sorry` says `sorry` or `admit`. As with probes, it is no verdict.
- **State ids.** A state's id is `<version>~<session>~<state>`, where
  `<state>` names what the state is: the same tactic on the same state
  gives the same id.
- **The final check.** `POST /sessions/{id}/check` checks the assembled
  proof from scratch, as `/check` does, and also requires the session's
  original statements and imports. It is a `lean` error if the axiom report
  can't describe a declaration of the session's code, whose statement it
  then couldn't guard.
- **Replaying.** The worker that holds the session's document keeps the
  last 256 states it made. Another worker, or this one for an older state,
  replays the state's tactics from the nearest state it keeps, or from the
  `sorry`, reopening the document first if it isn't open there. Each
  replayed tactic has the `timeout` the session's start had. The answer
  says how many tactics it `replayed`, and whether it `reopened` the
  document; an error says so too. A replay that fails keeps none of the
  states it made on the way, so the next try replays from where this one
  started.
- **Closing.** `DELETE /sessions/{id}`, or `DELETE /documents/{id}` on its
  id, closes a session (204), and its states then answer 404. A session
  expires `PISA_STATE_TTL_SECS` after it starts.
- **Async.** A session's start, tactics and final check run as jobs with
  `Prefer: respond-async` (see
  [Async requests and jobs](api.md#async-requests-and-jobs)).
- Only workers with [Beam slots](#beam-workers) serve sessions. Without
  one, `/sessions` and `/states/{id}/tactic` answer 503 `unavailable`.

## The verification policy

A check is `ok` only when all of these hold:

- The code elaborated without errors.
- Every new declaration depends only on `propext`, `Classical.choice` and
  `Quot.sound` (`PISA_ALLOWED_AXIOMS`). This rejects `sorry`, `admit`,
  `native_decide` and new axioms.
- The code contains no metaprogramming: `#eval`, `#eval!` and `#guard`
  (which evaluates its term as `#eval` does), `run_cmd`, `elab`,
  elaborator attributes, `unsafe`, `implemented_by`, simprocs, attributes
  that install code to run later (`@[init]`, `@[tactic]`, delaborators,
  `norm_num` and `positivity` extensions) and similar. Attributes are
  refused where they are applied, in `@[…]` and `attribute […]`, so
  `extern` or `init` can still name a variable, and a name in `«»` is never
  a command. A command's word is refused even as a field, as in
  `(c).initialize`: when Lean recovers from a parse error before it, it
  runs the command there. Write such a name `«initialize»`, as the
  refusal's hint says.
- Any `set_option` changes only an allowlisted option
  (`PISA_ALLOWED_OPTIONS`): resource limits (`maxHeartbeats`,
  `maxRecDepth`, `synthInstance.maxHeartbeats`, `synthInstance.maxSize`,
  `maxSynthPendingDepth`, `exponentiation.threshold`), `autoImplicit` and
  `relaxedAutoImplicit`, `diagnostics`, `quotPrecheck*`, `linter.*`,
  `pp.*`, `trace.*`, and `backward.*`, which restores older elaborator
  behaviour that Mathlib-style code often asks for. Options that skip the
  kernel, such as `debug.skipKernelTC`, are refused.

Code the scan refuses is refused before it waits for a worker's Lean
process. The result's `reasons` say what (`forbidden: #eval`,
`option: debug.skipKernelTC`), and its `messages` say where, each with what
to do instead, such as that a snapshot allows the macro family; a
session's or a tactic's error says the same of the first.

Lean's `autoImplicit` option is off by default, in checks, sessions and
snapshot builds alike, so a name that isn't bound in a statement is an
error rather than a new implicit argument. Code can turn it back on with
`set_option autoImplicit true`, alone or with `in`, because it is an
allowlisted option. Code written for a library that turns it on, as Physlib
does, meets "Unknown identifier `d`" here, with Lean's note that the option
would have bound `d`. When code that doesn't set the option has such an
error (or an unknown universe level), a check's or a snapshot build's
`messages` add an `info` hint at it, naming the option. An unknown name the
option wouldn't bind, such as a misspelt lemma in a proof, gets no hint.

### The axiom report

The axiom report is a `run_cmd` call into the `Pisa` library
(`docker/pisa-lean`, built into the worker image at `PISA_LEAN_LIB`), so
workers add `import Pisa` to every header they load, or `meta import Pisa`
to a module-system header. It describes each declaration: its axioms, and
the canonical text of its type and, for a definition, of its value, which
the worker hashes with SHA-256. The text is injective, so two declarations
with the same hashes have the same statement. The kernel re-check
describes declarations with the same library.

On `/check` and sessions the report runs even when the code has errors, so
the declarations Lean kept still have a status. One it can't describe,
such as one left with a metavariable, is a reason (`error: axiom report:
describing x: a metavariable`), as is a report that fails outright
(`error: axiom report unavailable: …`, with what Lean said). A snapshot's
module and the kernel re-check describe every declaration or nothing.

### The canonical form

The canonical form reads code as the kernel does, so that what doesn't
change a statement doesn't change its text:
- Binder names don't count, and binder kinds (`(n)`, `{n}`, `[inst]`) do.
  A declaration's universe parameters count by position, so renaming one,
  or the hygienic name Lean gives one it makes, doesn't change it.
- A module's own private names read without the module, which differs
  between a module and its replacement; another module's read whole, so
  one `import all`-ed can't stand in for the module's own.
- A `sorry` reads as one marker of its type, whatever Lean labels it with
  (the module and position it was written at).
- A proof reads as the proposition it proves, as the kernel takes any two
  proofs of a proposition to be equal: an application of a theorem, or of
  a constant whose type is a proposition (`rfl`, `Eq.refl`), and a `sorry`
  of a proposition. So a field filled with `sorry`, Lean's `_proof_1` or a
  lemma reads the same.
- Lean's auxiliaries (internal names such as `f._unary` or `f._mutual`,
  matchers such as `f.match_1`, `grind`'s) and the definitions it makes for
  notation and syntax (the parser of `notation "d" => 2`) aren't
  declarations of their own: what uses one reads its value in its place.
  So notation can be added, dropped or reworded, as statements are
  compared after it is expanded.
- Equation lemmas Lean makes on demand are `generated`, including those it
  names privately in module-system code (`_private.….f.eq_1`, for a `f`
  whose body isn't exposed).

What it doesn't see through is refused, as a changed statement would be:
- an instance found by another path after new instances or imports, such
  as a helper's `import Mathlib` that the spec doesn't have: the refusal's
  `was_explicit` and `now_explicit` show the instance arguments (see
  [Replacing a snapshot](snapshots.md#replacing-a-snapshot)), and a spec
  avoids it by importing what its proofs will (see
  [Running agents on snapshots](runs.md#running-agents-on-snapshots));
- a proof by a lemma stated in another but equal form, such as one of
  `b ≥ a` where `a ≤ b` is wanted, which reads as a proof of `b ≥ a`;
- a statement that uses an `import all`-ed private of a snapshot the code
  no longer builds on;
- a definition that uses `panic!`, whose message carries the module and the
  line it is written on.

### The kernel re-check

The scan is a conservative pre-filter, not the last word: it
over-approximates "the kernel checked every declaration with sound options"
by forbidding anything that could subvert the kernel or the report. A
[`verified`](snapshots.md#verifying) snapshot doesn't rely on it. The
re-check runs after the build, in a separate clean process (`pisa verify`,
built from `docker/pisa-lean`): it re-adds the module's own declarations to
an environment of just its dependencies, with the kernel checking each one,
and collects their axioms by walking the re-checked constants. So a
declaration added during the build with the kernel check skipped is caught
even though no forbidden keyword appears in the source.

Because that verdict doesn't depend on the build being honest, snapshot
creation allows the macro family (`macro`, `macro_rules`, `syntax`,
`declare_syntax_cat`): pure syntax transformers whose only risk is what
they expand into, which the re-check covers. The re-check runs by default,
and `verify: false` opts out, but not for module-system code: its build
exports its theorems as axioms, so only the re-check can describe it.
`/check` and `/sessions`, which don't produce a module to re-check, keep
the scan as it is.

### Policy settings

The workers read the policy from three settings (see
[Worker settings](configuration.md#worker)):
- `PISA_ALLOWED_AXIOMS`: the axioms a declaration may depend on and still
  be `proved`, comma-separated; `propext,Classical.choice,Quot.sound` by
  default. The servers read it too, for what an audit lists of a
  declaration that isn't proved: set it on both, or on neither.
- `PISA_ALLOWED_OPTIONS`: the options `set_option` may change,
  comma-separated, a trailing `*` matching a prefix; the list above by
  default (see
  [The default allowed options](configuration.md#the-default-allowed-options)). Setting it replaces the whole list.
- `PISA_ALLOW_METAPROGRAMMING`: off by default. Set, the scan lets
  metaprogramming through, the macro family included, everywhere;
  `set_option` is still held to the allowlist. A check's or a session's
  verdict on such a pool rests on code that could rewrite the axiom
  report, so only a `verified` snapshot's statuses can be trusted there.

## Limits

- The REPL never frees environments, so workers replace each process after
  `PISA_MAX_USES` jobs (200 by default), or once its resident memory passes
  `PISA_MAX_RSS_MB` (off by default).
- A session doesn't write a finished tactic script back into the `sorry`.
  Clients submit the assembled proof to `/sessions/{id}/check`.
- A state that another worker continues is replayed from its `sorry`, so
  that step takes as long as the tactics before it, and the reopening.
- A check that runs past its time limit reports no declarations, and
  doesn't say which one Lean was elaborating: bisect with smaller checks.
- There is no authentication, and no fairness between
  [scopes](api.md#scopes).
- An acceptance judges its run's pairs one after another, on one worker
  slot, and each of the six kernels replays every constant a pair's
  theorems reach, the library's included: on a large library, minutes per
  pair. See [The kernels](acceptance.md#the-kernels).
- Search serves one version, `PISA_SEARCH_VERSION`; on another pool,
  `/search`, `/resolve` and `/declarations` answer `not_found`. See
  [What a pool lacks](environments.md#what-a-pool-lacks).
