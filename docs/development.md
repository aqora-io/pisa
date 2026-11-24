# Development

This page is for people who change pisa: how the repository is laid out, how
to build it and run every test CI runs, the fakes that stand in for Lean,
how to regenerate the files made from the code, the playground, and the
scripts. [CONTRIBUTING.md](../CONTRIBUTING.md) says how to send a change.

## Layout

The Rust workspace is in `crates/`:

| Crate | Role |
| --- | --- |
| `pisa-protocol` | The HTTP API types, REPL protocol types, header splitting, the jobs and results that go through the queue |
| `pisa-repl` | Async driver for one REPL process: blank-line framing, timeouts, crash reporting |
| `pisa-beam` | Client for Lean Beam's MCP server (`lean-beam-mcp`, shipped in Lean toolchains from v4.36): tactic probes at positions, speculative states, goals, and the problems left in a file; and `fake-beam-mcp` |
| `pisa-worker` | Slot pool cached by import header, process recycling, sandbox, cgroups, verification, documents and tactic sessions on Lean Beam, claiming jobs |
| `pisa-db` | The database (PostgreSQL, or SQLite for tests and single-process runs), its migration, the job queue and the workers' registry, and the snapshot catalog, shared by the servers and the workers |
| `pisa-server` | The server: `pisa-router`'s API and `pisa-search`'s in one process, with a combined `/health` and graceful shutdown |
| `pisa-router` | The check, session and snapshot API: queueing jobs and waiting for their results, and answering catalog reads |
| `pisa-snapshots` | What reads the snapshot catalog, for the servers and the workers: snapshots, the comparison of a replacement with what it replaces, audits and their review lists, rebuild plans |
| `pisa-client` | Async Rust client for the HTTP API |
| `pisa-python` | Python bindings for `pisa-client` (PyO3 + maturin), imported as `pisa_client` |
| `pisa-tests` | The fakes of the REPL, `pisa`, `lean`, `leanexport` and `lake` (below), and in-process integration tests: a router and workers on fake-repl |
| `pisa-search` | Declaration search over Mathlib, Physlib and CSLib: corpus, descriptions, index, HTTP API, and the CLI that builds and loads bundles (see [Search](search.md)) |

`pisa-protocol` and `pisa-client` are published to crates.io, and
`pisa-python` to PyPI as `pisa-client`; the other crates are not published.

The rest of the repository:

