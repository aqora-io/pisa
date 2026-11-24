# Concepts

This page says what pisa does, how a request runs through it, and what the
words the rest of the documentation uses mean. Read it first if you are new
to pisa. The [glossary](#glossary) defines each term in a sentence or two
and links the page that covers it.

## What pisa does

pisa is a server that checks Lean 4 code for programs: proof-search agents,
the tools that run them, and the people who build those tools. It runs
unmodified upstream [Lean REPL](https://github.com/leanprover-community/repl)
processes, and Lean Beam for code kept open. With it you can:

- **Check code.** `POST /check` answers a verdict, `ok`, from Lean and from
  the axioms each new declaration depends on, not from string matching,
  with Lean's messages, and on request the goals at positions and every
  tactic Lean ran. See [Checking code](checking.md).
- **Work on one proof many times.** Open it as a [document](#document) to
  ask for goals at positions and try candidate tactics at a `sorry`, or
  start a [session](#session-and-state) to prove its `sorry`s a tactic at a
  time. See [Documents](checking.md#documents) and
  [Tactic sessions](checking.md#tactic-sessions).
- **Build a library incrementally.** Accepted code is kept as a
  [snapshot](#snapshot-and-module), a compiled module that later code
  imports instead of re-checking it. See [Snapshots](snapshots.md).
- **Run agents against a spec.** A leader keeps the statements to prove as
  spec snapshots, agents replace them with proofs, and pisa audits the
  [run](#run) from its records and accepts it with
  [comparator](#comparator). See [Runs](runs.md) and
  [Acceptance](acceptance.md).
- **Search declarations** of Mathlib, Physlib, CSLib and snapshots, from a
  description, LaTeX, a Lean pattern or goal, or a partial name. See
  [Search](search.md).

## How a request runs

- **Servers and workers.** A server puts each request's [job](#job) on a
  queue in the database, sessions and documents ahead of checks and
  snapshot builds. Workers claim jobs, preferring those whose document they
  hold open or whose [header](#header) they have warm, and a job whose
  worker dies runs on another. Servers hold no state, so they run in
  replicas. See [Operations](operations.md).
- **Pools by version.** A worker serves one Lake project, and its
  [version](#version) names that project. Workers of one version are a
  [pool](#pool), and a request reaches a pool by naming its version. Several
  pools share the servers, database and store. See
  [Versions and environments](#versions-and-environments).
- **Hardened workers.** Each Lean process runs in its own [slot](#slot).
  Where the deployment turns them on, as Compose and the chart do, a slot
  has a bubblewrap sandbox and cgroup v2 limits; the worker binary alone
  runs without either (see [Configuration](configuration.md)). Code that
  uses metaprogramming or unsafe options is refused before it runs, and
  proofs are judged by the axioms they depend on (see
  [The verification policy](checking.md#the-verification-policy)).
- **Sessions.** A session is a document open on Lean Beam, and a state is a
  tactic run from a `sorry` or from another state, so tactics see what the
  session's own code declares. The states' records are kept in the
  database, so any worker can continue a session by replaying them. A final
  check runs the whole proof again from scratch and confirms it proves the
  original statements, with the original imports.
- **Snapshots.** A snapshot's module is built once, re-checked by the
  kernel in a clean process unless the create opts out, and kept in the
  [store](#store); its records are in the [catalog](#catalog). Code that
  imports it loads the compiled module.

## Versions and environments

A version names an [environment](#environment): a Lean toolchain and the
Lake project its workers build, labelled `<toolchain>-<hash of the
project's files>`, such as `v4.36.0-54500a67` (Lean v4.36.0 with Mathlib,
Physlib and CSLib). A request names a version, or an alias of one, and
otherwise gets the server's default. Snapshot, document, session and state
ids carry their version, so requests about them name none. A check's result
says which version ran it.

`GET /environments` lists every version a worker has registered, with its
toolchain, the packages it pins and the project's files, whether or not it
has live workers (see [`GET /environments`](api.md#get-environments)).
Another toolchain or library is another environment, with a pool of its own
on the same servers: [Environments](environments.md) says how to build one.

## Glossary

### Code and checks

#### Check

One run of code through Lean on a worker, judged by the
[verification policy](checking.md#the-verification-policy). Its result's
`ok` is true when the code elaborates without errors, passes the policy
scan, and each new declaration depends only on the allowed axioms. See
[Checking code](checking.md).

#### Header

The lines at the top of the code that say what it imports: an optional
`module` line, then the `import` lines. A worker keeps Lean processes with
a header's imports loaded, so later code with the same header runs only its
body.

#### Verification policy

What a check requires besides Lean's acceptance: the standard axioms only,
no metaprogramming, and only allowlisted options. See
[The verification policy](checking.md#the-verification-policy).

#### Status

What a declaration's axioms make it: `proved` (the allowed axioms only),
`conditional` (assumptions too), `sorry` or `rejected`. See
[Statuses and assumptions](runs.md#statuses-and-assumptions).

#### Assumption

An `axiom` declared in snapshot code, or in a snapshot the code builds on. A
declaration that uses assumptions and nothing worse is `conditional`. See
[Statuses and assumptions](runs.md#statuses-and-assumptions).

### Documents and sessions

#### Lean Beam

Lean's `lean-beam-mcp`, shipped with toolchains from Lean v4.36, which keeps
an elaborated file open and answers goals and runs tactics against it.
Workers run it in Beam slots, for documents and sessions. See
[Documents](checking.md#documents).

#### Document

Code a worker keeps open on Lean Beam, so that goals at positions and
candidate tactics need no new elaboration. An edit makes a new revision.
See [Documents](checking.md#documents).

#### Session and state

A session is a document opened from code with `sorry`s, each of which
starts a state: the goals left to prove. A tactic on a state gives a new
state, and earlier states stay valid, so a session is a tree. See
[Tactic sessions](checking.md#tactic-sessions).

### Snapshots and runs

#### Snapshot and module

A snapshot is code kept for later code to import. Its module
(`PisaSnap.s…`) comes from a hash of the code and is what code imports; the
snapshot (`<version>~<32 hex>`) is a reference to the module with labels of
its own. See [Snapshots](snapshots.md).

#### Catalog

The database's records of snapshots: their modules and what each imports,
the snapshots and their labels, what replaces what, run names and
acceptances, per version. The servers answer snapshot reads from it
without a worker. See [How it works](snapshots.md#how-it-works).

#### Store

Where the compiled modules are kept: an S3-compatible object store (Garage
in Compose), which every worker of a version must share, or by default the
worker's own memory, which no other worker reads. See
[Store](configuration.md#store).

#### Label

A key and a string value on a snapshot, at most 64 of them, to list, audit
and rebuild snapshots by. See [Labels](snapshots.md#labels).

#### Verified

Said of a module that the kernel re-checked in a clean process, with every
module it imports, so that its statuses don't depend on the build being
honest. See [Verifying](snapshots.md#verifying).

#### Run

A spec snapshot and the snapshots that replace it, directly or through
others. A run may have a name, which code imports as
`import PisaRun.<name>`. See [Runs](runs.md) and
[Naming a run](runs.md#naming-a-run).

#### Spec

A snapshot whose statements a run is to prove, usually a skeleton with
`sorry`s. Its theorems are the run's targets. See
[Running agents on snapshots](runs.md#running-agents-on-snapshots).

#### Target

A theorem of a spec, which the run's heads must prove with the same
statement. See
[Auditing a run against its spec](runs.md#auditing-a-run-against-its-spec).

#### Head

A snapshot that nothing replaces. A run's heads are its latest state, and a
create may replace only a head. See
[Replacing only a head](snapshots.md#replacing-only-a-head).

#### Hole

A definition a spec leaves for the snapshots that replace it to change,
named in `holes` at its create. See [Holes](snapshots.md#holes).

#### Fork

A snapshot replaced more than once, which a create allows with
`fork: true`, for parallel attempts at one target. An audit lists forks.
See [Replacing only a head](snapshots.md#replacing-only-a-head).

#### Leader

The client that runs a run: it creates the specs, hands their targets to
agents, and audits and accepts the run. (Search also has a leader: the one
server replica that indexes snapshots.) See
[Running agents on snapshots](runs.md#running-agents-on-snapshots).

#### Audit

`GET /snapshots/audit`: what snapshots rest on, read from their records by
the server, with no worker. Against a spec, it compares each head with the
spec and says whether the run is `clean`. See
[Auditing a run against its spec](runs.md#auditing-a-run-against-its-spec).

#### Amend, refute, rebuild and settle

A leader amends a spec by creating a corrected one with `amends`, which
supersedes it. An agent refutes a target by keeping a proof of its
negation. A rebuild copies a snapshot's dependents onto its replacement,
and a settle rebuilds a run's heads onto the proofs their targets are read
through. See [Amending](runs.md#amending-a-spec),
[Refuting](runs.md#refuting-a-target),
[Rebuilding](runs.md#rebuilding-dependents) and
[Settling](runs.md#settling-a-run).

#### Comparator

`lake comparator`, Lean's tool that judges whether a solution proves a
challenge's theorems with the same statements and only permitted axioms,
from exports of the two modules, with Lean's kernel and, paranoid, other
kernels too. Accepting a run runs it on each spec and head. See
[Acceptance](acceptance.md).

#### Export

`GET /snapshots/export`: a run as a Lake project, for judging it with
comparator outside pisa. See [Acceptance](acceptance.md).

#### Palomar

Lean formalizations packaged for comparator, each a Lake package with a
challenge, a solution, comparator's configuration and metadata in
`formalization.yaml`, its modules module-system files that require Mathlib
alone. `GET /snapshots/export?profile=palomar` packages one pair of a run
so. See [A Palomar package](acceptance.md#a-palomar-package).

### Running pisa

#### Version

The label of an environment and of its workers' pool, such as
`v4.36.0-54500a67`. Ids and check results carry it. A search bundle's
manifest calls it `env_id`. See [Environments](environments.md).

#### Environment

The Lean toolchain and Lake project behind a version, which
`GET /environments` lists. See [Environments](environments.md).

#### Pool

The live workers of one version. `/health` lists each pool with its slots,
toolchain and packages. See [Environments](environments.md).

#### Slot

A Lean process on a worker, in a sandbox and cgroup of its own where the
deployment sets them up: `PISA_SLOTS` REPL slots for checks, snapshot
builds and acceptances, and `PISA_BEAM_SLOTS` Lean Beam slots for documents
and sessions. See [Worker settings](configuration.md#worker).

#### Job

A request's work on the queue in the database, which one worker claims and
holds with a lease. An async request answers a job's id at once, and
`/jobs/{id}` answers its result. See
[Async requests and jobs](api.md#async-requests-and-jobs).

#### Scope

Labels a request sends in the `Pisa-Scope` header, which keep it to one
tenant: what it creates carries them, and what it reads must have them.
See [Scopes](api.md#scopes).

#### Bundle

A search index as files, built with `pisa-search build` and loaded into
Qdrant with `pisa-search load`. See [Search indexes](search-index.md).

#### Qdrant, Garage, Traefik and KEDA

Services pisa runs with. Qdrant holds search's index. Garage is the
S3-compatible store and Traefik the load balancer in the Compose stack.
KEDA scales the chart's workers by the jobs waiting on the queue. See
[Docker Compose](deploy-compose.md) and [Kubernetes](deploy-kubernetes.md).
