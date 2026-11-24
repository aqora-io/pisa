# Running with Docker Compose

This page is for running pisa on one machine with Docker Compose, to try it,
develop against it, or serve a small team. It covers what the machine
needs, starting the stack, the variables Compose reads, search, and running
the binaries without Docker. For a cluster, see
[Deploying on Kubernetes](deploy-kubernetes.md).

## Prerequisites

- **Docker** with Compose v2 and BuildKit, able to run privileged
  containers, on a host with cgroup v2 (`docker info` shows
  `Cgroup Version: 2`). The workers run privileged, in a private cgroup
  namespace, so that bubblewrap can sandbox each Lean process and the
  worker can give each one a cgroup with memory and process limits. A
  worker that can't set its cgroups up doesn't start.
- **Network** for the build: it fetches elan, the Lean toolchain, the
  REPL, the project's packages, and Mathlib's prebuilt cache.
- **Disk.** The full worker image is about 20 GB, and Docker holds about as
  much again while it writes it, besides its build cache: leave 50 GB free.
  The `lean-core` image, with Lean alone, is much smaller.
- **Time.** The first full build compiles Physlib and CSLib from source,
  about 35 minutes on a 12-core machine; Mathlib comes prebuilt from its
  cache. Later builds reuse Docker's cache while the project is unchanged.