| Path | What it holds |
| --- | --- |
| `docker/Dockerfile` | The images: targets `server`, `worker`, `corpus` (the search corpus of the pinned project) and `manifest` (a project's resolved `lake-manifest.json`, for `scripts/env.py`) |
| `docker/lean-project`, `docker/lean-core` | The Lake projects the workers import from: Mathlib, Physlib and CSLib, or Lean alone |
| `docker/pisa-lean` | The `Pisa` library the axiom report runs from, and the `pisa` tool that builds snapshot modules and re-verifies them through the kernel |
| `docker/repl` | The patch to the Lean REPL that adds `infotreeAt` |
| `docker-compose.yml` | The local stack (see [Running with Docker Compose](deploy-compose.md)) |
| `deploy/helm/pisa`, `deploy/minikube` | The Helm chart, and values and manifests for trying it on minikube (see [Kubernetes](deploy-kubernetes.md)) |
| `web/` | A pnpm workspace: the TypeScript client (`packages/client`), the React hooks (`packages/react`) and the playground (`apps/playground`); see [Clients](clients.md) |
| `search/` | Search's inputs and reports: the Lean exporter of the corpus (`exporter/`), the symbol table and glossaries, evaluation sets and reports (`eval/`); and the generated corpus, store and bundle, which are not committed (see [Search indexes](search-index.md)) |
| `scripts/` | The end-to-end script, the environment builder and the autoscaling check (see [Scripts](#scripts)) |
| `openapi.json` | The server's OpenAPI document, generated from the code |

### The fakes

The fakes let the tests run pisa's servers and workers without Lean. Each is
scripted by markers in the code it is given; its source's header lists them.

| Binary | Crate | Stands in for |
| --- | --- | --- |
| `fake-repl` | `pisa-tests` | The Lean REPL, speaking its protocol: `#check Nat` gives an information message, `error_here` an error, `sorry` a warning and a proof state, `SLEEP <ms>` a delay, `CRASH` an exit, and `needs NAME` an error unless `NAME` is declared |
| `fake-pisa` | `pisa-tests` | The `pisa` tool: `pisa build` compiles a module into a copy of its code and reports it as fake-repl does, and `pisa verify` re-verifies modules, with markers for a forged declaration or a failing kernel |
| `fake-lean` | `pisa-tests` | `lean`, which a worker runs to explain an import the REPL fails to load: each `import Clash` line fails |
| `fake-leanexport` | `pisa-tests` | Lean's bundled `leanexport`: it prints a module's code as its export |
| `fake-lake` | `pisa-tests` | `lake comparator`, judging the exports fake-leanexport writes: `sorry` is refused for its axiom, and markers make it sleep, abort, or have one of the kernels decline, reject or fail |
| `fake-beam-mcp` | `pisa-beam` | `lean-beam-mcp`: the real tool list, with argument checks as strict as Beam's and scripted answers |

## Building

```sh
cargo build                                   # pisa-server, pisa-worker, pisa-search and the fakes
docker compose up --build                     # the stack: Lean v4.36.0 + Mathlib + Physlib + CSLib
PROJECT=lean-core docker compose up --build   # the stack without Mathlib, for quick checks
```

The first full image build compiles Physlib and CSLib, which takes a long
time; `PROJECT=lean-core` builds Lean alone, much sooner. [Running with Docker
Compose](deploy-compose.md) describes the stack, and
[Running without Docker](deploy-compose.md#running-without-docker) what the
binaries need.

`target/` grows large. CI builds without debug information or incremental
builds, which keeps it much smaller; do the same locally:

```sh
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0
```

## Tests

```sh
cargo test --workspace                 # fast tests with the fakes, no Lean needed
(cd docker/pisa-lean && lake build) && cargo test -p pisa-worker --test real_lean -- --ignored   # the report and the verifier on real Lean
cargo test -p pisa-worker --test real_comparator -- --ignored   # acceptance on real Lean, which needs bwrap
cargo test -p pisa-beam --test real_beam -- --ignored           # the Lean Beam client on real Beam
python3 scripts/e2e.py [--mathlib] [--search] [--beam]          # end to end, against a running stack
```

- `cargo test --workspace` runs every crate's tests, the in-process
  integration tests of `pisa-tests` among them, on SQLite. It also checks
  that every package names the workspace's version
  (`cargo test -p pisa-tests --test release`).
- The real-Lean tests need elan on `PATH`, and run on `docker/pisa-lean`'s
  toolchain; build that library first. The comparator tests also need a
  `bwrap` that works: on Ubuntu, AppArmor keeps it from making namespaces
  until `sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0`. The
  Beam tests run `lean-beam-mcp` from the same toolchain.
- The clients' tests and the end-to-end script are below.

### PostgreSQL and Qdrant

Without more, the catalog and queue tests run on SQLite only, and the tests
that search through Qdrant are skipped. Two variables add the rest:

- `PISA_TEST_DATABASE_URL`: a PostgreSQL that every catalog and queue test
  runs on too, besides SQLite.
- `PISA_TEST_QDRANT_URL`: a Qdrant for the search tests, the clients'
  included.

The tests write into both, so give them throwaway ones, on ports a running
stack doesn't use (Compose's search profile puts its Qdrant, which holds the
index, on 6333):

```sh
docker run -d --rm --name pisa-test-postgres -e POSTGRES_PASSWORD=pisa -p 25432:5432 postgres:18
docker run -d --rm --name pisa-test-qdrant -p 26333:6333 qdrant/qdrant:v1.20.0
export PISA_TEST_DATABASE_URL=postgres://postgres:pisa@localhost:25432/postgres
export PISA_TEST_QDRANT_URL=http://localhost:26333
cargo test --workspace
docker stop pisa-test-postgres pisa-test-qdrant   # --rm removes them
```

### The clients

The clients' tests start `pisa-server`, `pisa-worker` and `pisa-search` from
`target/debug` (`PISA_BIN_DIR` names another directory), with the shell's
environment, from which they read their `PISA_*` settings. Unset any that
the shell sets for a stack, e.g. `env -u PISA_SEARCH_QDRANT pnpm
test:integration`.

The web tests (`test:integration` starts a server and a fake worker, and
searches too when `PISA_TEST_QDRANT_URL` is set):

```sh
cd web && pnpm install
pnpm typecheck && pnpm test
cargo build -p pisa-server -p pisa-worker -p pisa-tests -p pisa-search && pnpm test:integration
```

The Python tests, against a server and a fake worker too:

```sh
cargo build -p pisa-server -p pisa-worker -p pisa-tests -p pisa-search
cd crates/pisa-python
python -m venv .venv && . .venv/bin/activate && pip install maturin pytest
maturin develop && pytest tests
```

### End to end

`scripts/e2e.py` checks a running Compose stack with real Lean: checks, the
policy, sessions, snapshots, specs and runs, audits, acceptance with
comparator, labels and scopes. It talks to `PISA_URL`, by default
`http://localhost:8000`.

| Flag | Adds |
| --- | --- |
| `--mathlib` | Checks that import Mathlib, Physlib and CSLib: needs the full image, not `lean-core` |
| `--search` | Snapshot search: needs a stack that searches (`PISA_SEARCH_QDRANT` set, and Compose's `search` profile) |
| `--beam` | Open documents: needs a Beam slot on the workers (`PISA_BEAM_SLOTS`, 1 in Compose) |
| `--no-failover` | Skips stopping and restarting the stack's two workers, which checks that sessions resume on the other; for a stack that isn't Compose's, such as Kubernetes through a port-forward |

Without `--no-failover`, it waits for two workers and stops each in turn, so
run it against a stack whose data doesn't matter.

```sh
PROJECT=lean-core docker compose up -d --build
python3 scripts/e2e.py --beam
```

### What CI runs

`.github/workflows/ci.yml` runs on every pull request and push to `main`:

| Job | Steps |
| --- | --- |
| Rust, PostgreSQL and real Lean | `cargo fmt --all --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace` with PostgreSQL 18 and Qdrant 1.20 as services (`PISA_TEST_DATABASE_URL`, `PISA_TEST_QDRANT_URL`); `lake build` in `docker/pisa-lean`; the real-Lean tests; bubblewrap, then the real-comparator tests; the real-Beam tests |
| Web and Python clients | Builds the server, the worker, the fakes and `pisa-search`; `pnpm typecheck`, `pnpm test` and `pnpm test:integration` in `web/`; `maturin develop` and `pytest tests` on Python 3.12. This job has no Qdrant, so the clients' search tests are skipped |
| End to end on lean-core | `docker compose config -q`; `PROJECT=lean-core docker compose up -d --build`; waits for `/health`; `scripts/e2e.py --beam`, with `PISA_BEAM_SLOTS=1` |

CI builds with `CARGO_INCREMENTAL=0` and `CARGO_PROFILE_DEV_DEBUG=0`.

## Regenerating openapi.json, openapi.ts and the Python stubs

The server describes its API and search as OpenAPI 3.1 at `GET
/openapi.json`, also committed as `openapi.json`. Its schemas are derived
from the Rust types, and the TypeScript types in
`web/packages/client/src/openapi.ts` are generated from it. After changing
the API, regenerate both; a test fails until you do:

```sh
PISA_UPDATE_OPENAPI=1 cargo test -p pisa-server --test openapi
pnpm -C web generate
```

`web/packages/client/src/types.ts` names the generated types the client
uses.

The Python client's type stubs, `crates/pisa-python/python/pisa_client/_native.pyi`,
describe `Client` by hand, and a test checks that they name every call and
argument the native module has. `AsyncClient`'s section is made from
`Client`'s. After changing `Client`'s calls, edit its stubs, then write
`AsyncClient`'s again, with the module built (`maturin develop`):

```sh
cd crates/pisa-python && PISA_UPDATE_STUBS=1 pytest tests/test_stubs.py
```

## The playground

The playground (`web/apps/playground`) has a tab for each part of the API:

- **Check**: code checked as you type, or with Ctrl/⌘-Enter, and the goals
  at the cursor. Declarations in full, a timeout, positions pinned for their
  goals and terms, a check run as a job, and a batch of code split at
  `-- ---` lines.
- **Prove**: a session's tree of tactics from each `sorry`, candidates tried
  at a `sorry`, and the finished proof checked against the session.
- **Documents**: code kept open, saved a revision at a time, with its
  sorries, the goals at the cursor or at lines, and candidates tried at a
  `sorry` or at the cursor.
- **Snapshots**: creates that may replace a snapshot, fork a run or leave
  holes; the list, or its heads; and a snapshot with its link, holes, stored
  check and dependents, to verify, replace or delete.
- **Runs**: audits of a project by its labels or of a run against its spec,
  the export and acceptance, and rebuilds planned and run.
- **Search**: several fields at once, a declaration's details, and names
  resolved, when the server searches.

Prove and Documents need a worker with a Lean Beam slot (`PISA_BEAM_SLOTS`).
Links carry code and ids between tabs: a check's code to keep or prove, a
snapshot's header to build on, a hit's imports to try, and any snapshot id
to open. The version box applies to every request once it settles, except
those about a snapshot, document or session, whose id carries its version.

`vite dev` proxies `/api` to `PISA_URL`, by default the Compose stack's
`http://localhost:8000`:

```sh
cd web && pnpm install
pnpm dev                                  # http://localhost:5173
PISA_URL=http://localhost:8001 pnpm dev   # another server
```

To serve the built playground from another origin, build it with
`VITE_PISA_URL` set to the server, and allow its origin with
`PISA_CORS_ORIGINS` (see [Configuration](configuration.md)).

### Its end-to-end walk

A Playwright walk through every tab (`web/apps/playground/e2e`) drives the
dev server, which it starts, against a stack whose data doesn't matter, and
deletes the snapshots it made. Building a second Compose project retags the
`pisa-server` and `pisa-worker:$PROJECT` images, so give it other `image:`
names in an override file when another stack runs from them:

```sh
export COMPOSE_PROJECT_NAME=pisa-e2e PROJECT=lean-core PISA_PORT=9100
docker compose up -d --build
PISA_URL=http://localhost:9100 pnpm -C web --filter playground e2e
docker compose down -v
```

With `COMPOSE_PROJECT_NAME` set, it stops the stack's workers for a moment to
withdraw a job no worker has taken; without it, it skips that step.
`PLAYWRIGHT_CHROMIUM` names a Chromium to run instead of Playwright's own.
Screenshots land in `e2e/out`, which git ignores.

## Scripts

### scripts/e2e.py

The end-to-end checks; see [End to end](#end-to-end).

### scripts/env.py

Builds the worker image of an environment, a Lean toolchain and the
libraries its workers build, from a TOML file; see
[scripts/env.py](environments.md#scriptsenvpy).

```sh
scripts/env.py build <environment>.toml [--out DIR] [--update] [--project-only] [--repository R] [--revision COMMIT] [--push]
scripts/env.py label <project>
```

| Flag | Effect |
| --- | --- |
| `--out DIR` | Where the environment's Lake project goes; by default beside the file, named as it is |
| `--update` | Resolves its dependencies again; otherwise a manifest is kept while the file says what it said |
| `--project-only` | Writes the project and prints its version, without building an image |
| `--repository R` | The image's name, without its tag; `pisa-worker` by default. The tag is `<release>-<version>`, the release from `Cargo.toml` |
| `--revision COMMIT` | The pisa commit the image reports, in `/health` and its label; by default `HEAD` |
| `--push` | Pushes the image |

`build` prints `version=<version>` and `image=<image>`. `label` prints the
version of a Lake project, as its workers compute it, e.g.
`scripts/env.py label docker/lean-project`.

### scripts/k8s_scale.py

Checks KEDA autoscaling and graceful drain on a Helm release, through
`PISA_URL` (by default `http://localhost:8000`, e.g. a port-forward):

1. sends a burst of slow checks and waits for the worker Deployment to scale
   above its minimum, then for every check to pass;
2. waits for the Deployment to scale back down to its minimum;
3. deletes the worker pod while it runs a check: the pod drains, so the
   check still passes, without waiting for its claim to lapse.

| Flag | Default | Effect |
| --- | --- | --- |
| `--kubectl` | `minikube -p pisa kubectl -- --context pisa` | The kubectl command, e.g. `kubectl` for the current context |
| `--deployment` | `pisa-worker` | The worker Deployment |
| `--min` | 1 | The ScaledObject's `minReplicaCount` |
| `--jobs` | 24 | Checks in the burst |
| `--seconds` | 20 | How long each check runs |
| `--scale-down-timeout` | 300 | Seconds to wait for the minimum |

[Kubernetes](deploy-kubernetes.md#on-minikube) shows it run on minikube.

## Releasing

Releases are tags; [CONTRIBUTING.md](../CONTRIBUTING.md#releasing) has the
steps.
