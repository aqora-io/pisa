# Work #6: the `pisa` tool, re-checked creates, and catalog reads on the servers

This measures work #6 against the release before it (`vnxtuymo`). Work #6 brings:
- the compiled `pisa` tool, which builds a snapshot in one process and re-checks a chain of modules in another;
- plain code re-checked by default;
- catalog reads answered by the servers from records stored as columns.

## Setup

**Machine.** Ryzen 9 7940HS, 8 cores / 16 threads, 60 GB of RAM. Lean v4.36.0-rc2.

**Stacks.** Both ran with docker compose:
- *Before:* a `git archive` of `vnxtuymo`, run as compose project `pisabefore`.
- *After:* the tree with this report.

**Physlib.** `scripts/bench_snapshots.py --phases api,verify --auto-implicit`, on `PROJECT=lean-project` with one worker. The library is the import closure of `Physlib.QuantumMechanics`: 102 modules and 25,286 lines, one snapshot per module, each importing the snapshots of the modules it imports.
- **Module system** (`--as-modules`): each file as it is, its imports pointed at snapshots. It is always re-checked.
- **Plain code:** the first 30 modules without the module system, on `import Mathlib`.
  - The 28th, `Physlib.SpaceAndTime.Space.Module`, doesn't elaborate as plain code: a typeclass search runs out of heartbeats.
  - It fails the same way in every run, and the run stops there, so each plain run has the same 27 snapshots.
- The `verify` phase times the re-check of the 102 modules in the worker container, outside pisa:
  - each module alone;
  - then, after, all of them in one `pisa verify`.

**Library.** `scripts/bench_library.py`, on `PROJECT=lean-core` with two workers:
- 1,500 module-system snapshots on Lean's core, created 8 at a time, layer by layer.
- Each imports one to three modules of the layer below, and up to two of the eight layers below that.
- Each declares an exposed definition and a theorem; every 50th theorem is a `sorry` and every 300th uses an axiom.

**Conditions.** Benchmarking found two bugs in the worker image (Findings 5), so the runs differ in how the worker ran:
- *Before, module system:* as it shipped, the worker as PID 1 with per-process cgroups. Sandboxes it killed piled up as zombies, short of the limit in this run.
- *Before, plain code and the library:* compose's `init: true` reaped them.
- *After:* the image runs tini.
- *Cgroups:* until the cgroup fix, which came last, either init left Lean without per-process cgroups. The final after runs, the Physlib module-system creates and the library, have them.
- The numbers didn't move with either change.

The raw results are next to this file:
- `before/` and `after/`: `raw-api-modules.json` and `verify-modules.json`;
- `plain-default/`, `plain-verify-true/` and `plain-verify-false/`;
- the library's in `bench/library/2026-11-11/`.

## Results

### Creating Physlib, module system (102 snapshots, all re-checked)

| | Before | After |
| --- | ---: | ---: |
| Create, median | 11.2 s | **3.8 s** |
| Create, p90 | 34.1 s | **6.4 s** |
| Create, max | 62.8 s | **17.5 s** |
| All 102 creates | 1,756 s | **441 s** |
| Build alone (`check.time`), median | 1.4 s | 1.2 s |
| The library of 25 leaves | 5.1 s | 3.0 s |
| A check right after a create | 17 ms | 14 ms |

All 102 snapshots are kept and verified in both. Hits on the whole library stay at 13 ms, and misses, other headers on the same snapshot, at 1.1 s.

### Plain code (27 snapshots on `import Mathlib`)

| | Create median | p90 | max | All 27 | Verified |
| --- | ---: | ---: | ---: | ---: | ---: |
| Before, default (no re-check) | 5.8 s | 6.8 s | 11.0 s | 166 s | 0 |
| Before, `verify: true` | 12.2 s | 23.9 s | 46.5 s | 409 s | 27 |
| **After, default (re-checked)** | **7.4 s** | **9.1 s** | **13.3 s** | **217 s** | **27** |
| After, `verify: false` | 5.6 s | 6.5 s | 10.9 s | 165 s | 0 |

### The kernel re-check alone (102 Physlib modules)

| | Each alone: median | max | All 102 |
| --- | ---: | ---: | ---: |
| Before: `lean --run PisaVerify.lean`, one process per module | 8.1 s | 53.4 s | 1,315 s |
| After, first version: `pisa verify`, one process per module | 6.5 s | 51.5 s | 1,150 s |
| **After: `pisa verify`, one process per module** | **2.1 s** | **8.3 s** | **226 s** |
| **After: one `pisa verify` for all 102** | | | **97 s** |

Every module passes in every run. "First version" is the tool before it found axioms in one walk (Findings 1).

### A library of 1,500 snapshots (lean-core)

