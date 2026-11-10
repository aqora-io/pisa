# Lean Beam against the REPL, in the worker's sandbox

This is the spike in the review's second work item (L1, with C7 and C10 to C12). Lean v4.36.0-rc2, which `main` pins, ships Lean Beam: `lean-beam-mcp` serves tactic probes at file positions, chained speculative states, goals and a file's remaining `sorry`s over JSON-RPC on stdio.

The spike asks two things:
- Can Beam serve pisa's interactive work (sessions, goals at positions, candidate attempts) inside the worker's sandbox?
- How does it compare with the REPL on the same code?

Acceptance (whole-file checks, the axiom report, the kernel re-check) was not meant to move, and the results below say it shouldn't.

## Setup

- **Code:** `crates/pisa-beam` (the client) and `crates/pisa-beam-bench` (this bench).
  - Both backends start through the worker's own `Launcher`, so they run with the worker's bubblewrap arguments, environment and per-slot cgroups.
  - Beam also gets a writable `HOME` and session directory in the slot.
  - Each measurement uses a fresh slot, so its cgroup counters are its own.
- **Image:** `pisa-worker:lean-project` (`sha256:6b01e8d8…`): Lean, Mathlib and the REPL v4.36.0-rc2, Physlib and PhyslibAlpha from aqora-io/physlib `lean-v4.36.0-rc2`, version `v4.36.0-rc2-1e541805`.
  - Beam is lean-beam-mcp 0.2.0, from the toolchain.
  - The bench was built in `rust:1.99-trixie` and mounted into a fresh privileged container (`--cgroupns private --cpus 8`, `PISA_SANDBOX=bwrap`, `PISA_CGROUP_ROOT=/sys/fs/cgroup`).
- **Machine:** AMD Ryzen 9 7940HS (16 threads, 8 given to the container), 59 GB of RAM.
- **Modules:** the eight of `bench/infotree/2026-11-06`, each elaborated as a body on its own imports.
  - The document is `header_command(header)`, the header the worker sends, followed by the body.
  - The REPL gets the same header as its own command, then the body.
- **Workload** (`workload.json`): every `theorem` or `lemma` proved as `:= by` plus an indented block, 198 blocks and 652 steps in all.
  - A line at the block's indentation starts a step; `<;>` and match arms continue one.
  - Each block is replaced by `sorry` in the "sorried" body.
  - The modules compile, so every block is a correct proof.
- **Commands:** `beam-bench probe`, then `workload`, then `all`. The phases are `session`, `goals`, `replay`, `candidates` and `concurrency`. Raw results are the `*.json` files next to this report.

Times are single runs, not the median of two as the plan asked; every phase took seconds to minutes. Memory is the slot cgroup's `memory.peak`. It counts only the pages a slot touched first, so it understates whichever backend read a library after the other.

## Probe (`probe.json`)

