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
     and their records and the sessions are kept in TiKV, so any worker can
     continue a session.
   - A final check re-runs the whole proof from scratch and confirms it proves
     the original statements, with the original imports.

On top of these, [snapshots](#snapshots) let a client build a library
incrementally: accepted code is kept as a compiled module, and later code
imports it instead of re-checking it.

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

Compose also starts a single-node TiKV (a PD and a TiKV) and Garage. Workers keep
session data in two places, chosen by environment variables that Compose and the
chart set:
- **Metadata**, the sessions, the records that make proof states loadable,
  and the snapshot entries and module records, is in TiKV. `PISA_METADATA` is
  `tikv` or `memory` (the default). `memory` keeps it in the worker process, so
  it isn't shared between workers or kept across restarts. `PISA_TIKV_PD` lists
  the PD endpoints, comma-separated (e.g. `pd:2379`), and `tikv` requires it.
- **Blobs**, the pickled proof states and snapshot modules, are in S3.
  `PISA_STORE` is `s3`, which reads the `AWS_*` variables, or `memory`.

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

**Storage.** Metadata (sessions, proof-state records, snapshot entries and module
records) lives in TiKV, and blobs (pickled proof states, snapshot modules) in an
S3-compatible bucket.
- The chart doesn't deploy TiKV. Set `metadata.tikv.pdEndpoints` to the PD
  endpoints of a cluster you run, for example with the
  [TiDB Operator](https://docs.pingcap.com/tidb-in-kubernetes/stable). It is
  required.
- Give the bucket's credentials as an existing Secret, or leave
  `store.s3.credentialsSecret.name` empty to use the pod's ambient credentials
  (e.g. EKS IRSA).

```sh
helm repo add kedacore https://kedacore.github.io/charts
helm install keda kedacore/keda -n keda --create-namespace --version 2.21.0
kubectl create secret generic pisa-s3 \
  --from-literal=AWS_ACCESS_KEY_ID=... --from-literal=AWS_SECRET_ACCESS_KEY=...
helm install pisa deploy/helm/pisa \
  --set worker.image.repository=<registry>/pisa-worker \
  --set keda.prometheus.serverAddress=http://prometheus-server.monitoring.svc.cluster.local \
  --set metadata.tikv.pdEndpoints={pd.tikv:2379} \
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
kubectl apply -f deploy/minikube/tikv.yaml     # a throwaway single-node TiKV
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
`crates/pisa-protocol/src/api.rs`. Code is a Lean file: plain, or
module-system (a `module` header with `public`, `meta` and `all` imports).

| Endpoint | Request → response |
| --- | --- |
| `POST /check` | `{code, version?, timeout?, detail?}` → check result |
| `POST /check/batch` | `{items: [{id, code}], version?, timeout?, detail?}` → `{results: [...]}`, in order |
| `POST /sessions` | `{code, version?, timeout?}` with `sorry`s → `{session, states: [{state, goal, pos, end_pos}], messages}` |
| `POST /states/{state}/tactic` | `{tactic, timeout?}` → `{state, goals, complete, messages}`; earlier states stay valid |
| `POST /sessions/{session}/check` | `{code, timeout?}` → check result, also requiring the session's original statements and imports |
| `POST /snapshots` | `{code, version?, timeout?, verify?, labels?, key?}` → `{entry?, snapshot?, header?, version, dirty, verified, labels?, result}`; see [Snapshots](#snapshots) |
| `GET /snapshots` | `?label=k:v` (repeated), `limit?`, `after?`, `version?` → `{entries: [{entry, snapshot, imports, header, labels, dirty, verified, created}], next?}`, newest first; see [Labels](#labels) |
| `GET /snapshots/{entry}` | `?version=` → `{entry, snapshot, imports, header, code, labels?, dirty, verified, result, created}` |
| `PATCH /snapshots/{entry}` | `{set?, remove?}`, `?version=` → the entry as in a listing; see [Labels](#labels) |
| `DELETE /snapshots/{entry}` | `?version=` → `{entry, removed: [...]}`, the modules that went with the entry; see [Retention](#retention) |
| `POST /snapshots/{entry}/verify` | `?version=` → `{entry, snapshot, verified, dirty, checked, failure?}`; see [Verifying](#verifying) |
| `GET /health`, `GET /metrics` | Liveness, Prometheus metrics |

A check result is `{ok, time, error?, messages, sorries, reasons, declarations}`:
- `ok` is the verdict.
- `reasons` says why `ok` is false, e.g. `axiom: t uses sorryAx`.
- `declarations` lists each new declaration with the axioms it uses.

With `detail: ["tactics"]`, a check result also has `tactics`, the goals at
every step of the proof. Each entry is one run of a tactic, in the order Lean
ran them, as `{tactic, goals_before, goals_after, pos, end_pos}`:
- `tactic` is its source text, and `pos` and `end_pos` locate it.
- `goals_before` and `goals_after` list the goals as Lean prints them.
- Structure such as `by`, `·`, `<;>` and parentheses has no entry of its own,
  but the tactics inside it do.
- A tactic run on several goals, e.g. under `all_goals` or `<;>`, appears once
  per run, and so does every alternative `first` tried. One that failed has
  the same goals after as before.

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
| `bad_request`, `not_found` | 400, 404 | Malformed request, unknown state, session or snapshot entry |
| `internal` | 500 | A bug |

## Snapshots

A formalizer that builds a library statement by statement would otherwise
resend, and Lean re-check, the whole library on every compile. Instead it
keeps each accepted chunk as a snapshot and builds on it:

```text
POST /snapshots {"code": "import Mathlib\n\ndef double (n : ℕ) := 2 * n", "labels": {"run": "7"}}
  → {"entry": "9c0e…", "snapshot": "PisaSnap.s1f…", "header": "import Mathlib\nimport PisaSnap.s1f…",
     "dirty": false, "verified": false, "labels": {"run": "7"}, "result": {"ok": true, …}}
POST /check {"code": "import Mathlib\nimport PisaSnap.s1f…\n\nexample : double 2 = 4 := rfl"}
```

**Entries and modules.** A create returns two names:
- **The module** (`snapshot`, e.g. `PisaSnap.s1f…`) is what code imports. It
  comes from a hash of the code, so the same code is the same module, and its
  check result, `dirty` and `verified` belong to it.
- **The entry** (`entry`, 32 hex digits) is what this create added: a
  reference to the module with its own `labels` and `created` time. Creating
  the same code again adds another entry on the same module without building
  it again. `GET`, `PATCH`, `DELETE` and `verify` take the entry; a module
  name there is a `bad_request`.

A create that keeps nothing has no `entry`. Neither does `verify: true` on code
that is already kept but fails the re-check: the response then has its
`snapshot` and `header` with the error, as new code that fails the re-check
isn't kept.

<a id="labels"></a>
**Labels.** An entry's labels are string key/value pairs, at most 64 of them. A
key is 1–128 bytes of letters, digits, `.`, `_`, `/` and `-`; a value is up to
512 bytes of UTF-8. Labels that break these rules are a `bad_request`: in
`result.error` on a create, which then keeps nothing, and a 400 on a `PATCH`,
which then changes nothing.
- **Editing.** `PATCH /snapshots/{entry}` with `{"set": {"stage": "done"},
  "remove": ["draft"]}` removes the `remove` keys, then sets the `set` pairs,
  and returns the entry as a listing shows it.
- **Filtering.** `GET /snapshots?label=project:foo&label=kind:lemma` lists the
  entries that have every label given, each an exact match of `key:value`
  split on the first `:`, so a value can contain `:`. Without `label` it lists
  every entry of the version. There is no OR or prefix matching. Two values for
  the same key match nothing, as an entry has one value per key.
- **Order and pages.** Entries come newest first, as summaries: what
  `GET /snapshots/{entry}` returns without `code` and `result`. `limit` is 1–1000
  (default 100). A full page has `next`, an opaque cursor: pass it as `after`
  for the page after it, which may be empty. A cursor is a position, so it
  keeps working when its entry is deleted.
- **Speed.** The first `label` is scanned and the others are looked up for
  each entry it finds, so put the most selective label first. A filter that
  matches few of the first label's entries can take a while to fill a page.

**Retrying a create.** A create can carry an idempotency `key`, 1–256 bytes of
letters, digits, `.`, `_`, `:`, `/` and `-` (anything else is a `bad_request`
in `result.error`), so that a retry is safe:
- While an entry made with the key exists, creating the same code with it
  returns that entry, with its own labels, without building anything. A
  different `labels` on the retry is ignored.
- Different code under a used key is a `bad_request` in `result.error`, and
  nothing is kept.
- A create that keeps nothing, e.g. when Lean reports errors or a `verify: true`
  re-check fails, doesn't use the key up, so a later create can.
- Deleting an entry frees its key.

**How it works.**
- **Building.** A module is its code compiled with `lean -o` to the module
  `PisaSnap.s<id>`, where `id` is a hash of the code.
- **Using.** Later code imports it like any module, in `/check`, `/sessions`
  or another snapshot. A chunk can import several snapshots, which merges
  parallel branches.
- **Hits.** The worker that built a snapshot keeps a Lean process with the
  returned `header` imported, and code that starts with exactly that header
  goes there. It then costs only its own elaboration. Other workers fetch the
  module from the store and import it (seconds at Mathlib scale).

**What carries over.** Snapshots follow Lean's module semantics. Children see
a parent's declarations, instances, `@[simp]` lemmas and notation, but not its
`private` declarations, `open`s, `variable`s, `set_option`s or unclosed
sections. Re-open what a chunk needs.

**Module-system code.** A chunk can be a module-system file (`module`,
`public import`, `@[expose]`):
- **Visibility.** Children then see what the module system exports: `public`
  declarations, with bodies only for `@[expose]` definitions.
- **Imports between kinds.** A `module` file can import only module-system
  snapshots; plain files can import either.
- **The returned `header`.** It keeps `module` and imports the new snapshot
  with the modifiers the chunk used for its snapshot imports. If the chunk
  had none, it uses `public import`, so chains stay transitive.

**Results.**
- Code is kept only if Lean reports no errors and, when it runs, the kernel
  re-check passes (see [Verifying](#verifying)). `result` is a check result, as
  for `/check`, without `sorries`.
- A chunk with `sorry` or another axiom outside the allowed list is kept but
  `dirty`, and checks that use it report the axiom.
- Imports of an unknown or deleted snapshot are `not_found`.

<a id="verifying"></a>
**Verifying.** The kernel re-check imports the snapshot's dependencies again and
re-runs the kernel over its declarations, which adds about 9 s to a small chunk
on `import Mathlib`. So most code skips it while exploring and runs it at the
end. It depends on `verify` in the request:

| `verify` | Kernel re-check at creation |
| --- | --- |
| omitted, no macros | None |
| omitted, the code uses macros or imports a snapshot that does | This chunk; if the kernel rejects it, it isn't kept |
| `true` | This chunk and any unverified imports; the result is `verified`, or has an `error` |
| `false` | None, even with macros |

`verified` means the snapshot and everything it imports have passed the
re-check. So a macro chunk on top of unverified snapshots is re-checked but not
yet `verified`.

`POST /snapshots/{entry}/verify` re-checks the entry's module and everything it
imports, imports first, and records each module that passes. A later run only
covers new chunks, and a retry resumes where the last one stopped. `verified`
and `dirty` belong to modules, so every entry on a module shows the result.
- `checked` lists the modules this run re-checked and passed.
- If the kernel rejects a module or the re-check times out (60 s per module),
  the run stops, still with a 200, and `failure` has the module and the reason.
  Nothing is deleted, because other code may import it: the module stays usable
  and not `verified`. Delete its entries if you don't want it.
- `dirty` and `result` are updated from the re-check, whose axioms don't depend
  on the build being honest.

Check results don't say whether the snapshots they import are verified: look
at the snapshot, or verify it, when that matters.

<a id="retention"></a>
**Retention.** An entry is kept until it is deleted. A module is kept while an
entry points at it or a kept module imports it: deleting an entry removes its
module once nothing else needs it, then each of that module's imports on the
same terms, and `removed` lists them. So deleting the entry of a snapshot that
others build on removes nothing yet, and the code on top keeps working. Each
worker checks a module it has cached against the store at most every
`PISA_SNAPSHOT_CONFIRM_SECS` (60), so a removal takes effect everywhere within
that time.

**Trust.** A snapshot that isn't `verified` has the same guarantee as a
`/check`: the policy scan and an axiom report from the process that imported
it. Macros go beyond that, because what they expand into isn't in the text the
scan sees, so with `verify: false`, `ok` and `dirty` can be wrong until the
snapshot is verified. A `verified` snapshot's own declarations and those of
everything it imports have been re-checked through the kernel, so a forged
proof is caught.

Either way, the `.olean` is imported without re-checking, as with Mathlib's
cache: whoever can write the store can still install an unchecked module, and
the recorded SHA-256 catches only corruption, not a malicious write. The base
Mathlib/Physlib oleans and the kernel are the trusted base.

## Clients

Rust (`pisa-client`):

```rust
let client = pisa_client::Client::new("http://localhost:8000")?.with_timeout(60);
let result = client.check("import Mathlib\ntheorem t : 2 + 2 = 4 := by norm_num").await?;
assert!(result.ok);
let session = client.start_session("theorem t (n : Nat) : n + 0 = n := by sorry").await?;
let step = client.tactic(&session.states[0].state, "simp").await?; // Error::Server { kind: Lean, .. } if it fails
let verdict = client.check_session(&session.session, "theorem t (n : Nat) : n + 0 = n := by simp").await?;
let base = client.create_snapshot("import Mathlib\ndef double (n : ℕ) := 2 * n").await?;
let next = client.check(&format!("{}\nexample : double 2 = 4 := rfl", base.header.unwrap())).await?;
let entry = base.entry.unwrap();
let page = client.list_snapshots(&[("run", "7")], Some(50), None).await?; // newest first
let more = client.list_snapshots(&[("run", "7")], Some(50), page.next.as_deref()).await?;
let done = std::collections::BTreeMap::from([("stage".to_string(), "done".to_string())]);
let summary = client.set_snapshot_labels(&entry, &done, &[]).await?;
let verified = client.verify_snapshot(&entry).await?;
let removed = client.delete_snapshot(&entry).await?; // the modules that went with it
```

Python (`pisa_client`), built from `crates/pisa-python` with `maturin build` or `maturin develop`:

```python
from pisa_client import Client, LeanError

client = Client("http://localhost:8000", timeout=60)
client.check("import Mathlib\ntheorem t : 2 + 2 = 4 := by norm_num")["ok"]
session = client.start_session("theorem t (n : Nat) : n + 0 = n := by sorry")
step = client.tactic(session["states"][0]["state"], "simp")   # raises LeanError if it fails
client.check_session(session["session"], "theorem t (n : Nat) : n + 0 = n := by simp")["ok"]
base = client.create_snapshot("import Mathlib\ndef double (n : ℕ) := 2 * n", labels={"run": "7"})
client.check(base["header"] + "\nexample : double 2 = 4 := rfl")["ok"]
page = client.list_snapshots({"run": "7"}, limit=50)   # newest first; pass page["next"] as after=
client.set_snapshot_labels(base["entry"], set={"stage": "done"}, remove=["run"])
client.verify_snapshot(base["entry"])["verified"]
client.delete_snapshot(base["entry"])   # the modules that went with it
```

Calls are synchronous. They release the GIL, so one `Client` can be shared
across threads. Results are plain dicts; the type stubs describe their shapes.
Exceptions carry `.kind` and `.status`, and their class says what to do:
- `LeanError`: Lean or the policy refused the input.
- `LimitError`: the input hit a timeout, the memory limit, or crashed Lean.
- `CapacityError`: no worker was free; retry later.
- `NotFoundError`: unknown or expired state or session, or unknown snapshot entry.
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
- **dense:** Qwen3-Embedding-8B vectors for every declaration, from Scaleway's
  serverless API, which also embeds each query. The lane's weight scales with
  how many of the lexical lane's top hits have vectors, so a partially embedded
  bundle doesn't crowd out other results. Qwen3-Embedding-0.6B is a free,
  local alternative (TEI on CPU, or llama.cpp on a GPU) that retrieves somewhat
  worse; see `search/bench/2026-10-22/report.md`.

PageRank over the dependency graph is a prior. Declarations that depend on
`sorry` are hidden unless `include_sorried` is set, and so are Lean-generated
helpers (`….mk.injEq`) and metaprogramming internals (`Mathlib.Tactic.…`);
any of them is still returned, flagged, when a query names it exactly.
Deprecated aliases and `to_additive` pairs fold into one hit.

**Descriptions.** Mathlib declarations reuse LeanSearch v2's descriptions
(Apache-2.0, see `search/NOTICE`), matched by name and by the constants in
their type. `pisa-search informalize` writes the rest with Claude Haiku 5.5
(about $38 for the 128k declarations left, in about 3 hours; other Claude and
OpenAI models are a flag away):
- It works in dependency order, so each prompt shows descriptions of what the
  declaration uses.
- It caches by content hash, so after a library update only changed
  declarations cost anything.
- It checks LaTeX against KaTeX and checks that cited Lean names exist.

**Prebuilt indexes.** Each index is a GitHub release tagged `index-<env_id>`,
on the commit that built it. Its assets are archives of the gitignored `search/`
directories, each with `NOTICE`:
- `bundle.tar.zst`: `search/bundle`, which the service serves;
- `store.tar.zst`: `search/store`, the descriptions and vectors. A rebuild that
  starts from it only describes and embeds new or changed declarations;
- `corpus.tar.zst`: `search/corpus`, the exported declarations.

```sh
# Serve a published index (the repository is private, so gh must be logged in).
# Swap bundle for store or corpus to fetch the others.
gh release list -R aqora-io/pisa
gh release download index-v4.35.0-da683984 -R aqora-io/pisa -p bundle.tar.zst -O - \
  | tar -I zstd -x -C search
docker compose --profile search up --build
```

To publish an index after building it (below), from the pushed commit that built
it:

```sh
ENV_ID=$(jq -r .env_id search/bundle/manifest.json)
mkdir -p search/release
for d in bundle store corpus; do
  tar -I 'zstd -T0' -cf search/release/$d.tar.zst -C search $d NOTICE
done
# A new env_id gets a new release.
gh release create index-$ENV_ID -R aqora-io/pisa --target "$(git rev-parse main)" \
  --title "Search index $ENV_ID" --notes "$(jq -c . search/bundle/manifest.json)" \
  search/release/*.tar.zst
# A rebuild for an env_id that already has a release replaces its assets.
gh release upload index-$ENV_ID -R aqora-io/pisa --clobber search/release/*.tar.zst
```

**Building an index:**

```sh
# Export the pinned project's declarations (about 25 minutes).
docker build -f docker/Dockerfile --target corpus --output search/corpus .
# Reuse LeanSearch v2's Mathlib descriptions (downloads 332 MB, verified by sha256).
pisa-search import-lsv2 --corpus search/corpus --file search/lsv2.jsonl --informal search/store
# Describe everything else with Claude (needs ANTHROPIC_API_KEY), and embed every
# declaration as its description lands (needs SCW_SECRET_KEY). Try --sample 500 first.
LIBS=Mathlib,Physlib,Batteries,Init,Std,QuantumInfo
EMBEDDER="openai+https://api.scaleway.ai/v1?model=qwen3-embedding-8b&key_env=SCW_SECRET_KEY"
pisa-search embed --corpus search/corpus --informal search/store --libraries $LIBS \
  --embedder "$EMBEDDER" --label qwen3-embedding-8b@scaleway --batch 32 --concurrency 32 \
  --follow search/work/described &
pisa-search informalize --corpus search/corpus --informal search/store --libraries $LIBS \
  --concurrency 40 && touch search/work/described; wait
# Build the bundle and serve it on :8100.
pisa-search build --corpus search/corpus --informal search/store \
  --embedding-model qwen3-embedding-8b@scaleway --out search/bundle
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

Unknown names and empty results are answers, not errors. A `libraries` or
`kinds` filter matches case-insensitively; an unknown one is a 400 that lists
the known ones. Pinning an `env_id` the server doesn't have is a 404. If the
embedding API fails, the query still returns results from the other lanes,
after at most a few seconds, and says so in the result's `warnings`.

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

**Imports.** Most of the index loads with `import Mathlib` and `import Physlib`.
QuantumInfo (density matrices, entropies, channels) can't be imported
alongside Mathlib, because both declare `Commute.log_left`; pisa reports that
clash as a header error. Its declarations are exported from a second
environment, and each hit carries the header that loads it in
`flags.imports` (`import Physlib; import QuantumInfo`) and in its `text`.

## Verification policy

A check is `ok` only when all of these hold:

- The code elaborated without errors.
- Every new declaration depends only on `propext`, `Classical.choice` and
  `Quot.sound`. This rejects `sorry`, `admit`, `native_decide` and new axioms.
- The code contains no metaprogramming: `#eval`, `run_cmd`, `elab`,
  elaborator attributes, `unsafe`, `implemented_by` and similar.
- Any `set_option` changes only an allowlisted option: resource limits
  (`maxHeartbeats` and similar), `linter.*`, `pp.*`, `trace.*`, and
  `backward.*`, which restores older elaborator behaviour that Mathlib-style
  code often asks for. Options that skip the kernel, such as
  `debug.skipKernelTC`, are rejected.

The axiom report is a `run_cmd` metaprogram. It needs `Lean.Elab.Command`, so
workers add `import Lean.Elab.Command` to every header they load, or
`meta import Lean.Elab.Command` to a module-system header.

This scan is a conservative pre-filter, not the last word: it over-approximates
"the kernel checked every declaration with sound options" by forbidding
anything that could subvert the kernel or the report. A `verified`
[snapshot](#verifying) doesn't rely on it. The re-check runs after the build,
in a separate clean process (`docker/verifier/PisaVerify.lean`, run with
`lean --run`): it re-adds the module's own declarations to an environment of
just its dependencies, with the kernel checking each one, and collects their
axioms by walking the re-checked constants. So a declaration added during the
build with the kernel check skipped is caught even though no forbidden keyword
appears in the source. Because that verdict doesn't depend on the build being
honest, snapshot creation allows the **macro family** (`macro`, `macro_rules`,
`syntax`, `declare_syntax_cat`) — pure syntax transformers whose only risk is
what they expand into, which the re-check covers. It runs by default for code
that uses them, and `verify: false` opts out. `/check` and `/sessions`, which
don't produce a module to re-check, keep the scan as-is.

## Limits

- The REPL never frees environments, so workers replace each process after
  `PISA_MAX_USES` jobs or at a memory high-water mark.
- REPL tactic mode can't write a finished tactic script back into the `sorry`.
  Clients submit the assembled proof to `/sessions/{session}/check`.
- Scoped extensions, such as scoped notation, don't survive pickling.
- Not built yet: auth and per-tenant fairness, `/jobs` async batches, and
  human workspaces (stage 4).