- **Memory.** Compose's per-process limits add up to 80000 MiB across the
  two workers; see [Memory](#memory) to lower them.

## Starting it

For a quick stack, build the workers on `docker/lean-core`, Lean without
Mathlib:

```sh
PROJECT=lean-core docker compose up --build
```

For the full environment, `docker/lean-project`: Lean v4.36.0 with Mathlib,
Physlib and CSLib, version `v4.36.0-54500a67`:

```sh
docker compose up --build
```

The API is on port 8000 (`PISA_PORT` changes it). The servers answer
`/health` as soon as they are up, before any worker is, so wait for a
worker too:

```sh
until curl -sf localhost:8000/health >/dev/null; do sleep 2; done
curl -s localhost:8000/health | jq '{status, workers, pools: [.pools[]?.version]}'
```

`workers` counts the live workers, and `pools` lists the versions they
serve. Then check some code; this needs no Mathlib, so it runs on either
stack:

```sh
curl -s localhost:8000/check -H 'content-type: application/json' \
  -d '{"code":"theorem t : 2 + 2 = 4 := by decide"}'
```

[Checking](checking.md) describes the answer. `python3 scripts/e2e.py` runs
pisa's end-to-end checks against the stack (`--mathlib` on the full one);
it stops and restarts the workers, so run it against a stack of your own
only.

## What runs

Compose is laid out like a deployment:

| Service | What it is |
| --- | --- |
| `traefik` | The ingress, on port 8000. It finds the servers of this Compose project through Docker, spreads requests over them, and health-checks them on `/health` every 2 s. |
| `server` (×2) | `pisa-server`: the HTTP API. It puts jobs on the queue in PostgreSQL, so any server serves any request. |
| `worker` (×2) | `pisa-worker`: two Lean slots and one Lean Beam slot each, taking jobs from the queue. |
| `postgres` | PostgreSQL 18: the job queue, and the documents, sessions, states, snapshots and module records. |
| `garage` | Garage, an S3 store, bucket `pisa`: the snapshot modules. |
| `qdrant` | Qdrant, for search; only with the `search` or `search-load` [profile](#search). |
| `search-load` | Loads a search bundle into Qdrant, once; only with the `search-load` profile. |

`--scale server=N` or `--scale worker=N` runs more of either. Every worker
names itself after its container and a random suffix.

### Data and restarts

- PostgreSQL and Garage keep their data in the named volumes
  `postgres-data`, `garage-meta` and `garage-data`, and Qdrant in
  `qdrant-data`. `docker compose down -v` deletes them.
- PostgreSQL holds the only copy of the snapshot records, which never
  expire: back it up (see [Backups](operations.md#backups)).
- Traefik, PostgreSQL, Garage, Qdrant, the servers and the workers restart
  with the Docker daemon and whenever they exit, unless stopped, even a
  server or worker that drained cleanly.
- A worker that is stopped drains: it takes no more jobs and finishes those
  it has, for up to 600 s, and Compose gives it 630 s. A server stops within
  Docker's default 10 s; a request it cuts off still runs, as its job is in
  the queue.
- Workers delete documents, sessions and their states after a day
  (`PISA_STATE_TTL_SECS=86400`).

The servers and workers find their data through `PISA_DATABASE_URL` and
`PISA_STORE` with the `AWS_*` variables, which Compose sets. How workers
claim jobs, hold them with a lease, and hand on those of a worker that died
is in [The queue and leases](operations.md#the-queue-and-leases).

### Memory

Compose gives each worker's Lean processes these limits, which it reads
from the shell:

| Variable | Compose's value | What it is |
| --- | --- | --- |
| `PISA_SLOTS` | 2 | Lean REPL processes per worker, for checks, snapshots and acceptances |
| `PISA_MEMORY_MAX_MB` | 16000 | Each one's memory limit, in MiB |
| `PISA_MAX_RSS_MB` | 12000 | The resident memory, in MiB, at which a REPL process is replaced |
| `PISA_BEAM_SLOTS` | 1 | Lean Beam processes per worker, for documents and tactic sessions |
| `PISA_BEAM_MEMORY_MAX_MB` | 8000 | Each Beam process's memory limit, in MiB |

A worker's limits add up to 2 × 16000 + 1 × 8000 = 40000 MiB, and the two
workers' to 80000 MiB. They are limits, not reservations, but a host with
less memory than its workers' limits is `overcommitted` in `/health`'s
`hosts`, the servers log a warning, and a Lean process can be killed for
memory its limit promised. On a smaller machine, lower the limits (the
chart's are 8000 MiB and 6000 MiB), use fewer slots, or run one worker
(`--scale worker=1`). This stack's limits add up to 32000 MiB:

```sh
PISA_SLOTS=1 PISA_MEMORY_MAX_MB=8000 PISA_MAX_RSS_MB=6000 docker compose up --build
```

Lean alone needs far less than Mathlib: the
[minikube values](deploy-kubernetes.md#on-minikube) give each process of
the `lean-core` image 1500 MiB.

## Variables

Compose reads these from the shell, or from a `.env` file beside
`docker-compose.yml`. The first five are Compose's own; the others it
passes to the servers or workers, which
[Configuration](configuration.md) describes.

| Variable | Default | What it does |
| --- | --- | --- |
| `PISA_PORT` | `8000` | The host port Traefik listens on |
| `PROJECT` | `lean-project` | The directory of `docker/` the worker image is built on: `lean-project` or `lean-core`. The image is `pisa-worker:$PROJECT`. |
| `PISA_REVISION` | `unknown` | The commit the images are built from (see [Naming the commit](#naming-the-commit)) |
| `PISA_SEARCH_BUNDLE` | `./search/bundle` | The bundle `search-load` loads into Qdrant |
| `COMPOSE_PROFILES` | none | Compose's own: profiles to enable, e.g. `search` |
| `PISA_SEARCH_QDRANT` | empty | Qdrant's URL, `http://qdrant:6333` with the `search` profile; empty, the servers don't search |
| `PISA_SEARCH_VERSION` | `v4.36.0-54500a67` | The version of the loaded bundle |
| `PISA_SEARCH_EMBEDDER` | `openai+https://api.scaleway.ai/v1?model=qwen3-embedding-8b&key_env=SCW_SECRET_KEY` | The embedding server for queries and snapshot declarations; it must serve the model that embedded the bundle |
| `SCW_SECRET_KEY` | empty | The Scaleway key the default embedder reads |
| `PISA_SEARCH_DESCRIBE_MODEL` | empty | A Claude or OpenAI model that describes snapshot declarations for search, which sends their code to that provider (see [What snapshot search sends to a model](search.md#what-snapshot-search-sends-to-a-model)) |
| `ANTHROPIC_API_KEY`, `OPENAI_API_KEY` | empty | Keys for the describe model |
| `PISA_CORS_ORIGINS` | empty | Origins whose pages may call the API from a browser |
| `PISA_SLOTS`, `PISA_MEMORY_MAX_MB`, `PISA_MAX_RSS_MB`, `PISA_BEAM_SLOTS`, `PISA_BEAM_MEMORY_MAX_MB` | see [Memory](#memory) | The workers' processes and their limits |
| `PISA_BEAM_DOCUMENTS` | `4` | Documents each Beam process keeps open |
| `PISA_PREWARM` | empty | Headers each worker imports as it starts, separated by `;`, e.g. `import Mathlib` |

Compose also sets, without reading the shell, `PISA_MAX_WAIT_SECS=120` on
the servers, and `PISA_SANDBOX=bwrap`, `PISA_CGROUP_ROOT=/sys/fs/cgroup`,
`PISA_STORE=s3`, `PISA_STATE_TTL_SECS=86400` and Garage's `AWS_*` on the
workers.

## Search

The servers search declarations when `PISA_SEARCH_QDRANT` is set, over
Qdrant, which the `search` and `search-load` profiles start. Qdrant keeps
the index in the `qdrant-data` volume and listens on `127.0.0.1:6333`. Load
a bundle into it once, then start the stack with search:

```sh
# The published index of docker/lean-project's version.
curl -fL https://github.com/aqora-io/pisa/releases/download/index-v4.36.0-54500a67/bundle.tar.zst \
  | tar -I zstd -x -C search
docker compose --profile search-load run --rm search-load
PISA_SEARCH_QDRANT=http://qdrant:6333 SCW_SECRET_KEY=... \
  docker compose --profile search up --build
```

To search from a plain `docker compose up`, export
`COMPOSE_PROFILES=search` and `PISA_SEARCH_QDRANT=http://qdrant:6333`, in an
`.envrc` say. Until Qdrant has the bundle, `/search` answers 503 and the
rest of the API works. [Search](search.md) describes the API, and
[Search indexes](search-index.md#serving-an-index) loading, building and
publishing bundles.

## Naming the commit

To name the commit a stack runs, build it with `PISA_REVISION`, which
Compose passes to the images as the `REVISION` build argument. The binaries
report it, in `/health`'s `revision` and `worker_revisions`, and the images
carry it as the label `org.opencontainers.image.revision`:

```sh
# jj: the working copy's commit if it has changes, else its parent
PISA_REVISION=$(jj log -r 'coalesce(@ ~ empty(), @-)' --no-graph -T 'commit_id') docker compose up --build
# git
PISA_REVISION=$(git rev-parse HEAD)$(git diff --quiet || echo -dirty) docker compose up --build
# a plain image build
docker build --build-arg REVISION=<commit> -f docker/Dockerfile --target server .
```

## Other environments

Another toolchain or library runs as another pool of workers beside these,
on the same servers, database and store: see
[Environments](environments.md), and
[Running a pool with Compose](environments.md#with-compose).

## Running without Docker

The binaries also run directly. Build them with Cargo:

```sh
cargo build --release -p pisa-server -p pisa-worker
```

`pisa-server` needs only `PISA_DATABASE_URL`, and listens on `PISA_HTTP`
(default `0.0.0.0:8000`). The database can be a SQLite file, which servers
and workers on one machine share:

```sh
PISA_DATABASE_URL=$PWD/pisa.db target/release/pisa-server
```

A worker needs the same `PISA_DATABASE_URL`, and a Lean installation laid
out as the worker image lays it out (`docker/Dockerfile`, stages `lean` and
`worker`):

| What | Setting | How the image gets it |
| --- | --- | --- |
| elan, with `lean` and `lake` on `PATH` | `PISA_LEAN` (default `lean`), `PISA_LAKE` (default `lake`) | elan's installer. The worker runs `lake env` in the project, and finds the toolchain through elan by the project's `lean-toolchain`. |
| A built Lake project | `PISA_PROJECT` | `docker/lean-project` or `docker/lean-core`, built with `lake build` (after `lake exe cache get` for Mathlib) |
| The REPL, on the project's Lean version | `PISA_REPL` (default `repl`) | [leanprover-community/repl](https://github.com/leanprover-community/repl) at the tag of the project's toolchain, with `docker/repl/infotree-at.patch` applied, built with `lake build` |
| The `pisa` tool and the `Pisa` library, on the project's toolchain | `PISA_TOOL` (default `/opt/pisa/.lake/build/bin/pisa`), `PISA_LEAN_LIB` (default `/opt/pisa/.lake/build/lib/lean`) | `docker/pisa-lean` with the project's `lean-toolchain`, built with `lake build` |
| `lean-beam-mcp` and `leanexport` | `PISA_BEAM_BIN`, `PISA_LEAN_EXPORT` | Lean's toolchain ships both (Lean Beam from v4.36); the worker finds them through `lean --print-prefix` |
| bubblewrap (`bwrap`) on `PATH` | `PISA_SANDBOX=bwrap` | Debian's `bubblewrap` package. Without it, set `PISA_SANDBOX=none` (the default). Acceptance needs it either way, as comparator sandboxes its kernels with it, and needs `/run/user` to exist. |
| A delegated cgroup v2 directory | `PISA_CGROUP_ROOT` | `/sys/fs/cgroup` in a privileged container. Unset, Lean runs without memory or process limits, and the worker logs a warning. |

For example, on the `lean-core` project, with the REPL and
`docker/pisa-lean` built as above:

```sh
cp -r docker/lean-core ~/pisa-project
PISA_DATABASE_URL=$PWD/pisa.db \
PISA_PROJECT=~/pisa-project \
PISA_REPL=<repl checkout>/.lake/build/bin/repl \
PISA_TOOL=$PWD/docker/pisa-lean/.lake/build/bin/pisa \
PISA_LEAN_LIB=$PWD/docker/pisa-lean/.lake/build/lib/lean \
  target/release/pisa-worker
```

`PISA_STORE` defaults to `memory`, which keeps snapshot modules in the
worker's memory: they are lost when it stops, while the database keeps their
records. For snapshots that last, or more than one worker, set
`PISA_STORE=s3` and the `AWS_*` variables of an S3-compatible store. The
image also removes Lake's artifact cache settings from its packages and
replays Mathlib's build, so that Lean Beam can replay imports read-only; the
comments in `docker/Dockerfile`'s `worker` stage say why. Every setting is
in [Configuration](configuration.md).
