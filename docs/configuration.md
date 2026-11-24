# Configuration

Every setting pisa's binaries read: the worker (`pisa-worker`), the server
(`pisa-server`, which serves the API and search), and the `pisa-search`
tool, then the store, model-provider and other variables the code reads.
It is for whoever deploys pisa.

Most settings are both an environment variable and a flag; the flag wins.
Lists are separated by `,`, except `PISA_PREWARM`'s, by `;`. The last two
columns say where `docker-compose.yml` and the Helm chart
(`deploy/helm/pisa`) set each variable. "(shell)" marks a value Compose
takes from your shell or a `.env` file, with that default; the chart's
column names the values key and its default; – means it is left at the
binary's default. How to run pisa with these is in
[Operations](operations.md).

## Worker

| Variable | Flag | Default | What it does | Compose | Chart |
| --- | --- | --- | --- | --- | --- |
| `PISA_DATABASE_URL` | `--database-url` | required | The database: a PostgreSQL URL (`postgres://…`), or the path of a SQLite file, which only one host can use. It holds the job queue and the records of documents, sessions, states and snapshots. | `postgres://pisa:pisa@postgres:5432/pisa` | `metadata.database.urlSecret` |
| `PISA_QUEUE` | `--queue` | `pisa` | The job queue to take jobs from. The servers must name the same one; deployments that share a database need queues of their own. | – | `queue` (the release's full name) |
| `PISA_STORE` | `--store` | `memory` | Where snapshot modules live: `s3`, which reads the [`AWS_*` variables](#store), or `memory`, in this process only, so no other worker can import its snapshots and they are gone when it stops. All workers of a version must share one store. | `s3` | fixed `s3` |
| `PISA_WORKER_ID` | `--worker-id` | `$HOSTNAME`, a dash and 8 random hex digits | The worker's name in the queue, `/health` and logs. Without `HOSTNAME` it takes `/etc/hostname`, else `worker`. | – | the pod's name |
| `PISA_VERSION` | `--version` | from `PISA_PROJECT`, e.g. `v4.36.0-54500a67`; `dev` without one | The version the worker serves, which requests name to reach it: the project's toolchain tag and 8 hex digits of a hash of its `lake-manifest.json`, lakefile and top-level `.lean` files. | – | – |
| `PISA_EXPECT_VERSION` | `--expect-version` | none | The version the worker must turn out to serve; with another, it doesn't start. | – | `worker.version` (`v4.36.0-54500a67`), or a pool's `version` |
| `PISA_SLOTS` | `--slots` | the CPU count minus one, at least 1 | Lean REPL processes, for checks, snapshots and acceptances. | `2` (shell) | `worker.slots` (2) |
| `PISA_BEAM_SLOTS` | `--beam-slots` | `1` | Lean Beam processes for documents and tactic sessions, besides the REPL slots. 0 serves neither. | `1` (shell) | `worker.beamSlots` (1) |
| `PISA_BEAM_DOCUMENTS` | `--beam-documents` | `4` | Documents each Beam process keeps open. To open another, it closes the least recently used one that no job is using. | `4` (shell) | `worker.beamDocuments` (4) |
| `PISA_PREWARM` | `--prewarm` | none | Headers to import into idle slots at startup, e.g. `import Mathlib`. | empty (shell) | `worker.prewarm` (empty) |
| `PISA_MAX_USES` | `--max-uses` | `200` | Jobs after which a REPL process is replaced, as the REPL never frees environments. | – | – |
| `PISA_IMPORT_TIMEOUT_SECS` | `--import-timeout-secs` | `600` | How long importing a header into a fresh process may take. | – | – |
| `PISA_SANDBOX` | `--sandbox` | `none` | How each Lean process is isolated: `none`, or `bwrap`, bubblewrap with no network, a private `/proc` and `/tmp`, and a read-only filesystem. Only Compose and the chart turn it on. | `bwrap` | `worker.sandbox` (`bwrap`) |
| `PISA_CGROUP_ROOT` | `--cgroup-root` | none | A delegated cgroup v2 directory. Each Lean process gets a child cgroup with the three limits below. Set, the worker doesn't start if it can't set them up; unset, Lean runs with no memory or process limit. | `/sys/fs/cgroup` | `/sys/fs/cgroup` while `worker.cgroups` (true) |
| `PISA_MEMORY_MAX_MB` | `--memory-max-mb` | `0`, no limit | `memory.max` of each REPL process, in MiB. A job that runs out is an `out_of_memory` error that names it. | `16000` (shell) | `worker.memoryMaxMb` (8000) |
| `PISA_BEAM_MEMORY_MAX_MB` | `--beam-memory-max-mb` | `0`, no limit | `memory.max` of each Beam process, in MiB. | `8000` (shell) | `worker.beamMemoryMaxMb` (8000) |
| `PISA_PIDS_MAX` | `--pids-max` | `256` | `pids.max` of each Lean process; 0 for no limit. | – | – |
| `PISA_MAX_RSS_MB` | `--max-rss-mb` | `0`, off | Replace a REPL process once its resident memory exceeds this, in MiB. It works without cgroups too. | `12000` (shell) | `worker.maxRssMb` (6000) |
| `PISA_WORK_DIR` | `--work-dir` | `$TMPDIR/pisa-<worker id>` | Scratch space: per-slot files, the snapshot modules the worker fetched, and acceptances' exports. | – | – |
| `PISA_ALLOWED_AXIOMS` | `--allowed-axioms` | `propext,Classical.choice,Quot.sound` | The axioms a checked declaration may depend on. Give the servers the same list. | – | `allowedAxioms` (empty: the default) |
| `PISA_ALLOWED_OPTIONS` | `--allowed-options` | [below](#the-default-allowed-options) | The options `set_option` may change; a trailing `*` matches a prefix. | – | – |
| `PISA_ALLOW_METAPROGRAMMING` | `--allow-metaprogramming` | off | Lets through what the [policy](checking.md#the-verification-policy) refuses as metaprogramming: `#eval`, `run_cmd`, `elab`, `unsafe`, macros, and attributes that run code. Options and axioms are still checked. `true` or `1` turns it on. | – | – |
| `PISA_LEASE_SECS` | `--lease-secs` | `30` | How long a claimed job stays the worker's without a heartbeat, which renews it every third of that. A job whose worker died runs elsewhere once it lapses; see [The queue and leases](operations.md#the-queue-and-leases). | – | `worker.leaseSecs` (30) |
| `PISA_DRAIN_TIMEOUT_SECS` | `--drain-timeout-secs` | `600` | On SIGTERM, how long to wait for running jobs before exiting anyway. | – (its `stop_grace_period` is 630 s) | `worker.drainTimeoutSecs` (600); the pod's grace period is 30 s more |
| `PISA_STATE_TTL_SECS` | `--state-ttl-secs` | `0`, keep them | Delete documents, sessions and their states older than this. | `86400` | `store.stateTtlSecs` (86400) |
| `PISA_SNAPSHOT_CONFIRM_SECS` | `--snapshot-confirm-secs` | `60` | How long a lookup in the store vouches for a snapshot the worker has cached: a deleted snapshot keeps working here for at most this long. | – | – |
| `PISA_CLAIM_STORE` | `--claim-store` | off | Make the store's marker name this database's catalog as the worker starts, so that its workers sweep the store: for [a database reset on a kept store](snapshots.md#a-database-reset-on-a-kept-store). Unset it once a worker has started with it. | – | – |
| `PISA_PROJECT` | `--project` | none | The Lake project whose dependencies code imports, through `lake env`. It gives the default version, and the toolchain, packages and files `/environments` lists. | – (the image's `/opt/project`) | – (the image's) |
| `PISA_REPL` | `--repl` | `repl` | The REPL executable. | – (the image's `/usr/local/bin/repl`) | – (the image's) |
| `PISA_LEAN` | `--lean` | `lean` | `lean`, which explains imports the REPL fails to load, and finds the project's toolchain with `lean --print-prefix`. | – | – |
| `PISA_LAKE` | `--lake` | `lake` | `lake`, which runs `lake comparator` on the exports of a run's modules. | – | – |
| `PISA_LEAN_EXPORT` | `--lean-export` | the toolchain's `leanexport` | The exporter of the snapshot modules comparator judges. | – | – |
| `PISA_TOOL` | `--tool` | `/opt/pisa/.lake/build/bin/pisa` | The `pisa` tool (`docker/pisa-lean`), which builds snapshot modules and re-checks them through the kernel. | – | – |
| `PISA_LEAN_LIB` | `--lean-lib` | `/opt/pisa/.lake/build/lib/lean` | The `.olean` root of the built `Pisa` library, whose report every header imports. | – | – |
| `PISA_BEAM_BIN` | `--beam-bin` | the toolchain's `lean-beam-mcp` | Lean Beam. | – | – |
| `PISA_PROJECT_GIT`, `PISA_PROJECT_REV` | `--project-git`, `--project-rev` | none | For a project that is itself a library code imports, not a wrapper that requires one: the git repository and revision of its code. An export of a run whose modules import it requires the project from there, and is refused without them. Set both or neither. | – | – (`worker.extraEnv`) |

The chart sets these per pool: a pool's keys override `worker`'s (see
[Chart pools](environments.md#chart-pools)).

### The default allowed options

`PISA_ALLOWED_OPTIONS` defaults to `maxHeartbeats`, `maxRecDepth`,
`synthInstance.maxHeartbeats`, `synthInstance.maxSize`,
`maxSynthPendingDepth`, `autoImplicit`, `relaxedAutoImplicit`, `linter.*`,
`pp.*`, `trace.*`, `exponentiation.threshold`, `backward.*`, `diagnostics`
and `quotPrecheck*`.

### Acceptance and bubblewrap

Acceptance runs `lake comparator`, whose kernels run in a bubblewrap sandbox
of their own whatever `PISA_SANDBOX` says: it needs `bwrap` on the `PATH`,
unprivileged user namespaces and a `/proc` of its own. A worker that can't
make that sandbox logs a warning as it starts, and acceptances fail on it.

## Server

The server's own settings, and its router's.

| Variable | Flag | Default | What it does | Compose | Chart |
| --- | --- | --- | --- | --- | --- |
| `PISA_DATABASE_URL` | `--database-url` | required | The workers' database: the job queue, and the records the server reads and search indexes. | `postgres://pisa:pisa@postgres:5432/pisa` | `metadata.database.urlSecret` |
| `PISA_HTTP` | `--http` | `0.0.0.0:8000` | The address the API listens on. | – | fixed `0.0.0.0:8000`; the Service's port is `server.service.port` (8000) |
| `PISA_QUEUE` | `--queue` | `pisa` | The job queue the server puts jobs on. | – | `queue` (the release's full name) |
| `PISA_MAX_WAIT_SECS` | `--max-wait-secs` | `60` | How long a job may wait for a worker before its request gets a 429 `capacity`, and how long a job whose worker was lost waits for another before a 503 `worker_lost`. Async jobs wait longer; see [The queue and leases](operations.md#the-queue-and-leases). | `120` | `router.maxWaitSecs` (300) |
| `PISA_MAX_RETRIES` | `--max-retries` | `2` | How many more workers may run a job after its worker is lost. | – | `router.maxRetries` (2) |
| `PISA_DEFAULT_VERSION` | `--default-version` | none | The version a request that names none goes to. Without one, such a request runs on any worker, and a snapshot read takes the one version the live workers serve. | – | `router.defaultVersion`, else `worker.version` |
| `PISA_ALIASES` | `--alias` | none | Other names for versions, `NAME=VERSION`, e.g. `mathlib=v4.36.0-54500a67`. | – | `router.aliases` |
| `PISA_ALLOWED_AXIOMS` | `--allowed-axioms` | `propext,Classical.choice,Quot.sound` | The axioms the workers allow, which an audit leaves out of what it lists of a declaration that isn't proved. Keep it the workers' list. | – | `allowedAxioms` (empty: the default) |
| `PISA_CORS_ORIGINS` | `--cors-origin` | none | Origins whose pages may call the API from a browser, e.g. `https://app.example.com`, or `*` for any. | empty (shell) | `server.corsOrigins` |
| `PISA_DRAIN_TIMEOUT_SECS` | `--drain-timeout-secs` | `360` | On SIGTERM, how long to wait for the requests in flight, after taking requests for 5 s more. | – | `server.drainTimeoutSecs` (360); the pod's grace period is 30 s more |

## Search

The server searches when `PISA_SEARCH_QDRANT` is set. An empty value counts
as unset, as Compose passes unset variables as empty ones. The chart sets
these only with `search.enabled`.

| Variable | Flag | Default | What it does | Compose | Chart |
| --- | --- | --- | --- | --- | --- |
| `PISA_SEARCH_QDRANT` | `--qdrant` | none | Qdrant's REST URL. Without it the server doesn't search, and `/search`, `/resolve` and `/declarations` answer 404 `not_found`. | empty (shell) | `search.qdrant.url`, or the Qdrant subchart's Service (`qdrant.enabled`) |
| `PISA_SEARCH_VERSION` | `--version` | none; required with `--qdrant` | The version whose loaded bundle to serve, the bundle's `env_id`. | `v4.36.0-54500a67` (shell) | `search.version`, else `worker.version` |
| `PISA_SEARCH_QDRANT_KEY_ENV` | `--qdrant-key-env` | none | The name of the variable that holds Qdrant's API key, if it needs one. | – | `QDRANT_API_KEY`, set from `search.qdrant.apiKeySecret` |
| `PISA_SEARCH_EMBEDDER` | `--embedder` | none | The embedding server: a TEI URL, or `openai+<base url>?model=<model>[&key_env=<VAR>]` for an OpenAI-compatible endpoint whose key is in `VAR`. It embeds queries, for the dense lane, and snapshot declarations, so it must serve the model that embedded the bundle. Without it, search has no dense lane. | `openai+https://api.scaleway.ai/v1?model=qwen3-embedding-8b&key_env=SCW_SECRET_KEY` (shell) | `search.embedder` (the same) |
| `PISA_SEARCH_SNAPSHOTS` | `--snapshots` | `true` | Whether to search the workers' snapshots, from the database's catalog: `true` or `false`. | – | `false` when `search.snapshots` is false |
| `PISA_SEARCH_SNAPSHOT_VERSION` | `--snapshot-version` | `PISA_SEARCH_VERSION`'s | The worker version whose snapshots to search. | – | – |
| `PISA_SEARCH_DESCRIBE_MODEL` | `--describe-model` | none | A Claude or OpenAI model that describes every snapshot declaration, so snapshot code goes to its provider (see [What snapshot search sends to a model](search.md#what-snapshot-search-sends-to-a-model)). Without one, snapshot declarations are searched by name, signature and docstring. | empty (shell) | `search.describeModel` (empty), while `search.snapshots` |
| `PISA_SEARCH_DESCRIBE_CONCURRENCY` | `--describe-concurrency` | `8` | Model and embedding requests in flight for snapshots. | – | `search.describeConcurrency` (8), while `search.snapshots` |
| `PISA_SEARCH_SYMBOLS` | `--symbols` | the built-in `search/symbols.toml` | The symbol dictionary. | – | – |

## pisa-search

`pisa-search` builds, loads and evaluates indexes (see
[Search indexes](search-index.md)); the server image has it. Most of its
settings are flags of a subcommand, which `pisa-search <command> --help`
lists. These also read a variable:

| Variable | Flag | Subcommands | Default | What it does | Compose | Chart |
| --- | --- | --- | --- | --- | --- | --- |
| `PISA_SEARCH_QDRANT` | `--qdrant` | `load`, `eval`, `query` | required | Qdrant's REST URL. | `search-load` passes `--qdrant http://qdrant:6333` | – |
| `PISA_SEARCH_QDRANT_KEY_ENV` | `--qdrant-key-env` | `load`, `eval`, `query` | none | The name of the variable that holds Qdrant's API key. | – | – |
| `PISA_SEARCH_EMBEDDER` | `--embedder` | `embed`; `eval`, `query` | `http://localhost:8090` for `embed`; none for the others, which then have no dense lane | The embedding server, as for the server. | – | – |
| `PISA_DATABASE_URL` | `--database-url` | `validate-descriptions` | none | The database that keeps readers' description flags, for `--flagged`. | – | – |
| `PISA_SEARCH_DESCRIBE_MODEL` | `--model` | `validate-descriptions` | `claude-haiku-5-5` | The model that judges and describes. | – | – |

`informalize` and `validate-descriptions` need a
[model provider's key](#model-providers). A description store given as
`s3://…` (`--informal`) reads the [`AWS_*` variables](#store).

## Store

Workers with `PISA_STORE=s3` read the `AWS_*` variables, through the
`object_store` crate, and so does `pisa-search` for an `s3://` store. The
servers don't use the store.

| Variable | Default | What it does | Compose | Chart |
| --- | --- | --- | --- | --- |
| `AWS_BUCKET` | required | The bucket. | `pisa` | `store.s3.bucket` (required) |
| `AWS_REGION` | `us-east-1` | The bucket's region. | `garage` | `store.s3.region` |
| `AWS_ENDPOINT` | AWS S3 | The URL of another S3-compatible store, such as MinIO, Garage or R2. | `http://garage:3900` | `store.s3.endpoint` |
| `AWS_ALLOW_HTTP` | `false` | Allow an `http://` endpoint. | `true` | `store.s3.allowHttp` (false) |
| `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` | none | The bucket's credentials. Without them, the environment's are used: a web identity (`AWS_WEB_IDENTITY_TOKEN_FILE` and `AWS_ROLE_ARN`, as EKS IRSA sets them), container credentials, or the instance's role. | Garage's key | `store.s3.credentialsSecret` |
| other `AWS_*` | | Any other `object_store` reads, such as `AWS_SESSION_TOKEN`. | – | – (`worker.extraEnv`) |

## Model providers

A describe model whose name starts with `claude-` goes to Anthropic; any
other goes to OpenAI. The server's describe model reads their variables,
and so do `pisa-search informalize` and `validate-descriptions`.
`SCW_SECRET_KEY` is read by the embedder that names it, in the server and
in `pisa-search embed`, `eval` and `query`. The chart reads the keys from
the Secret `search.secret.name`, under the names `search.secret.*` give,
and skips missing ones.

| Variable | Default | What it does | Compose | Chart |
| --- | --- | --- | --- | --- |
| `ANTHROPIC_API_KEY` | none | The key for a `claude-` model. | empty (shell) | `search.secret.anthropicApiKey` (`ANTHROPIC_API_KEY`) |
| `ANTHROPIC_BASE_URL` | `https://api.anthropic.com` | Another endpoint for Anthropic's Messages API; `/v1/messages` is appended. | – | – (`server.extraEnv`) |
| `OPENAI_API_KEY` | none | The key for any other model. | empty (shell) | `search.secret.openaiApiKey` (`OPENAI_API_KEY`) |
| `OPENAI_BASE_URL` | `https://api.openai.com` | Another endpoint for OpenAI's Responses API; `/v1/responses` is appended. | – | – (`server.extraEnv`) |
| `SCW_SECRET_KEY` | none | Scaleway's key, which the default embedder reads through its `key_env=SCW_SECRET_KEY`. Any variable an embedder's `key_env=` names is read the same way. | empty (shell) | `search.secret.scwSecretKey` (`SCW_SECRET_KEY`) |

## Logging, TLS and the host

| Variable | Read by | Default | What it does | Compose | Chart |
| --- | --- | --- | --- | --- | --- |
| `RUST_LOG` | all three | `info` | The log filter, in `tracing`'s `EnvFilter` syntax, e.g. `info,pisa_worker=debug`. | `info` | servers: fixed `info`; workers: unset |
| `SSL_CERT_FILE`, `SSL_CERT_DIR` | server, worker, `pisa-search` | the system's roots | With `sslmode=require` in `PISA_DATABASE_URL`, the database's certificate is checked against the roots these name instead, as for a private CA. Other modes connect without TLS. | – | `server.extraEnv`, `worker.extraEnv` |
| `HOSTNAME` | worker | – | The start of the default `PISA_WORKER_ID`. Docker sets it to the container's id. | the container's id | – (`PISA_WORKER_ID` is set) |
| `TMPDIR` | worker | `/tmp` | Where the default `PISA_WORK_DIR` is. | – | – |
| `LEAN_PATH` | worker | none | Without `PISA_PROJECT`, the base of the search path Lean imports from; with a project, `lake env` gives it. | – | – |

`lake` and `lean --print-prefix` run with only `PATH`, `HOME`, `ELAN_HOME`,
`LANG` and `TMPDIR` from the worker's environment.

`PISA_REVISION`, at build time, names the commit the binaries report in
`/health`'s `revision` and `worker_revisions`; without it they say
`unknown`. `docker/Dockerfile` sets it from the build arg `REVISION`.

## Compose-only variables

`docker-compose.yml` reads these from your shell or a `.env` file; the
binaries don't (see [Variables](deploy-compose.md#variables)):

- `PISA_PORT` (`8000`): the host port Traefik listens on.
- `PROJECT` (`lean-project`): the directory of `docker/` the worker image is
  built on, `lean-project` (Mathlib, Physlib and CSLib) or `lean-core` (Lean
  alone). The image is `pisa-worker:<PROJECT>`.
- `PISA_REVISION` (`unknown`): passed to the images as the build arg
  `REVISION`, which the build hands to the binaries as `PISA_REVISION`.
- `PISA_SEARCH_BUNDLE` (`./search/bundle`): the bundle the `search-load`
  service loads into Qdrant.
- `COMPOSE_PROFILES`: Docker Compose's own. `search` adds Qdrant;
  `search-load` adds Qdrant and the `search-load` service.

Compose also passes `PISA_SEARCH_EMBEDDER`, `PISA_SEARCH_VERSION` and the
other variables marked "(shell)" above from your shell to the binaries,
which read them.

## Test and development variables

- `PISA_TEST_DATABASE_URL`: a PostgreSQL the Rust tests run against too,
  besides in-memory SQLite. Each test uses versions of its own there, so
  tests can share one database. CI sets it.
- `PISA_TEST_QDRANT_URL`: a Qdrant for the search tests, in Rust, Python
  and TypeScript; without it they are skipped.
- `PISA_UPDATE_OPENAPI`: with it set,
  `cargo test -p pisa-server --test openapi` writes `openapi.json` again.
- `PISA_UPDATE_STUBS`: with it set, `pytest tests/test_stubs.py`, in
  `crates/pisa-python`, writes the Python client's stubs again.
- `PISA_BIN_DIR` (`target/debug`): where the Python and TypeScript client
  tests find `pisa-server`, `pisa-worker`, `pisa-search` and the fakes.
- `PISA_URL`: the server `scripts/e2e.py`, `scripts/k8s_scale.py` and the
  playground's dev server talk to (`http://localhost:8000`), and the
  playground's end-to-end tests (`http://localhost:9100`).
