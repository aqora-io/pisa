# What the REPL's info trees cost

This is the open question in phase 5 of "Pisa snapshots: closing gaps 1–4" (gap 3). It asks what it would cost to answer goals and expected types at positions from the REPL's `infotree` output, on the bench library of `bench/snapshots/2026-10-22`.

## Setup

**Library and header.** These are the same as October's: the import closure of `Physlib.QuantumMechanics`, one module per chunk, replayed as a chain on `import Mathlib` and Physlib's compiled `Meta` tooling.

**Runs.**
- **Targets:** the six largest chunks that elaborate without errors, the median one, and the one at the first quarter by size.
- **Each target** ran on its own env with no info tree, with `"original"` and with `"full"`, all three twice, the second round in reverse order. Times are the medians of the two.
  - `"original"` gives the tactic nodes that `detail: ["tactics"]` asks for today.
  - `"full"` gives every node, which expected types need.
- **Positions:** 200 random code positions in two chunks were answered from the `"full"` tree. Lines of comments, imports, `open`, `namespace`, `section`, `end`, `variable` and attributes were skipped.

**How it was run.**
- **Scripts:** `scripts/bench_infotree.py` (`raw.json`) and `scripts/bench_positions.py` (`positions.json`). Both reuse `scripts/bench_snapshots.py` and drive the REPL directly in the `pisa-worker:lean-project` image: Lean v4.35.0, REPL v4.35.0.
- **Machine:** a 12-vCPU VM (Intel Haswell) with 45 GB of RAM, not October's machine.
  - The chain took 915 s here against October's 268 s, so times are about 3.4 times October's.
  - Compare ratios within this run, not times across the two.

## Results

| Chunk | Lines | No tree | `"original"` | `"full"` | `"original"` size | `"full"` size |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| QuantumMechanics.Operators.Unbounded | 999 | 71.5 s | 89.9 s (+26%) | 183.5 s (×2.6) | 1.6 MB | 37.2 MB |
| SpaceAndTime.Space.IsDistBounded | 965 | 39.9 s | 71.6 s (+79%) | 247.8 s (×6.2) | 2.3 MB | 34.4 MB |
| Mathematics.Distribution.TemperedDistribution | 668 | 49.0 s | 62.0 s (+27%) | 116.4 s (×2.4) | 0.9 MB | 11.8 MB |
| SpaceAndTime.Space.Derivatives.Basic | 505 | 18.4 s | 22.5 s (+22%) | 38.7 s (×2.1) | 0.3 MB | 5.9 MB |
| Mathematics.SpecialFunctions.PhysHermite | 501 | 12.3 s | 21.5 s (+75%) | 76.1 s (×6.2) | 0.7 MB | 13.4 MB |
| Relativity.Tensors.RealTensor.Vector.Basic | 473 | 13.4 s | 16.4 s (+23%) | 26.4 s (×2.0) | 0.4 MB | 4.2 MB |
| SpaceAndTime.Space.Basic | 287 | 2.3 s | 2.9 s (+28%) | 6.5 s (×2.9) | 0.1 MB | 1.6 MB |
| QuantumMechanics.Operators.Momentum | 168 | 19.1 s | 22.1 s (+16%) | 37.2 s (×1.9) | 0.1 MB | 4.0 MB |

Each module is prefixed `Physlib.`. The two rounds agreed within 4%, or 0.1 s on Space.Basic.

**What the trees hold.** The largest `"full"` tree, Unbounded's, has:
- 22,667 original term nodes, and 13,523 synthetic ones;
- 13,210 completion nodes and 5,686 macro expansions;
- 1,159 original tactic nodes, and 1,903 synthetic ones.

`"original"` keeps only the 1,159 original tactic nodes. Python parses the 37 MB in 0.4 s.

| At 200 random positions | Space.Basic (287 lines) | TemperedDistribution (668 lines) |
| --- | ---: | ---: |
| With goals (an original tactic contains it) | 32 (16%) | 75 (38%) |
| With an expected type (an original term with one contains it) | 46 (23%) | 86 (43%) |
| Kept per position (goals, expected type and term) | 43 B | 490 B |
| `"full"` tree it came from | 1.6 MB | 11.8 MB |

## Findings

1. **Elaboration isn't what expected types cost; printing them is.**
   - **The REPL always builds the trees.** It elaborates every command with info trees on, so "no tree" already includes them.
   - **The extra time is serialisation.** It pretty-prints each kept node's goals, or its type and term.
   - **The size of the gap:** `"original"` prints goals for the original tactics, +16–79%; `"full"` prints every term, ×1.9–6.2.
   - **Our side is cheap.** Parsing the result costs well under a second.

2. **A position needs almost none of the tree.** The answers come to 43–490 B per position, against 1.6–37 MB of tree. Most of the time the REPL spends on `"full"` goes on nodes nobody asked about.

3. **The two tiers cost what their parts cost.**
   - **Goals come from `"original"`.** Today's `detail: ["tactics"]` already pays for it, so asking for goals at positions costs nothing more than that.
   - **Expected types need `"full"`.** For a statement-sized chunk that is seconds: Space.Basic's 287 lines took 6.5 s against 2.3 s. For a 1,000-line chunk it is minutes.

4. **Nothing can be read back later.**
   - **The REPL keeps each command's trees in its memory.** They sit in the env it records, until the process is retired (REPL v4.35.0, `REPL/Main.lean`, `runCommand`).
     - Each run's env stays in the process, so resident memory grew from 8.7 GB to 9.6 GB over the timed runs.
   - **No command prints them again.** The `infotree` option only serialises the command just run, and Lean keeps only a run's own trees in its result (`src/Lean/Elab/Frontend.lean`, `IO.processCommandsIncrementally`).
   - **So a second question elaborates again.** Asking about a position after a check means elaborating the code a second time.

## What this means for phase 5

- **The two tiers:**
  - goals at positions from `"original"`;
  - expected types and terms from `"full"`, only when a check asks for them;
  - either way, the worker filters the tree down to the positions and drops the rest.
- **The docs should say:**
  - expected types are for statement-sized code;
  - every position should be asked in one call, because nothing is cached.
- **Two REPL changes would remove most of the cost.** They would be proposed upstream:
  1. **A position filter on `infotree`.** The REPL would serialise only the nodes that contain the given positions. Printing is what costs (finding 1), so `"full"` at a few positions would cost about what a plain check does.
  2. **A command that prints the trees an existing env kept, at given positions.** The trees are already in memory (finding 4), so a later question would need no new elaboration. This is how Lean's language server answers hovers.
