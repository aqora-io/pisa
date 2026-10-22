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