| Check | Result |
| --- | --- |
| Beam under the worker's bwrap arguments, unchanged | works: handshake, `update`, `goals`, `run_at`; 0.5 s on a bare workspace |
| A workspace over the project (`.lake/packages` symlinked, or an absolute `packagesDir`) | both work, with no Lake build or write. A document with Physlib's header and `meta import Pisa` syncs in 3.7 to 3.9 s; the REPL imports the same header in 1.2 s |
| `import Pisa` and a compiled `PisaSnap` module through the inherited `LEAN_PATH` | both resolve |
| `lean_save` in that workspace | writes `BeamDocs.P`'s `.olean`, `.ilean` and trace |
| The axiom report as a command probe, read by the worker's `parse_report` | identical to the REPL's on `Operators.Momentum` (17 declarations) and `Vector.Basic` (69). On `Space.Basic` both fail the same way (below) |
| Documents in one process | one to four documents: 361 MB to 1.9 GB, 52 to 94 pids (each document has its own file worker and imports its header again) |
| 40 probes at once | 8 refused as busy (Beam's default limit is 32 unfinished requests), none lost |
| A probe cancelled by its timeout | the process stays responsive; the next probe takes 14 ms |

A command probe runs in command mode only at the start of a following command. The lines after a `by` block still belong to it. So documents end with an empty `section`/`end`, and the report is probed there (`pisa_beam::pos::anchored`).

## Results

### Session start (`session.json`)

The REPL path is a fresh process, the header import, the sorried body, the report, and a pickle per `sorry`. The Beam path is a fresh process, `sync` of the sorried document, then `todo` for the `sorry`s and their goals.

| | REPL | Beam, fresh | Beam, same document with one proof filled |
| --- | ---: | ---: | ---: |
| Total, across the eight modules | 1.6 to 5.6 s (median 2.4) | 3.9 to 6.7 s (median 4.5) | 0.29 to 2.9 s |
| Peak memory | 261 to 410 MB | 482 MB to 1.26 GB | |

Beam has no warm header to reuse. A fresh document imports its header again, which costs about 3 s more than the REPL's import. Re-elaborating an open document after an edit is cheap.

### Goals at positions (`goals.json`)

The positions are the start of each step, at most 64 per module, 403 in all.
- **REPL:** elaborates the body with the `"original"` tactic tree and reads every position from it. It elaborates again for every request, because the tree is dropped (`check.rs:103`).
- **Beam:** syncs once, then asks `goals before` at each position.

| | REPL | Beam |
| --- | ---: | ---: |
| First answer for a module | 0.8 to 13.1 s (elaborate with tree) | 3.9 to 6.7 s (sync) |
| Each further question | the same again | p50 2.0 to 6.5 ms, p90 at most 13.8 ms, max 21 ms |
| After a one-line edit | the same again | sync 0.27 to 2.1 s |

Where the two agree, and where they don't, over the 403 positions:
- 259 are identical and 51 differ only in line breaking. The REPL indents a goal's continuation lines by four, the server by two.
- At 84 (21%) the REPL's tree has no tactic, so it returns nothing; Beam answers.
- At 9 (2%) they answer for different tactics. The REPL takes the outer step; Beam gives the goals inside a case split started on the same line.

### Tactic replay (`replay.json`)

Each block's steps are applied one at a time from its `sorry`:
- **REPL:** `Tactic`, then `PickleProofState`, which is the real session step.
- **Beam:** `run_at_handle` at the sorry, then `run_with_linear`.

| | REPL | Beam |
| --- | ---: | ---: |
| Blocks replayed to a closed goal | **10 of 198** | **175 of 198** |
| Steps that succeeded | 97 of 652 | 578 of 652 |
| Step latency, per module p50 | tactic 1.0 to 11 ms; with pickle 3.2 to 21 ms | first step 3.4 to 15 ms, later steps 2.7 to 37 ms |
| The 10 blocks both replay | `Unbounded`: p50 25.8 ms with pickle; `PhysHermite`: 10.9 ms | 18.8 ms; 13.5 ms |

The REPL's failures aren't the workload's. Running the REPL by hand on `IsDistBounded` confirmed it:
1. The sorried body elaborates with no errors and 41 sorries.
2. `#check @Space.IsDistBounded` in the env after the body succeeds.
3. A `Tactic` on the first sorry's fresh proof state answers `Lean error: Unknown constant 'Space.IsDistBounded'`.

So the REPL's tactic mode can't see the file's own declarations, even before any reload. The cause is how the body was sent, not the module system:
- The bench sends the whole body as one REPL command on the header's environment, as pisa's sessions do, and the REPL makes each sorry's proof state on the environment that command started from.
- A plain file fails the same way. With `def foo : Nat := 1` and then `theorem t : foo = 1 := by sorry` sent as one command, `decide` on the state fails on `foo`; sent as two commands it works, as the review measured by hand with the REPL of the 4.35.0 worker image (C14). The end-to-end script checks the first on 4.36.0-rc2, and that the same theorem started on a snapshot that declares `foo` works.
- The eight modules here are all module-system files, so the bench couldn't tell the two causes apart.

The replay row therefore measures this way of sending the body, not what the REPL could do with the body split into commands. Splitting it would serve fresh states, but a reload on another worker would then have to rebuild the environment before the sorry. This is not C10's reload problem either, and C10's replay would not cure it. pisa's sessions now warn when their code declares something before a `sorry`.

Beam's 23 unclosed blocks are in `Derivatives.Basic` (18), `IsDistBounded` (4) and `Space.Basic` (1). They weren't investigated. `IsDistBounded`'s body elaborates without errors under the worker's header, so the `autoImplicit` errors below don't explain all of them.

### Candidates (`candidates.json`, `concurrency.json`)

Four blocks per module, six candidates each: `simp`, `omega`, `norm_num`, `aesop`, the block's first step and the whole block, 192 in all.

| | REPL | Beam |
| --- | ---: | ---: |
| T1: a tactic on a held state vs a probe at the sorry; closed the goal | **0 of 192** (the failure above) | **41 of 192** |
| T1 latency, per module | p50 0.4 to 2.6 ms (immediate failures) | p50 3.9 to 25 ms, p90 15 to 88 ms, max 2.0 s |
| T2: the body with the candidate, then the report (one `/check/batch` item) | p50 0.66 to 4.6 s | edit, sync, report probe: p50 0.46 to 3.3 s, 1.0 to 2.4 times faster |
| T2 reports equal | | 24 of 32 |

The 8 unequal reports, in `TemperedDistribution` and `PhysHermite`, have the same declarations, axioms and type and value hashes. They differ only in names that carry the module's name:
- a private name: the REPL's `_private.0.…`, Beam's `_private.BeamDocs.Doc.0.…`;
- an instance Lean names: `…Real` against `…Real_beamDocs`.

A file always has a module name, so Beam's report can't stand in for the REPL's in acceptance without mapping names.

**Concurrency on one document** (`Operators.Momentum`, 24 probes): 10.2 probes a second one at a time, and 11.9 to 12.0 at 4, 16 and 32 in flight. Probes on one document barely run in parallel. Throughput would have to come from several documents or processes, at about 0.5 GB each.

## Against the proposed thresholds

| Threshold | Result |
| --- | --- |
| Beam runs in the unchanged sandbox | met |
| Warm goals within 100 ms at p95 | met: p90 at most 13.8 ms, max 21 ms |
| A step no slower than the REPL's tactic plus pickle | met where both work (18.8 against 25.8 ms on `Unbounded`); the single `PhysHermite` block is 13.5 against 10.9 ms. Mostly moot: the REPL replays 10 of 198 blocks with the body sent as one command |
| Candidate throughput at least twice the REPL's at equal memory | not shown. Per document, probes don't scale with concurrency. The REPL's exploration path fails on this code sent as one command, so there is no throughput to compare; acceptance-style items are 1.0 to 2.4 times faster on Beam |
| Goals equal at 99% of positions | not met: 77% equal or equal up to layout. Most of the rest is the REPL answering nothing (21%); 2% are different tactics |
| Reports equal | not met as a stand-in for acceptance (24 of 32, only module-derived names differ). Met for reading statuses in interactive work |

## Recommendation

- **Use Beam for the interactive path.** That covers sessions and tactic steps, goals at positions and candidate exploration, the jobs that work on a file the client keeps editing.
  - On these files, sent as pisa sends a session's code, it is the only one of the two that works: 175 against 10 proofs replayed.
  - It answers goals in milliseconds once a document is open, and re-elaborates an edit in under 2 s.
- **Keep acceptance where it is.** Whole-file checks, the axiom report for verdicts, snapshots and the kernel re-check stay on the REPL and `lean`.
  - Beam's reports name private and Lean-named declarations after the document's module.
  - Its probes skip the policy scan, termination checking and new `let rec`s.
  - Its saved artifacts aren't certified equal to a batch build.
- **Don't spend the medium REPL fixes on this.** C10's session replay doesn't address the failure measured here. C12's shared prefixes is what an open Beam document already is.
- **What adoption (size L) has to design:**
  - Placement: about 0.5 GB per open document and no parallelism within one, so several documents per process, or several processes, by load.
  - Sessions that survive restarts and moves between workers. Handles die with the process; replaying the stored steps costs a few milliseconds each here.
  - The policy scan on every probe text.
  - The 3 s higher cost of a fresh document compared with a warm REPL header.
  - Beam is beta (0.2.0) and its interfaces may change.

## Other findings

- **`autoImplicit`:** the worker's header sets `autoImplicit false`. Three of the eight modules, `Space.Basic` and `Vector.Basic` among them, rely on it and gain "Unknown identifier `d`" (or `M`) errors under that header. Both backends get the same documents, so this is not a difference between them.
- **The axiom report and error recovery:** on `Space.Basic`, whose body has such errors, the report fails on both backends with `describing Space.instAddTorsorEuclideanSpaceRealFin: a metavariable`, and no `PISA_DECLS` is printed. The worker skips the report on a body with errors, so this only shows here. It is a lead for the report code (work #1).
- **Tactic positions in the REPL's tree:** at 21% of tactic starts, the REPL's `"original"` tree has no tactic node.