| | Before | After |
| --- | ---: | ---: |
| Creating all 1,500 | 793 s | **322 s** |
| A create, median | 2.5 s | **1.1 s** |
| A create, p95 | 4.6 s | **2.0 s** |
| One create on the top layer (closure of 1,431), warm | 2.0 s | 1.0 s |
| The same, after the workers restart | 4.1 s | 1.5 s |
| Get | 0.01 s | 0.02 s |
| A listing page of 1,000 | 6.3 s | **0.02 s** |
| The second page | 0.72 s | 0.01 s |
| Dependents of the bottom module | 6.0 s | **0.04 s** |
| A label audit of the 1,500 | 6.9 s | **0.03 s** |
| An audit against a spec, with its review list | 12.4 s | **0.08 s** |
| A rebuild plan from the bottom module (1,487 items) | refused: more than 1,000 | **2.6 s** |
| The `modules` table | 463 MB | **4.1 MB** |
| A module row, median / max | 294 KB / 604 KB | **2.2 KB / 2.4 KB** |

## Findings

1. **The re-check was describing declarations, not checking them.**
   - **What took the time:** on the heaviest Physlib module, the kernel replay takes a few seconds, but `pisa verify` took 54 s, single-threaded. The build takes 8 s on 16 s of CPU.
   - **Why:** each declaration's description called Lean's `collectAxioms`. Its table of the imports' axioms is extension state, which a kernel-made environment doesn't have, and which a snapshot's build could forge anyway. So each call walked everything the declaration uses, through Mathlib, from scratch.
   - **The fix:** one walk serves all of a module's declarations. A walk inside a cycle, such as an inductive type's constructors, is kept only once the cycle closes.
   - **The result:**
     - the verdicts on Physlib modules are byte for byte the same;
     - the heaviest module goes from 34 s to 2.8 s;
     - all 102 one at a time, from 1,150 s to 226 s;
     - a real-Lean test holds the axioms to `#print axioms`.
   - **Plain code gains twice:** the build's report uses the same walk.

2. **One process for a chain saves the imports.**
   - Re-checking the 102 modules in one `pisa verify` takes 97 s, against 226 s one by one: each run's import of Mathlib and Physlib is about 1.3 s.
   - Compiling the old interpreted script into the tool gained little on Physlib (1,315 s to 1,150 s): the time was in the axiom walk, which both did.

3. **Re-checking plain code by default costs about 1.8 s per create** on `import Mathlib`: 7.4 s at the median, against 5.6 s with `verify: false`. That is less than half of what `verify: true` cost before (12.2 s, with a p90 of 24 s). The README's "about 2 s" stands.

4. **One process per build doesn't save much by itself.**
   - Plain creates with `verify: false` take what the old default did: 5.6 s against 5.8 s.
   - The old builder's report and export ran in a process that already had the header loaded.
   - The gain of the single process is that the build and the re-check describe declarations alike, from the build's own environment.

5. **Two bugs in the worker image showed up at scale, before and after.** These predate work #6.
   - **Zombies.** pisa-worker ran as PID 1 and never reaped the bubblewrap processes it orphans when it kills a sandbox. Each zombie stays in its slot's cgroup.
     - After a few hundred kills, a slot held 255 of its 256 pids.
     - Every Lean process there then failed with "failed to create thread", or the verifier gave no verdict.
     - The before stack's plain runs hit this after the module-system run.
     - The worker image now runs tini, which reaps them. After 1,500 creates each slot holds 9 pids, with a peak of 10.
   - **Cgroups under an init.** With tini as PID 1, the worker could no longer enable its slots' controllers, because tini stayed in the cgroup root. It now moves every process there into `agent`.

6. **Catalog reads no longer grow with the library.**
   - **Listings and dependents** take 20–40 ms on 1,500 modules, from 6 s: they were worker jobs reading each module's whole record.
   - **The audits** at first got slower on the server, 13 s and 23 s:
     - each module's record still read its closure with every build in it;
     - the audits read them one at a time.
   - **The fix** reads records many at a time, without their closures, plus one query for the imports of everything they build on. The audits now take 30–80 ms.
   - **The rebuild plan** of 1,487 items takes 2.6 s; before, it was refused over 1,000.
   - **Storage** grows linearly: a module row is 2.2 KB, against a median of 294 KB that grew with the closure.
   - **Creates** in a large library take 1.1 s at the median, against 2.5 s before.

7. **The builder's warm-up stays before the answer.**
   - Plan step 8 would move the warm-up of the builder's REPL after the create answers.
   - A module-system create's time, less its build and a re-check run alone, leaves 0.44 s at the median, and that includes the re-check's `lake env` start.
   - So the warm-up is under half a second of a 3.8 s create: too little to change how slots are leased and warmed. Step 8 is skipped.

8. **What is left of a heavy create is the kernel.** The slowest module-system create, 17.5 s for `Hydrogen.LaplaceRungeLenzVector`, is:
   - its build, 8.7 s;
   - its re-check, 8.3 s alone, now mostly kernel replay, on one thread.

   Replaying a module's theorems in parallel, as the build checks them, would be the next step for heavy modules.
