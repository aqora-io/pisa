# pisa

A Rust control plane for serving Lean 4 to machines. It runs unmodified
upstream [Lean REPL](https://github.com/leanprover-community/repl) processes. A
router queues requests globally and routes each one to a worker that has the
right project version and, ideally, the right imports already loaded.

This repository implements stages 1–3 of the design doc *Building a Scalable
Lean 4 Server in Rust*:

1. **Router and workers.** The HTTP API is compatible with Kimina Lean Server.
   The router keeps a global queue with two priority classes, routes by
   warm header with rendezvous hashing under a load bound, retries jobs when a
   worker disconnects, and exports Prometheus metrics.
2. **Hardened workers.**
   - Each REPL runs in a bubblewrap sandbox and its own cgroup v2 limits.
   - Proofs are judged by the axioms they depend on, not by string matching.
   - Metaprogramming and unsafe options are rejected before the code runs.
   - Worker pools are labelled by version.
3. **Tactic sessions.**
   - Proof states are pickled into object storage (S3, or Garage in Compose),
     so any worker can continue a session.
   - A final check re-runs the whole proof from scratch and confirms it proves
     the original statements.

## Layout

| Crate | Role |
| --- | --- |
| `pisa-protocol` | REPL protocol types, header splitting, the Kimina-compatible API, router↔worker frames, session types |
| `pisa-repl` | Async driver for one REPL process: blank-line framing, timeouts, crash reporting |
| `pisa-worker` | Slot pool cached by import header, process recycling, sandbox, cgroups, verification, sessions, state store |
| `pisa-router` | HTTP API, worker registry, global queue, routing, metrics |
| `pisa-tests` | `fake-repl` (speaks the REPL protocol without Lean) and in-process integration tests |

## Running

```sh
docker compose up --build              # Lean v4.35.0 + Mathlib + Physlib (first build compiles Physlib)
PROJECT=lean-core docker compose up --build   # no Mathlib, for quick checks
python3 scripts/e2e.py [--mathlib]     # end-to-end checks against the running stack
cargo test --workspace                 # fast tests with fake-repl, no Lean needed
```

The pinned versions live in `docker/lean-project`: `lean-toolchain`,
`lakefile.toml` and `lake-manifest.json`. Keep `REPL_REV` in `docker/Dockerfile`
on the same Lean version.

## API

| Endpoint | Purpose |
| --- | --- |
| `POST /api/check` | Kimina's current endpoint: `{snippets: [{id, code}], timeout, reuse, infotree}` |
| `POST /verify` | Kimina's legacy endpoint: `{codes: [{custom_id, proof}], timeout, infotree_type, disable_cache}` |
| `POST /sessions` | `{code}` with `sorry`s → `{session, states: [{state, goal, pos}]}` |
| `POST /sessions/{state}/tactic` | `{tactic}` → `{state, goals, proofStatus}`; old states stay valid |
| `POST /sessions/{session}/check` | `{code}` → `{ok, reasons, result}`: full re-check plus statement check |
| `GET /metrics`, `GET /health` | Prometheus metrics, liveness |

`/api/check` and `/verify` accept an optional `version`. It can be a worker
pool label such as `v4.35.0-1a2b3c4d`, or an alias set with `PISA_ALIASES`.
Each result also carries an optional `verification` field:
`{ok, reasons, declarations}`.

## Verification policy

A result is `ok` only when all of these hold:

- The code elaborated without errors.
- Every new declaration depends only on `propext`, `Classical.choice` and
  `Quot.sound`. This rejects `sorry`, `admit`, `native_decide` and new axioms.
- The code contains no metaprogramming: `#eval`, `run_cmd`, `macro`, `elab`,
  `syntax`, elaborator attributes, `unsafe`, `implemented_by` and similar.
- Any `set_option` changes only an allowlisted option.

The axiom report is a `run_cmd` metaprogram. It needs `Lean.Elab.Command`, so
workers add `import Lean.Elab.Command` to every header they load.

## Limits

- The REPL never frees environments, so workers replace each process after
  `PISA_MAX_USES` jobs or at a memory high-water mark.
- REPL tactic mode can't write a finished tactic script back into the `sorry`.
  Clients submit the assembled proof to `/sessions/{session}/check`.
- Scoped extensions, such as scoped notation, don't survive pickling.
- Not built yet: Kubernetes manifests, autoscaling, auth and per-tenant
  fairness, `/jobs` async batches, and human workspaces (stage 4).
