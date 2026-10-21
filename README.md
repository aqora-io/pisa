# pisa

A Rust control plane for serving Lean 4 to machines. It runs unmodified
upstream [Lean REPL](https://github.com/leanprover-community/repl) processes. A
router queues requests globally and routes each one to a worker that has the
right project version and, ideally, the right imports already loaded.

This repository implements stages 1–3 of the design doc *Building a Scalable
Lean 4 Server in Rust*:

1. **Router and workers.** The router keeps a global queue with two priority classes, routes by
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
| `pisa-protocol` | The HTTP API types, REPL protocol types, header splitting, router↔worker frames |
| `pisa-repl` | Async driver for one REPL process: blank-line framing, timeouts, crash reporting |
| `pisa-worker` | Slot pool cached by import header, process recycling, sandbox, cgroups, verification, sessions, state store |
| `pisa-router` | HTTP API, worker registry, global queue, routing, metrics |
| `pisa-client` | Async Rust client for the HTTP API |
| `pisa-python` | Python bindings for `pisa-client` (PyO3 + maturin), imported as `pisa_client` |
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

All fields are snake_case. The request and response types are in
`crates/pisa-protocol/src/api.rs`.

| Endpoint | Request → response |
| --- | --- |
| `POST /check` | `{code, version?, timeout?}` → check result |
| `POST /check/batch` | `{items: [{id, code}], version?, timeout?}` → `{results: [...]}`, in order |
| `POST /sessions` | `{code, version?, timeout?}` with `sorry`s → `{session, states: [{state, goal, pos, end_pos}], messages}` |
| `POST /states/{state}/tactic` | `{tactic, timeout?}` → `{state, goals, complete, messages}`; earlier states stay valid |
| `POST /sessions/{session}/check` | `{code, timeout?}` → check result, also requiring the session's original statements |
| `GET /health`, `GET /metrics` | Liveness, Prometheus metrics |

A check result is `{ok, time, error?, messages, sorries, reasons, declarations}`:
- `ok` is the verdict.
- `reasons` says why `ok` is false, e.g. `axiom: t uses sorryAx`.
- `declarations` lists each new declaration with the axioms it uses.

`version` selects a worker pool, by label (such as `v4.35.0-1a2b3c4d`) or by
an alias set with `PISA_ALIASES`. `timeout` is Lean's limit in seconds
(default 60).

**Errors** always look like `{"error": {"kind", "message"}}`.
- **Checks report failures in-band.** A check that times out, crashes or is
  rejected by the policy still returns 200, with `ok: false` and the `error`.
  Batch items work the same way, so one failed item never fails a batch.
- **Other requests use HTTP statuses:**

| `kind` | Status | Meaning |
| --- | --- | --- |
| `lean`, `policy` | 422 | Lean or the policy refused the input, e.g. an unknown tactic |
| `timeout`, `out_of_memory`, `crash` | 422 | The input hit a resource limit |
| `capacity`, `unavailable` | 429, 503 | No worker could take the request; retry later |
| `bad_request`, `not_found` | 400, 404 | Malformed request, unknown state or session |
| `internal` | 500 | A bug |

## Clients

Rust (`pisa-client`):

```rust
let client = pisa_client::Client::new("http://localhost:8000")?.with_timeout(60);
let result = client.check("import Mathlib\ntheorem t : 2 + 2 = 4 := by norm_num").await?;
assert!(result.ok);
let session = client.start_session("theorem t (n : Nat) : n + 0 = n := by sorry").await?;
let step = client.tactic(&session.states[0].state, "simp").await?; // Error::Server { kind: Lean, .. } if it fails
let verdict = client.check_session(&session.session, "theorem t (n : Nat) : n + 0 = n := by simp").await?;
```

Python (`pisa_client`), built from `crates/pisa-python` with `maturin build` or `maturin develop`:

```python
from pisa_client import Client, LeanError

client = Client("http://localhost:8000", timeout=60)
client.check("import Mathlib\ntheorem t : 2 + 2 = 4 := by norm_num")["ok"]
session = client.start_session("theorem t (n : Nat) : n + 0 = n := by sorry")
step = client.tactic(session["states"][0]["state"], "simp")   # raises LeanError if it fails
client.check_session(session["session"], "theorem t (n : Nat) : n + 0 = n := by simp")["ok"]
```

Calls are synchronous. They release the GIL, so one `Client` can be shared
across threads. Results are plain dicts; the type stubs describe their shapes.
Exceptions carry `.kind` and `.status`, and their class says what to do:
- `LeanError`: Lean or the policy refused the input.
- `LimitError`: the input hit a timeout, the memory limit, or crashed Lean.
- `CapacityError`: no worker was free; retry later.
- `NotFoundError`: unknown or expired state or session.
- `PisaError`: the base class; also raised for transport failures, with no
  `kind` or `status`.

To run the Python tests:

```sh
cargo build -p pisa-router -p pisa-worker -p pisa-tests
cd crates/pisa-python && maturin develop && pytest tests
```

## Verification policy

A check is `ok` only when all of these hold:

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
