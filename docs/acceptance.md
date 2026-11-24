# Accepting a run

This page is for leaders who judge a run of agents with `lake comparator`,
and for anyone who judges it again outside pisa. It covers
`POST /snapshots/accept` and the acceptances it stores, the export of a run
as a Lake project, a pair exported as a Palomar package, and, last, how a
worker judges a pair, for operators.

## Acceptance and the audit

The [audit](runs.md#auditing-a-run-against-its-spec) compares a run's heads
with its spec snapshots using pisa's own code. `lake comparator`, which Lean
ships, judges a run with none of that code. Accepting a run takes both: the
run is `accepted` when the audit is `clean` and comparator accepts every pair
of a spec snapshot and a head it reaches.

Comparator judges the modules pisa built and kernel-checked when the
snapshots were created, exported with Lean's bundled `leanexport`.
Acceptance builds nothing, and no code of the run runs while it is judged. A
head passes comparator when each theorem of the spec is proved there with
the same statement, the definitions those statements use are the same but
for the spec's holes and `allow`, each assumption keeps its statement, and
the proofs use only the standard axioms (`propext`, `Quot.sound`,
`Classical.choice`) and the spec's assumptions.

The two don't see the same things (see
[Where the audit and comparator differ](#where-the-audit-and-comparator-differ)),
so comparator adds to the audit and doesn't replace it.

## Judging a run

```text
POST /snapshots/accept {"against": ["v4.36.0-54500a67~9c0e…"]}
  → {"id": "v4.36.0-54500a67~e41d…", "created": 1763990000,
     "against": ["v4.36.0-54500a67~9c0e…"], "paranoid": true,
     "accepted": true, "clean": true, "heads": ["v4.36.0-54500a67~4b7a…"],
     "audit": {…},
     "pairs": [{"spec": "v4.36.0-54500a67~9c0e…", "head": "v4.36.0-54500a67~4b7a…",
                "spec_module": "PisaSnap.s51c2…", "head_module": "PisaSnap.s0d9e…",
                "config": "comparator/1.json", "accepted": true, "exit_code": 0,
                "output": "…", "theorems": 1,
                "kernels": [{"name": "Lean paranoid", "verdict": "accepted"}, …,
                            {"name": "Lean default", "verdict": "accepted"}],
                "phases": [{"name": "exporting challenge", "seconds": 2.1, "peak_memory_mb": 1210}, …]}]}
```

`POST /snapshots/accept` audits the run as `GET /snapshots/audit?against=`
does, with the same `allow`, and runs `lake comparator` on a worker on
exactly the pairs the audit compared, each spec snapshot against each head
it reaches, one pair after another (see
[How a worker judges a pair](#how-a-worker-judges-a-pair)).

| Field | Default | |
| --- | --- | --- |
| `against` | required | The spec's snapshot ids, all of one version, as for the audit |
| `allow` | none | The definitions the heads may change besides the spec's holes, as for the audit |
| `paranoid` | `true` | Also run the external kernels Lean bundles (see [The kernels](#the-kernels)); `false` uses Lean's kernel only |
| `timeout` | 600 | Each pair's limit in seconds: its two exports and comparator together |

- **Async.** A pair that imports Mathlib takes about half a minute, or a
  minute paranoid: send the request with `Prefer: respond-async` and wait for
  its job (see [Async requests](api.md#async-requests-and-jobs) and
  [Progress](#progress)).
- **Unsettled runs.** A run whose audit lists `unsettled` heads, which hold
  only [through links](runs.md#through-links), is a 409 `conflict` before
  anything is judged: comparator would judge records that read `sorry`. The
  message names the `POST /snapshots/settle` to send first (see
  [Settling a run](runs.md#settling-a-run)). Nothing is stored.
- **Errors.** An empty `against`, ids of several versions, or an empty
  `allow` name is a 400 `bad_request`; a spec snapshot that doesn't exist is
  a 404.
- **Storing.** The acceptance is stored with its job's answer, and only
  then. A request lost with its worker (`worker_lost`) stored nothing:
  sending it again judges the run again, from the start (see
  [Lost jobs](api.md#lost-jobs)).

### The response

| Field | |
| --- | --- |
| `id` | `<version>~<32 hex>`, for `GET /snapshots/accept/{id}` |
| `created` | When it was judged, in seconds since the Unix epoch |
| `against`, `allow`, `paranoid` | What it judged, as the request gave them |
| `accepted` | The audit is `clean`, comparator accepted every pair, and there is at least one pair |
| `clean` | The audit's verdict |
| `heads` | The heads the audit and comparator judged |
| `audit` | The audit, as `GET /snapshots/audit?against=` answers it, with its `review` list at the default cap of 1,000 declarations |
| `pairs` | Each spec snapshot with each head it reaches, as comparator judged them, in the order of the audit's `comparisons` |
| `stale` | When read back: the run has changed since (see [Reading acceptances back](#reading-acceptances-back)) |
| `truncated` | What was left out so that the record fits its 6 MiB: `output`, each pair's, first, then `review` |

Each of `pairs`:

| Field | |
| --- | --- |
| `spec`, `head` | The spec snapshot and the head |
| `spec_module`, `head_module` | Their modules, `PisaSnap.s…`, which comparator judged as the challenge and the solution. They name the code it judged after the snapshots are deleted |
| `config` | The pair's configuration in the [export](#the-export), `comparator/<n>.json` |
| `accepted` | Comparator accepted the pair |
| `error` | Why not, `{kind, message}` (see [Why a pair isn't accepted](#why-a-pair-isnt-accepted)) |
| `exit_code` | Comparator's: 0 accepted, 1 refused or failed, 2 couldn't start. Absent when the pair ran past `timeout`, a signal killed comparator, an export failed, or comparator didn't run |
| `output` | The last 40 lines comparator printed; empty when it didn't run. A pair that ran out of time keeps what comparator printed until then |
| `kernels` | Each kernel's verdict, `{name, verdict, reason?}` (see [Verdicts](#verdicts)) |
| `theorems` | How many of the spec's theorems the configuration names |
| `left_out` | What the configuration leaves out, `{name, reason}` (see [Where the audit and comparator differ](#where-the-audit-and-comparator-differ)) |
| `phases` | Where the pair's time and memory went (see [Progress](#progress)) |

### Reading acceptances back

`GET /snapshots/accept/{id}` reads a stored acceptance again.
`GET /snapshots/accept?against=<id>` lists the acceptances that judged a spec
snapshot, newest first, at most 100; the spec snapshot may have been deleted
since. An id that isn't `<version>~<32 hex>` is a 400, and one that names no
acceptance a 404.

An acceptance keeps the audit and its review list, and names the code it
judged by module, so it stays readable when snapshots go. Read back, it says
`stale` once the run is no longer what it judged:

- its heads aren't the ones it judged, as a head was replaced or deleted
  since, or the run forked;
- a spec snapshot is amended, or deleted;
- a target is refuted that its audit didn't list.

A stale acceptance still says what was judged then. Accept the run again for
a verdict on it as it is now.

## The export

`GET /snapshots/export?against=<id>` (repeated, with `allow=` as for the
audit) answers the run as a Lake project in `pisa-run/`, as a tar.gz
(`pisa-run.tar.gz`). It is for judging the run with comparator outside pisa,
from source. A worker of the run's version builds it. `against` is required,
its ids of one version, else a 400, and a spec snapshot that doesn't exist is
a 404. The project holds:

- **The modules.** Every module the spec snapshots and their heads need,
  with its stored code. A pair whose spec is of a
  [named run](runs.md#naming-a-run) is named by the run: its head
  `PisaRun/<name>.lean`, and its spec, if it isn't the head,
  `PisaSpec/<name>.lean`. A forked run's modules keep their hash names, and
  so do the modules the pairs import, as `PisaSnap/s….lean`. Each file
  imports the others by these names: the export is a renamed build of what
  pisa built, so its module names, and private names with them, differ from
  pisa's build.
- **The Lake configuration.** The worker project's `lean-toolchain` and
  `lake-manifest.json`, and a `lakefile.toml` that requires what the project
  requires and builds the modules with `autoImplicit` off, as pisa builds
  them. When the modules import a library the project builds itself, because
  the project is that library's own checkout, the lakefile requires the
  project too, from the workers' `PISA_PROJECT_GIT` at `PISA_PROJECT_REV`;
  without them such an export is a 400 `bad_request` (see
  [The project](environments.md#the-project)).
- **`comparator/<n>.json`.** A configuration for each pair, numbered from 1
  in the order of the audit's comparisons.
- **`README.md`.** The toolchain; `lake exe cache get` first when the project
  requires Mathlib, as comparator builds in a sandbox without network and
  would compile Mathlib from source; each pair with its theorems and what it
  leaves out; and each file with the snapshots on its module and their
  labels.

Acceptance judges pisa's own builds with the same configurations, naming the
modules by their hash, as `spec_module` and `head_module`.

### A configuration

The spec snapshot's module is the challenge and the head's the solution:
`challenge_module` and `solution_module`, as comparator's configuration names
them.

- `theorem_names` are the spec's theorems, [witnesses](runs.md#witnesses)
  included, without what Lean generates;
- `definition_names` are the spec's holes and `allow`;
- `permitted_axioms` are `propext`, `Quot.sound` and `Classical.choice`, and
  the assumptions the spec permits, those declared in its module or in a
  module it builds on.

Each leaves out what the pair's `left_out` lists. Comparator replays the
proofs in Lean's kernel and, when paranoid, in the external kernels Lean
bundles.

### Judging it outside pisa

```sh
curl -s 'localhost:8000/snapshots/export?against=v4.36.0-54500a67~9c0e…' -o pisa-run.tar.gz
tar xzf pisa-run.tar.gz && cd pisa-run
lake exe cache get    # when the project requires Mathlib
lake build
lake comparator --paranoid --config comparator/1.json
```

`lake comparator` needs `bwrap`. `--paranoid` runs the same five external
kernels pisa does, with their own parallel workers, which can outgrow a
small machine; and for a pair whose spec permits an axiom of its own it
exits 1 when con-leche exits 2. To bound the workers, or for such a pair,
judge as pisa does: add the kernels to the configuration's
`external_kernels`, from the toolchain's `bin` (`<prefix>` is
`lean --print-prefix`). Leave out con-leche and con-ron for a spec axiom,
and their `--jobs=N` to let them use every core ([The kernels](#the-kernels)
says how pisa picks `N`):

```json
"external_kernels": {
  "Lean paranoid": ["<prefix>/bin/leanchecker-paranoid", "--silent", "--from-export"],
  "con-leche": ["<prefix>/bin/con-leche", "--jobs=3"],
  "con-ron": ["<prefix>/bin/con-ron", "--jobs=2"],
  "lean4lean": ["<prefix>/bin/lean4lean", "--import"],
  "nanoda": ["<prefix>/bin/nanoda_bin"]
}
```

Then run `lake comparator --config comparator/1.json`, without `--paranoid`.
`scripts/e2e.py` checks that an export builds with `lake build` and that
comparator accepts it there.

## Where the audit and comparator differ

Each pair's `theorems` and `left_out` say what its configuration couldn't
name, and `left_out` says why:

| Reason | What is left out |
| --- | --- |
| `private` | A private theorem, hole or assumption: private names differ between two modules. The audit compares them, reading a module's own private names alike |
| `private_in_statement` | A target whose statement uses a private name of the spec's module, directly or through the spec's definitions: comparator would refuse it |
| `not_a_definition_in_spec` | A hole or `allow` name that is an inductive type, a structure or an `opaque`: comparator takes as a hole only a `def` both sides declare. The audit allows it |
| `not_a_definition_in_head` | The same of the head, such as a name the head no longer builds on |
| `axiom_not_in_head` | A spec assumption the head no longer builds on: comparator needs each permitted axiom on both sides, and the head can't use it |

The other differences:

- **Definitions no statement uses.** Comparator compares what the theorems'
  statements reach; the audit compares every declaration of the spec.
- **A spec of definitions only** has no theorem for comparator to judge: the
  pair is `nothing_to_judge`, and the run isn't accepted. The audit compares
  its statements.
- **Heads comparator refuses that keep the statements**, such as code that
  opens `Classical` in its header, or whose auxiliary declarations differ:
  the audit is `clean` and the pair `refused`.
- **Axioms.** Comparator permits the standard three and the spec's
  assumptions; the audit's `not_proved` uses `PISA_ALLOWED_AXIOMS`.
- **Links' `allow`s** count for neither: both compare under the spec's holes
  and the request's `allow`.

## Progress

While an acceptance runs, its job's 202 at `GET /jobs/{id}` has `progress`,
`{phase, kernel?, seconds, peak_memory_mb?}`. The worker writes it each time
it renews its claim, every third of `PISA_LEASE_SECS` (10 s by default). All
of it is pisa's own, never read from what comparator prints.

```text
GET /jobs/3f9a…
  → 202 {"id": "3f9a…", "kind": "accept_run", "status": "running",
         "progress": {"phase": {"name": "judging", "pair": 1}, "kernel": "nanoda",
                      "seconds": 4.2, "peak_memory_mb": 2310}}
```

- **`phase`** is pisa's step. Pairs count from 1 in the order of `pairs`, as
  `comparator/<n>.json` do.

  | Phase | |
  | --- | --- |
  | `{"name": "linking"}` | Linking the modules the pairs need, each checked against its record |
  | `{"name": "waiting"}` | Waiting for a slot, and for the slot's memory to be freed |
  | `{"name": "exporting", "pair": n, "side": "challenge"}` | Exporting one side of pair `n`, `challenge` or `solution` |
  | `{"name": "judging", "pair": n}` | Comparator judging pair `n` |

- **`kernel`** is the kernel comparator is running, named as in `kernels`,
  which the worker finds by its executable among the processes in the slot's
  cgroup (`leanchecker` is `Lean default`). It is absent while comparator
  reads and compares the exports, and between kernels.
- **`seconds`** is the time in the phase, with that kernel, and
  **`peak_memory_mb`** the most memory the slot's cgroup held in that time,
  page cache included, as its memory limit counts it. The worker starts
  `memory.peak` again at each phase and each kernel, which needs Linux 6.12
  or later. Linking and waiting come before the slot, and have no
  `peak_memory_mb`.

A worker without cgroups (`PISA_CGROUP_ROOT` unset) gives neither `kernel`
nor `peak_memory_mb`.

Each pair keeps its phases in `phases`, each
`{name, seconds, peak_memory_mb?}`: `exporting challenge`, `exporting solution`, `judging`
for comparator reading and comparing the exports and running any kernel the
worker didn't see, and each kernel the worker saw, by its name. The worker
looks at the slot about each second, so a kernel shorter than that may have
no phase of its own and count in `judging`, a kernel's `seconds` are good to
about a second, and a peak right at a boundary may count in the next phase.

`progress` stays while the claim has lapsed, and goes when another worker
takes the job, until that one says. If the claim moves to another attempt,
as when a stall outlasts the lease and another worker claims the job, the
first worker stops comparator as soon as it sees that, kills what is left in
its slot, and stores nothing: the run has the other attempt's acceptance
only.

## Why a pair isn't accepted

A pair not accepted says why in `error.kind`, with comparator's last error,
or what kept it from running, in `error.message`:

| Kind | |
| --- | --- |
| `refused` | Comparator judged the head and refused it: a statement or a definition differs, an axiom isn't permitted, or a kernel rejected it (exit 1) |
| `export_failed` | pisa couldn't export a module from its build, so comparator didn't run |
| `timeout` | The pair ran past `timeout`, its exports and comparator together |
| `out_of_memory` | The worker's memory limit killed the exporter, comparator or one of its kernels (con-ron's parallel workers outgrow a slot, for one); the message names the limit |
| `cannot_run` | Comparator couldn't start or run its checks: a missing `bwrap`, a configuration it couldn't read, a kernel it couldn't run or that exited with a code that is no verdict, or a signal that killed comparator and wasn't the time limit's or the memory limit's |
| `nothing_to_judge` | The spec names no theorem comparator can judge, so it didn't run |
| `unsupported` | Comparator's last error line says con-leche or con-ron, which pisa ran, exited with 2: they don't support a non-standard axiom |

Only `refused` is a verdict on the proofs: a kernel that was killed or
couldn't run never makes one. Only comparator's exit code decides whether a
pair is accepted, never what it printed, which a kernel's output and the
names in the solution's declarations can imitate. A worker that can't run
the exporter at all, rather than an export that fails, fails the request
with a 500 `internal`.

## A Palomar package

Palomar is a registry of Lean formalizations, each a Lake package that
comparator judges: a challenge, a solution, `comparator.json`, and metadata
in `formalization.yaml`. `profile=palomar` exports one pair of the run as a
Palomar package instead of the Lake project, in `palomar/`
(`palomar.tar.gz`):

```sh
curl -s 'localhost:8000/snapshots/export?against=v4.36.0-54500a67~9c0e…&profile=palomar&pair=1' -o palomar.tar.gz
```

| Parameter | |
| --- | --- |
| `profile` | `palomar`, the only profile |
| `pair` | The pair to package, from 1 as `comparator/<n>.json` numbers them; needed unless the run has one pair |
| `challenge_module` | The challenge's module name, `Challenge` by default |
| `solution_module` | The solution's module name, `Solution` by default |

`pair`, `challenge_module` and `solution_module` need the profile. A module
name is dotted parts, each a letter or `_` then letters, digits and `_`, and
none a Lean keyword; the two names differ. The package holds:

- **The modules.** The pair's spec module is the challenge and its head's
  the solution, named as `challenge_module` and `solution_module` say; the
  modules they import of the run keep their names. Each plain file becomes a
  module-system file, as Palomar requires: `module` first, its imports
  `public`, and `@[expose] public section` after them. A module-system file
  stays as it is. Imports follow the new names.
- **`lakefile.toml`**, named by the run (its name with `_` for `.`, or
  `pisa_run`), with a `version`, the challenge's and solution's libraries as
  `defaultTargets`, `autoImplicit` off, and Mathlib alone required, at the
  commit the worker's manifest resolved. **`lake-manifest.json`** is the
  worker's without its other packages (Physlib, for one), each package's
  `inputRev` its commit, and **`lean-toolchain`** the worker's. A package of
  the worker's project that isn't a GitHub repository at a commit is a 500
  `internal`: it is the worker's project, not the request.
- **`comparator.json`**, the pair's configuration, with the new names.
- **`formalization.yaml`** (Palomar's v0.4, written as JSON, which YAML
  reads) has every key of a Palomar package's: the run's name, the theorems
  the solution proves, and in `status` no `sorry` and no axiom beyond the
  standard three, which `comparator.json` makes true of a solution it
  accepts. Everything else, the licence, authors and maintainers,
  description, classification, sources and the rest, reads `REVIEW`.
- **`REVIEW.md`** lists what to decide before publishing: the licence,
  authors, attribution, fidelity to the source, the rest of the metadata,
  and validation.
- **`conversion.json`** names the pair, each module's SHA-256 before and
  after, and the packages removed.

A 400 `bad_request` names what a package can't hold:

- a spec that permits an axiom beyond the standard three;
- a challenge that imports anything but `Mathlib`, `Init` and `Lean`;
- a spec that is its own head, with no solution yet;
- a pair that imports a module that is neither the run's, Lean's (`Init`,
  `Lean`, `Std`, `Lake`), Mathlib's nor of a package Mathlib builds on
  (`Batteries`, `Aesop`, `Qq`, `ProofWidgets`, `Plausible`, `ImportGraph`,
  `LeanSearchClient`, `Cli`). That is an allowlist: the worker's Physlib,
  `QuantumInfo`, `PhyslibAlpha` and `Cslib`, and whatever else the worker's
  project adds, are refused;
- a module the package keeps that already has the name `challenge_module` or
  `solution_module` gives.

New headers and names change what Lean builds: build the package, judge it
with comparator, and cross-check it in pisa before publishing it.

## How a worker judges a pair

Reference for operators: what `POST /snapshots/accept` runs on a worker, and
how it reads the kernels' verdicts.

### The steps

A judgment takes one REPL slot of a worker, and judges the run's pairs in
turn:

1. **Linking.** The worker links the module of each spec snapshot and head,
   and every module it builds on, into a directory of the judgment's own:
   the files pisa built and kernel-checked when the snapshots were created,
   each checked against the SHA-256 its record holds. A snapshot deleted
   after that takes nothing from the judgment, nor from the review list kept
   with it.
2. **Waiting.** It waits for a slot, and frees the slot's memory, as the
   exporter and the kernels load the modules.
3. **Exporting.** It exports each pair's two modules with the toolchain's
   `leanexport`: the targets comparator would export, the theorems, axioms
   and definition holes the configuration names and the constants the
   kernel builds in. The exports are kept by what they are made of (the
   modules' builds, the targets, the exporter and the pool version), the 64
   used last, so a module judged again isn't exported again.
4. **Judging.** Comparator judges the pair on the two exports
   (`--challenge-from-export`, `--solution-from-export`), with a
   configuration naming the modules by their hash. It builds nothing and
   loads no project, so no code of the run runs while it is judged, and
   nothing a pair or a judgment does reaches another.

### The exporter and Lake

- **The exporter** runs in pisa's sandbox and the slot's cgroup, with
  nothing writable but its output, the judgment's modules on its search path
  in the snapshot cache's place, `LEAN_SYSROOT` the toolchain's prefix, so
  that it never runs a `lean` it finds to ask for one, and
  `LEAN_ABORT_ON_PANIC=1`, so that a target it can't find fails the export
  instead of leaving it out. `PISA_LEAN_EXPORT` names it (default
  `<prefix>/bin/leanexport`, `<prefix>` being `lean --print-prefix` in the
  project).
- **Comparator** runs in the slot's cgroup and outside pisa's sandbox, as it
  sandboxes the kernels itself with `bwrap`. It has only `PATH`, `HOME`,
  `ELAN_HOME`, `LANG` and `TMPDIR` of the worker's environment, and runs in
  the judgment's directory, where a copy of the project's `lean-toolchain`
  has elan pick its `lake`. `PISA_LAKE` names `lake` (default `lake`).
- **Comparator's sandbox** needs unprivileged user namespaces, its own
  `/proc`, and `/run/user`, which the worker image creates. A worker whose
  `bwrap` can't make it warns as it starts that acceptances can't run there.
- **Cleanup.** A judgment's directory goes when it is done, and any left at
  once when the worker starts. The exports are kept in the work directory's
  `exports/` (`PISA_WORK_DIR`) across restarts.

### The kernels

With `paranoid`, comparator also runs the five kernels Lean bundles. Pisa
names them in the configuration's `external_kernels`, from the toolchain's
`bin`, and never passes `--paranoid`, which names the same five but can't
pass them arguments. Lake runs them sorted by name, Lean's own kernel last:

| Kernel | Command |
| --- | --- |
| Lean paranoid | `leanchecker-paranoid --silent --from-export` |
| con-leche | `con-leche`, with `--jobs=N` under a memory limit |
| con-ron | `con-ron`, with `--jobs=N` under a memory limit |
| lean4lean | `lean4lean --import` |
| nanoda | `nanoda_bin` |
| Lean default | `leanchecker`, Lake's own, which runs without `paranoid` too |

- **Workers.** con-leche and con-ron check declarations in parallel, a
  worker for each core, and a worker takes gigabytes: con-ron about
  6250 MiB, con-leche about 3790 MiB. On a computation-heavy proof a dozen
  workers outgrow a slot and the kernel is killed, which Lake prints as
  `rejected`, as it does for any non-zero exit. So when the slot has a
  memory limit (`PISA_MEMORY_MAX_MB`), pisa passes them `--jobs=N`, one
  argument (`--jobs N` exits 3): the limit less 3072 MiB, over a worker's
  memory, at least 1. A 16000 MiB slot runs con-ron with `--jobs=2` and
  con-leche with `--jobs=3`; a slot without a limit passes no `--jobs`.
- **A spec axiom.** con-leche and con-ron don't support a non-standard axiom
  and decline it, so when the spec permits one, pisa leaves them out of
  `external_kernels`.

### Verdicts

A pair's `kernels` list each kernel's verdict, `{name, verdict, reason?}`, in
Lake's order. For an accepted pair, they are what pisa configured, each
`accepted`, with con-leche and con-ron `unsupported`, and a `reason`, when a
spec axiom left them out; without `paranoid`, `Lean default` alone. For a
pair comparator failed, they are as comparator printed and exited them, for
the reader.

| Verdict | |
| --- | --- |
| `accepted` | The kernel checked the solution and accepts it |
| `rejected` | It exited with 1, which a kernel does to reject a proof |
| `unsupported` | It doesn't support the non-standard axiom the spec permits: pisa didn't run it, or it declined the proof (exit 2) |
| `cannot_run` | It couldn't run, or exited with a code that is no verdict |
| `failed` | It exited non-zero after the first kernel that failed, which is the only one whose code comparator reports |
| `out_of_memory` | The worker's memory limit killed it |

All but `accepted` and `rejected` are no verdict on the proof. Lake's final
error line, `error: <kernel> exited with N`, reports the code of the first
kernel that failed only. When comparator exits 1, pisa reads it from the
last such line that names a kernel it configured, exactly. For that kernel:

- 1 is `rejected` and the pair `refused`, unless the kernel's first line is
  bwrap's own (`bwrap: …`): bwrap exits 1 when it can't make the kernel's
  sandbox, and that is `cannot_run`;
- 2 for con-leche or con-ron is `unsupported`;
- 128 or more while the slot's out-of-memory count moved is `out_of_memory`;
- any other code (3, a kill without it, 42) is `cannot_run`. A kill is never
  `refused`.

Each other kernel is `accepted` if Lake printed that it accepts the
solution, `failed` if Lake printed that it rejected it, as there is no code
to say more, and `cannot_run` if Lake couldn't interact with it; a kernel
Lake printed nothing of isn't listed. A kernel that didn't accept has its
own first line in `reason`, cut at 200 characters, such as
`con-ron: declined: non-standard axiom ...`; a kernel that was killed has
none. A line of the output can be imitated by the solution, so these are for
the reader and never settle a pair, which only the exit code does.
