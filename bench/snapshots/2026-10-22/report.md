# Environment snapshots at Mathlib scale

Step 1 of gap 1 in "Pisa as a MerLean backend: what's missing". It measures chunk replay and compiled modules against what pisa does today, before snapshots are built.

## Setup

**Library.** It stands in for one a formalizer would build: the import closure of `Physlib.QuantumMechanics`.
- 86 modules and 21,022 lines, comparable to MerLean's 8k–19k-line libraries.
- One module per chunk (about 240 lines, against MerLean's roughly 360 per statement).

**Header.** `import Mathlib`, plus Physlib's `Meta` tooling, imported compiled (see Findings 5).

**How it was run.**
- `scripts/bench_snapshots.py` drives the REPL directly in the `pisa-worker:lean-project` image: Lean v4.35.0, REPL v4.35.0.
- Machine: Ryzen 9 7940HS, 8 cores / 16 threads, 60 GB of RAM.
- Header imports are timed with a warm page cache. The first, cold import of Mathlib took 21 s.
- The raw numbers are in `raw.json`; `tables.md` has the generated tables, including per-chunk figures.

## Results

| | Measurement | Time | Memory |
| --- | --- | ---: | ---: |
| M0 | Import the header | 3.4 s | 6.6 GB |
| M1 | **Today:** resend the library as one command | 63 s at 25%, 126 s at 50%, 173 s at 75%, **258 s** at 100% (about 12 s per 1k lines) | |
| M2 | **Snapshot miss:** replay all 86 chunks as a chain | **268 s** | 7.9 GB (+1.3 GB, 64 MB per 1k lines) |
| M2 | **Snapshot hit:** one chunk on its base | **median 1.0 s**, p90 8.6 s, max 33.5 s | |
| M3 | A trivial lemma on the final env, or on a mid-chain env | 17 ms, all OK | no growth |
| M4 | pisa's axiom report on the final env | whole: 6.2 s, 2,280 declarations | |
| M4 | The same report, scoped to the last chunk | 0.1 s, 31 declarations | |
| M5 | Build the 86 chunks as modules, one by one | 257 s (160 s without Lake's 1.1 s per call) | |
| M5 | **Snapshot miss via modules:** import the header and the 25 leaf modules, then a trivial lemma | **3.3 s**, then 5 ms | 6.6 GB (+46 MB) |

## Findings

1. **Hits are the win.** A statement-sized chunk on a held snapshot takes a median of 1 s; resending the 21k-line prefix takes 258 s. The hit costs what the new statement costs, and nothing for the library under it.

2. **A miss by replay costs a full resend.** Replaying the chain (268 s) is as slow as today's resend (258 s). That is acceptable only if misses are rare, and today they would not be:
   - a slot is replaced every 200 jobs (`PISA_MAX_USES`);
   - at MerLean's 13 compiles per statement, that is roughly every 15 statements;
   - replay also holds about 64 MB per 1k lines in the process.

3. **Compiled modules make misses cheap.**
   - **Import:** the whole library imports in 3.3 s with almost no extra memory, because modules are memory-mapped; a replay takes 268 s.
   - **Build cost:** building a module costs no more than replaying the chunk (160 s against 268 s in total).
   - **Takeaway:** compiling each accepted chunk once turns every later miss into a few seconds.

4. **The axiom report must be scoped to the new code.**
   - Unscoped, it re-reports the whole library on every check: 2,280 declarations, 6.2 s, growing with the library.
   - Scoped by an exclusion list passed as one string literal, it reports the 31 new declarations in 0.1 s.

5. **Replay is not the same as modules for every library.**
   - **Tooling can't be replayed.** Physlib's `Meta` tooling registers environment extensions with `initialize`. Lean cannot evaluate those in the module that defines them, and the REPL aborted on the next chunk that used one. Tooling has to be imported compiled, as here, and pisa's policy rejects most such code in requests anyway.
   - **Module-system code changes meaning.** Physlib is written for Lean's module system: other modules see a definition's body only if it is exposed, and a module sees only what it imports. A replay chain shares one environment, so every body is visible and every earlier chunk's instances are in scope. 7 of the 86 chunks failed under replay (unsolved `simp` goals, an instance search out of heartbeats), but all built cleanly as modules.
   - **MerLean is unaffected.** Its output is ordinary Lean in one dependency order, so it would not see this. Chunks with errors were kept in M1 and M2, so both measured the same text.

6. **Older envs stay usable.** A lemma on a mid-chain env worked in the same process as one on the final env. Sibling branches can share a process.

## What this means for the build round

The plan settled on chunk replay only, with modules "later, if the numbers call for them". The numbers call for them:
- **Hits:** serve them from a slot that holds the snapshot's env, as planned. This is where nearly all the saving is.
- **Misses:**
  - Compile each accepted chunk as a module once, store its `.olean` files with the snapshot, and on a miss import them (seconds) instead of replaying (minutes).
  - The imported snapshot is a new header key, so it gets its own warm process, which pisa already manages.
  - Replay can stay as the fallback when a module build fails.
- **Recycling:** keep slots holding snapshots alive longer than `PISA_MAX_USES` allows, or recycle on memory instead. Even with modules, a miss costs an import.
- **Axiom report:** scope it to the chunk from the start.

## Snapshots as built (measured through the API)

Snapshots were then built as compiled modules that later code imports (see
the README). The `api` phase of `scripts/bench_snapshots.py` drove them
through `POST /snapshots` and `/check`, against `docker compose` with two
workers and Garage.

**How it was run.**
- Each module became one snapshot, importing the snapshots of the modules it
  imports. That preserves Physlib's import graph, so many snapshots merge
  several others.
- Twelve checks followed each create, as a formalizer's next compiles would
  (MerLean averages 13 compiles per statement).
- `raw-api.json` has the per-module numbers, `cold.json` the restart and
  memory figures, and `determinism.json` the rebuild comparison.

**Which modules.** 47 of the 86 modules, 9,530 lines.
- **Plain Lean.** Snapshots take plain Lean, so the chunks drop Physlib's
  module-system markers, as in M2. The 7 modules that failed in M2 fail here
  too, with the same errors.
- **Not from leaked state.** Each snapshot imports only its real
  dependencies, so these failures don't come from other chunks' declarations
  leaking in. They come from dropping the module system, under which only
  exposed definitions unfold.
- **The rest.** 32 more modules depend on those 7, which leaves 47
  (`--skip-replay-failures`).
- **Policy.** Physlib's own code needs a `macro` and `backward.*` options, so
  the bench workers allowed both. MerLean's output needs neither.

| | Result |
| --- | --- |
| Create a snapshot (47) | median 7.2 s, p90 9.3 s, max 17.8 s; 369 s in total |
| of which `lean -o`, including its 3.4 s Mathlib import | median 3.7 s, max 14.3 s |
| of which the builder importing the new snapshot, the axiom report and the upload | 3.5 s, nearly constant |
| Checks right after each create (12 × 47) | all warm hits: median 40 ms, max 46 ms |
| One snapshot importing all 21 leaves (the whole library) | created in 7.0 s |
| Checks on it, warm | 41 ms |
| Checks on it with another header (a fresh import) | 3.4–3.5 s |
| Checks on it after restarting both workers (empty caches, modules downloaded from Garage) | 3.6 s |
| Memory of the warm whole-library process | RSS 6.6 GB, of which 465 MB private dirty; 5.5 GB is clean, file-backed `.olean` pages that other processes on the host share |

**Findings.**
1. **A statement costs seconds, not minutes.**
   - In MerLean's pattern of 12 checks and then one create, a statement on
     this 9.5k-line library costs about 7.7 s.
   - Resending the prefix (M1) costs about 12 s per 1k lines, so about 115 s
     per compile and about 1,500 s per statement.
2. **Checks follow the snapshot to its builder.** The builder is warm on the
   returned header, and the router sends code with that header there: all 564
   checks were hits.
3. **The library size barely matters.**
   - Importing the whole library costs what importing Mathlib alone costs
     (3.4 s), because modules are memory-mapped.
   - A worker with empty caches downloads and imports it in 3.6 s.
   - So recycling processes (`PISA_MAX_USES`) and restarts stay cheap.
4. **The fixed cost of a snapshot is two Mathlib imports, about 7 s:** the
   build's, and the builder's REPL. If that matters, running the axiom report
   inside the build and warming the builder in the background would halve it.
5. **Builds are deterministic at scale.** The three largest chunks (670, 504
   and 475 lines), rebuilt in each worker container, gave `.olean` files
   identical to each other and to the one the store served. So two workers
   building the same snapshot store the same bytes.
6. **A warm snapshot costs about 0.5 GB of anonymous memory.** The rest is
   page cache shared across processes, so slots can be sized by private
   memory rather than RSS.
7. **Module-system files were a follow-up**, now built; see the next section.

## Module-system snapshots on the whole closure

Snapshots now take module-system files and keep the five files Lean writes
for each. `--as-modules` passes every Physlib file through unchanged, except
that imports of other closure modules point at their snapshots.

| | Result |
| --- | --- |
| Modules kept | **86 of 86** (21,046 lines), including the 7 that fail as plain Lean |
| Dirty | 1, correctly: Physlib's `HarmonicOscillator.Eigenstates` has a `sorry` |
| Create a snapshot | median 2.9 s, p90 5.5 s, max 10.1 s; 302 s in total |
| of which `lean -o` | median 1.6 s |
| of which the builder importing it, the report and the upload | 1.4 s |
| Checks right after each create (12 × 86) | all warm: median 58 ms, max 82 ms |
| One snapshot importing all 25 leaves (the whole library) | created in 2.8 s |
| Checks on it, warm | 58 ms |
| Checks on it with another header (a fresh import) | 1.4–1.6 s |
| Checks on it after restarting both workers | 1.8 s |
| Memory of the warm whole-library process | RSS 1.8 GB, of which 273 MB private dirty |

**Findings.**
1. **The whole library builds.** Kept as module-system files, all 86 modules
   build as snapshots, including the 7 that elaborate differently as plain
   Lean (M2, and the plain run above).
2. **A statement costs about 3.6 s** in MerLean's pattern (twelve checks,
   then a snapshot), against about 3,350 s for 13 resends of this 21k-line
   library (M1).
3. **Module-system snapshots are about twice as fast as plain ones.** Each
   module imports only the Mathlib modules it needs, not all of Mathlib, so
   a build takes 1.6 s and a fresh import 1.4 s, against 3.7 s and 3.4 s.
4. **Builds are deterministic.** All five files match when rebuilt on each
   worker.
5. **Policy.** `backward.*` options are now allowed by default. The bench
   still allowed metaprogramming for the one Physlib module that defines a
   `macro`.
