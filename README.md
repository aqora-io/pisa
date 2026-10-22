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
| `pisa-search` | Declaration search over Mathlib and Physlib: corpus, descriptions, index, HTTP API (see [Search](#search)) |

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

## Kubernetes

`deploy/helm/pisa` is a Helm chart containing:
- the router;
- the worker Deployment;
- a KEDA `ScaledObject`;
- an optional `ServiceMonitor`.

**Cluster prerequisites.** Like most application charts, it uses the cluster's
own KEDA and Prometheus:
- `keda.prometheus.serverAddress` must point at that Prometheus.
- Prometheus must scrape the router. The router Service carries
  `prometheus.io/scrape` annotations, which `prometheus-community/prometheus`
  picks up. With the Prometheus Operator, set
  `metrics.serviceMonitor.enabled=true`, plus any labels its selector needs.
- Keep the scrape interval short, e.g. 5–15s: it bounds how fast workers react
  to load.

**Storage.** Proof states go to an S3-compatible bucket. Give its credentials as an existing Secret, or leave
`store.s3.credentialsSecret.name` empty to use the pod's ambient credentials
(e.g. EKS IRSA):

```sh
helm repo add kedacore https://kedacore.github.io/charts
helm install keda kedacore/keda -n keda --create-namespace --version 2.21.0
kubectl create secret generic pisa-s3 \
  --from-literal=AWS_ACCESS_KEY_ID=... --from-literal=AWS_SECRET_ACCESS_KEY=...
helm install pisa deploy/helm/pisa \
  --set worker.image.repository=<registry>/pisa-worker \
  --set keda.prometheus.serverAddress=http://prometheus-server.monitoring.svc.cluster.local \
  --set store.s3.bucket=<bucket> --set store.s3.region=<region> \
  --set store.s3.credentialsSecret.name=pisa-s3
```

`store.s3.endpoint` and `store.s3.allowHttp` point the workers at a non-AWS
store such as MinIO or Garage. `credentialsSecret.accessKeyIdKey` and
`credentialsSecret.secretAccessKeyKey` name the keys inside the Secret.

**Scaling.** Workers scale on the release's outstanding jobs, queued plus
running, read from the router's `pisa_queue_depth` and `pisa_slots{state="busy"}`
metrics. The query counts each job once even if two scrape jobs collect the
router; `keda.prometheus.query` overrides it.
- The target is `keda.jobsPerWorker` jobs per pod, which defaults to `worker.slots`.
- `keda.scaleDownStabilizationSecs` sets how long demand must stay low before pods are removed.

**Draining.** A worker that receives SIGTERM tells the router it is draining, gets no new jobs, finishes the ones it has, and exits. Two settings bound this:
- `worker.drainTimeoutSecs` is the longest it waits for running jobs.
- The pod's grace period is that plus 30s.

Scale-downs and rollouts therefore don't cut jobs off.

**On minikube**, with the images built by `PROJECT=lean-core docker compose build`:

```sh
minikube start -p pisa --driver=docker --cpus=8 --memory=20g
helm repo add kedacore https://kedacore.github.io/charts
helm repo add prometheus-community https://prometheus-community.github.io/helm-charts
helm install keda kedacore/keda -n keda --create-namespace --version 2.21.0
helm install prometheus prometheus-community/prometheus --version 29.37.0 \
  -n monitoring --create-namespace -f deploy/minikube/prometheus-values.yaml
minikube -p pisa image load pisa-router:latest pisa-worker:lean-core
kubectl apply -f deploy/minikube/garage.yaml   # a throwaway S3 store and its Secret
helm install pisa deploy/helm/pisa -f deploy/minikube/values.yaml
kubectl port-forward svc/pisa-router 8000:8000 &
python3 scripts/e2e.py --no-failover   # checks, policy, sessions against real Lean
python3 scripts/k8s_scale.py           # scales up under load, back down, drains on delete
```

**Not covered by the chart:**
- Separate pools per project version. The scaling query counts every job, so one release should serve one version.
- Auth on the API.
- A tighter security context: workers run privileged so bubblewrap and cgroup limits work.

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
cargo build -p pisa-router -p pisa-worker -p pisa-tests -p pisa-search
cd crates/pisa-python && maturin develop && pytest tests
```

## Search

`pisa-search` finds Mathlib and Physlib declarations from a description,
LaTeX, a Lean pattern or goal, or a partial or misspelled name. It is meant for
agents that know what they need but not what it is called. It runs as its own
service. Its index is built from the same pinned project as the workers, and
every response carries that project's version label as `env_id`, so the
declarations it returns exist in the matching worker pool.

**How it ranks.** Four lanes each return candidates, and reciprocal rank fusion
(k = 60) merges them:
- **lexical:** BM25 over names, descriptions, signatures and docstrings;
- **name:** exact, case-insensitive, last-component and fuzzy name matches;
- **structural:** constants the query names or implies, matched against each
  type's constants. LaTeX symbols map to constants through
  `search/symbols.toml`;
- **dense:** Qwen3-Embedding-0.6B vectors served by TEI. So far these cover
  Physlib and the declarations it uses directly, about 21,000 in all;
  embedding all of Mathlib needs a GPU. The lane's weight scales with how many
  of the lexical lane's top hits have vectors, so partial coverage doesn't
  crowd out other results.

PageRank over the dependency graph is a prior. Declarations that depend on
`sorry` are hidden unless `include_sorried` is set. Deprecated aliases and
`to_additive` pairs fold into one hit.

**Descriptions.** Mathlib declarations reuse LeanSearch v2's descriptions
(Apache-2.0, see `search/NOTICE`), matched by name and by the constants in
their type. `pisa-search informalize` writes the rest with Claude:
- It works in dependency order, so each prompt shows descriptions of what the
  declaration uses.
- It caches by content hash, so after a library update only changed
  declarations cost anything.
- It checks LaTeX against KaTeX and checks that cited Lean names exist.

**Building an index:**

```sh
# Export the pinned project's declarations (about 25 minutes).
docker build -f docker/Dockerfile --target corpus --output search/corpus .
# Reuse LeanSearch v2's Mathlib descriptions (downloads 332 MB, verified by sha256).
pisa-search import-lsv2 --corpus search/corpus --file search/lsv2.jsonl --informal search/store
# Optional: describe Physlib with Claude (needs ANTHROPIC_API_KEY). Try a sample first.
pisa-search informalize --corpus search/corpus --informal search/store \
  --sample 500 --report search/pilot/$(date +%F)/report.md
# Optional: embed Physlib and what it uses for the dense lane (about 3.5 hours on 8 CPU cores).
docker compose --profile search up -d tei
pisa-search embed --corpus search/corpus --informal search/store --libraries Physlib --with-deps
# Build the bundle and serve it on :8100.
pisa-search build --corpus search/corpus --informal search/store \
  --embedding-model Qwen/Qwen3-Embedding-0.6B --out search/bundle
docker compose --profile search up --build
```

`--informal` takes a directory or an `s3://bucket/prefix` URL (credentials come
from `AWS_*`). It holds descriptions and vectors, keyed by content hash.

**API** (types in `crates/pisa-protocol/src/search.rs`):

| Endpoint | Request → response |
| --- | --- |
| `POST /v1/search` | `{queries: [{q?, nl?, latex?, pattern?, goal?, name?, libraries?, kinds?, k?, include_sorried?}], env_id?}` → `{env_id, results: [{hits}]}`, at most 64 queries |
| `POST /v1/resolve` | `{names, env_id?}` → `{env_id, results: [{name, exists, deprecated_to?, suggestions}]}` |
| `GET /v1/decl/{name}` | → `{env_id, decl}`: signature, description, flags, dependencies and users |

Each hit carries:
- name, kind, library, module and signature;
- a description, when one exists;
- flags: `uses_sorry`, `informal`, `deprecated_to`, `additive_sibling`;
- the lanes that found it, and a source link;
- `text`, about 150 tokens ready for a model's context.

Unknown names and empty results are answers, not errors. Pinning an `env_id`
the server doesn't have is a 404.

```python
from pisa_client import SearchClient

search = SearchClient("http://localhost:8100")
for hit in search.search(nl="trace of a density matrix is one", libraries=["Physlib"])["hits"]:
    print(hit["text"])
search.resolve(["Real.sqrt_mul_sefl"])[0]["suggestions"][0]["name"]  # "Real.sqrt_mul_self"
```

The Rust `pisa_client::SearchClient` has the same methods. To run the Python
search tests, first run `cargo build -p pisa-search`.

**Quality.** Two reports track it:
- `search/eval/<env_id>.md`: recall, MRR and nDCG on LeanSearch v2's MathlibQR
  benchmark, per lane and per query style;
- `search/pilot/2026-10-22/report.md`: the informalization pilot, with cost,
  a full-run estimate, check pass rates and samples to review.

**Imports.** The index covers `import Mathlib` together with `import Physlib`.
Standalone QuantumInfo can't be imported alongside Mathlib, because both
declare `Commute.log_left`. pisa reports that clash as a header error.

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
- Not built yet: auth and per-tenant fairness, `/jobs` async batches, and
  human workspaces (stage 4).
