# Deploying on Kubernetes

This page is for operators who run pisa on Kubernetes with its Helm chart,
`deploy/helm/pisa`, published as `oci://ghcr.io/aqora-io/charts/pisa`. It
covers what the chart deploys and needs, the images, installing, storage,
scaling, draining, search, a trial on minikube, and every value the chart
takes. Running the cluster day to day is in [Operations](operations.md).

## What the chart deploys

Its resources are named after `<fullname>`: the release's name if it
contains `pisa`, else `<release>-pisa`, cut to 50 characters. `helm install
pisa …` gives `pisa`, and `helm install prod …` gives `prod-pisa`.

- **The servers:** the Deployment `<fullname>-server` (`server.replicas`,
  2), its Service and a PodDisruptionBudget, and with
  `server.autoscaling.enabled` a HorizontalPodAutoscaler in place of the
  fixed replicas.
- **The workers:** a Deployment per pool, `<fullname>-worker` for
  `worker`'s pool and `<fullname>-worker-<name>` for each of `pools` (see
  [Chart pools](environments.md#chart-pools)).
- **With `keda.enabled`** (the default): a KEDA ScaledObject per pool, named
  as its Deployment, and the TriggerAuthentication
  `<fullname>-worker-database`, through which KEDA reads the job queue.
- **With `serviceAccount.create`** (the default): the ServiceAccount
  `<fullname>`, which the servers and workers run as.
- **With `qdrant.enabled`:** the official
  [Qdrant chart](https://qdrant.github.io/qdrant-helm) as a subchart, for
  search.

The release's servers and workers share the job queue `<fullname>`
(`queue` names another), so releases that share a database keep their jobs
apart.

## Prerequisites

- **Helm** 3.8 or later, which installs charts from OCI registries.
- **KEDA** in the cluster, which scales the workers. Like most application
  charts, the chart uses the cluster's own and doesn't bundle it. Without
  KEDA, set `keda.enabled: false` (see [Scaling](#scaling)).
- **PostgreSQL**, which the chart doesn't deploy: run it with
  [CloudNativePG](https://cloudnative-pg.io), as a managed service, or by
  any other means. The servers and workers create the tables. KEDA connects
  to it too, from its own namespace, so the URL's host has to resolve there
  (a Service's full name, `<service>.<namespace>.svc.cluster.local`).
- **An S3-compatible bucket**, for the snapshot modules.
- **Nodes that run privileged pods, with cgroup v2.** Workers run
  privileged so that bubblewrap can sandbox each Lean process and the
  worker can give each one a cgroup with memory and process limits. A
  worker that can't set its cgroups up doesn't start. On nodes without
  cgroup v2, set `worker.cgroups: false` to run Lean without those limits;
  with `worker.sandbox: none` too, workers run unprivileged and unsandboxed.
  Acceptance needs bubblewrap either way, for comparator's sandbox, and a
  worker logs a warning as it starts if bubblewrap can't make it.
- **Room for the workers.** By default a worker pod requests 3 CPUs (one
  per slot and Beam slot) and has a memory limit of 24512 MiB (see
  [Worker resources](#worker-resources)). The default worker image is about
  20 GB.

## Images

Each release publishes to ghcr.io:

| Image | What it is |
| --- | --- |
| `ghcr.io/aqora-io/pisa-server:1.0.0` | The server, and `pisa-search` for loading search bundles |
| `ghcr.io/aqora-io/pisa-worker:1.0.0-v4.36.0-54500a67` | A worker on `docker/lean-project`: Lean v4.36.0 with Mathlib, Physlib and CSLib, about 20 GB |
| `ghcr.io/aqora-io/pisa-worker:1.0.0-v4.36.0-ca45ccf8` | A worker on `docker/lean-core`: Lean alone |

The chart uses them by default: the server tagged with the chart's
`appVersion`, and each pool's worker tagged `<appVersion>-<version>`,
`worker.version` being `v4.36.0-54500a67`. For the Lean-only workers, set
`worker.version=v4.36.0-ca45ccf8`.

To build your own, from the repository root, and push them to your
registry:

```sh
REVISION=$(git rev-parse HEAD)
docker build -f docker/Dockerfile --target server --build-arg REVISION=$REVISION \
  -t <registry>/pisa-server:1.0.0 .
docker build -f docker/Dockerfile --target worker --build-arg PROJECT=lean-project \
  --build-arg REVISION=$REVISION -t <registry>/pisa-worker:1.0.0-v4.36.0-54500a67 .
docker push <registry>/pisa-server:1.0.0
docker push <registry>/pisa-worker:1.0.0-v4.36.0-54500a67
```

then set `server.image.repository` and `worker.image.repository` to them.
Tagged so, the chart's default tags find them. `REVISION` names the commit
in `/health` and the images' labels. An image for another environment is
built by `scripts/env.py build --repository <registry>/pisa-worker --push`
(see [Environments](environments.md)). For a private registry, name a pull
secret in `imagePullSecrets`.

## Installing

Put the database's URL and the bucket's credentials in Secrets, then
install the published chart:

```sh
helm repo add kedacore https://kedacore.github.io/charts
helm install keda kedacore/keda -n keda --create-namespace --version 2.21.0
kubectl create secret generic pisa-db \
  --from-literal=url='postgres://pisa:...@<host>/pisa?sslmode=require'
kubectl create secret generic pisa-s3 \
  --from-literal=AWS_ACCESS_KEY_ID=... --from-literal=AWS_SECRET_ACCESS_KEY=...
helm install pisa oci://ghcr.io/aqora-io/charts/pisa --version 1.0.0 \
  --set metadata.database.urlSecret.name=pisa-db \
  --set store.s3.bucket=<bucket> --set store.s3.region=<region> \
  --set store.s3.credentialsSecret.name=pisa-s3
```

`metadata.database.urlSecret.name` and `store.s3.bucket` are required.
From a clone, build the chart's dependencies first, which fetches the Qdrant
subchart into `deploy/helm/pisa/charts/` (the published chart includes it):

```sh
helm dependency build deploy/helm/pisa
helm install pisa deploy/helm/pisa --set …
```

Then reach the API, as the install's notes say:

```sh
kubectl port-forward svc/pisa-server 8000:8000 &
curl -s localhost:8000/health
curl -s localhost:8000/check -H 'content-type: application/json' \
  -d '{"code":"theorem t : 2 + 2 = 4 := by decide"}'
```

The first worker pod pulls its image before it registers; `/health`'s
`workers` counts the live ones.

## Storage

The job queue and the metadata (documents, sessions and their states,
snapshots and module records) live in PostgreSQL, and the snapshot modules
in the bucket.

- **The database.** `metadata.database.urlSecret.name` names the Secret
  that holds its URL, under `metadata.database.urlSecret.key` (`url`). The
  servers, the workers and KEDA read it. PostgreSQL holds the only copy of
  the snapshots and module records, which never expire, so it must be
  durable and backed up. Connect directly, or through PgBouncer in session
  mode, not transaction mode. With `sslmode=require` the server's
  certificate is checked against the image's roots; for a private CA, set
  `SSL_CERT_FILE` to its certificate through `server.extraEnv` and
  `worker.extraEnv`. The chart has no values for mounting files, so the
  certificate has to be in the image.
- **The bucket.** `store.s3.bucket` and `store.s3.region` name it.
  `store.s3.endpoint` and `store.s3.allowHttp` point the workers at a
  non-AWS store such as MinIO or Garage.
- **Its credentials.** Give them as an existing Secret,
  `store.s3.credentialsSecret.name`, with `accessKeyIdKey` and
  `secretAccessKeyKey` naming its keys. Or leave the name empty to use the
  pod's ambient credentials, such as an instance role or EKS IRSA: annotate
  the service account with the role,

  ```yaml
  serviceAccount:
    annotations:
      eks.amazonaws.com/role-arn: arn:aws:iam::<account>:role/<role>
  ```

  or set `serviceAccount.create: false` and `serviceAccount.name` to use one
  you manage.

## Scaling

**Workers** scale on their pool's outstanding jobs, queued plus running,
which KEDA's `postgresql` trigger counts in the job queue every
`keda.pollingInterval` seconds, through the database Secret.

- The target is `keda.jobsPerWorker` jobs per pod, the pool's `slots` by
  default: a pool runs as many pods as its outstanding jobs need, between
  `keda.minReplicas` and `keda.maxReplicas` (or the pool's own).
- `keda.scaleDownStabilizationSecs` is how long demand must stay low before
  pods are removed. Removed workers drain, so this trades idle pods against
  cold imports.
- With `minReplicas` 0 a pool runs no pod until a job names its version;
  the request then waits for a pod to start and pull its image, so keep
  `router.maxWaitSecs` above that. `keda.cooldownPeriod` is how long such a
  pool stays up after its last job.
- With `keda.enabled: false` the chart makes no ScaledObject, and each
  pool's Deployment runs `minReplicas` pods.

**Servers** scale on their own: `server.replicas`, or
`server.autoscaling` on CPU, which needs the cluster's metrics server. Any
replica serves any request, as the jobs are in the database, and the
servers spread over nodes where they can.

## Pools

`worker` is the release's pool, serving `worker.version`, and each entry of
`pools` another, for [another environment](environments.md): a Deployment
and a ScaledObject of its own, which scales on the jobs of its version
alone. [Chart pools](environments.md#chart-pools) describes them.

## Draining

A worker that receives SIGTERM claims no more jobs, finishes the ones it
has, and exits, waiting at most `worker.drainTimeoutSecs` (600) for them;
its pod's grace period is 30 s longer.

A server that receives SIGTERM answers `/health` with a 503 `draining` but
keeps taking requests for 5 s, while its readiness probe takes it out of
the Service, then waits up to `server.drainTimeoutSecs` (360) for those in
flight; its grace period is 30 s longer. A request it cuts off still runs,
as its job is in the queue. Keep `server.drainTimeoutSecs` above
`router.maxWaitSecs` plus the longest job. The servers have a readiness
probe on `/health` and no liveness probe, which would fail during the
drain and kill the replica before it finished.

Scale-downs and rollouts therefore don't cut jobs off. More in
[Draining](operations.md#draining).

## Search

`search.enabled` has the servers search declarations, over Qdrant: the
Qdrant subchart (`qdrant.enabled`, 3 nodes) or one you run
(`search.qdrant.url`). One of them is required.

- `search.version` is the version of the loaded bundle, `worker.version`'s
  by default.
- `search.embedder` embeds queries and snapshot declarations, and must
  serve the model that embedded the bundle. The default is Qwen3-Embedding-8B
  on Scaleway, which needs `SCW_SECRET_KEY`.
- Give API keys as an existing Secret, `search.secret.name`, under the keys
  `search.secret.scwSecretKey`, `anthropicApiKey` and `openaiApiKey` name;
  missing ones are skipped. A Qdrant that needs an API key takes it from
  `search.qdrant.apiKeySecret`.
- The servers search snapshots too, unless `search.snapshots` is false.
  `search.describeModel` has a Claude or OpenAI model describe every
  snapshot declaration, which sends all snapshot code to that provider (see
  [What snapshot search sends to a model](search.md#what-snapshot-search-sends-to-a-model)).

Load each bundle into Qdrant once, and after each rebuild, for example
through a port-forward. The server image has `pisa-search`:

```sh
kubectl port-forward svc/<release>-qdrant 6333 &
# on Linux, from the repository root, with the bundle in search/bundle
docker run --rm --network host -v "$PWD/search/bundle:/bundle:ro" \
  --entrypoint pisa-search ghcr.io/aqora-io/pisa-server:1.0.0 \
  load --bundle /bundle --qdrant http://localhost:6333 --replication 2
```

`--replication 2` keeps a copy of each collection when a Qdrant node goes.
Until the bundle is loaded, `/search` answers 503, `/health` says why, and
the rest of the API works. [Search](search.md) describes the API, and
[Search indexes](search-index.md) where bundles come from and other ways to
get `pisa-search`.

## On minikube

To try the chart with images built locally, on `docker/lean-core`, with the
throwaway PostgreSQL and Garage in `deploy/minikube/`:

```sh
PROJECT=lean-core docker compose build
minikube start -p pisa --driver=docker --cpus=8 --memory=20g
helm repo add kedacore https://kedacore.github.io/charts
helm install keda kedacore/keda -n keda --create-namespace --version 2.21.0
minikube -p pisa image load pisa-server:latest pisa-worker:lean-core
kubectl apply -f deploy/minikube/garage.yaml   # a throwaway S3 store and its Secret
kubectl apply -f deploy/minikube/postgres.yaml # a throwaway PostgreSQL and its Secret
helm dependency build deploy/helm/pisa
helm install pisa deploy/helm/pisa -f deploy/minikube/values.yaml
kubectl port-forward svc/pisa-server 8000:8000 &
python3 scripts/e2e.py --no-failover   # checks, policy, sessions against real Lean
python3 scripts/k8s_scale.py           # scales up under load, back down, drains on delete
```

`deploy/minikube/values.yaml` points the chart at the local images and at
`lean-core`'s version, gives each Lean process 1500 MiB, and scales down
after 30 s.

## Not covered by the chart

- PostgreSQL and the bucket.
- Building a pool's image, which `scripts/env.py` does, and adding a pool
  without a `helm upgrade`.
- Auth on the API, and an Ingress: the chart makes a Service of
  `server.service.type`, `ClusterIP` by default.
- Mounting files into the pods.
- A tighter security context: workers run privileged so bubblewrap and
  cgroup limits work.

## Values reference

Every value in `deploy/helm/pisa/values.yaml`, with its default. Variables
in the descriptions are the settings the chart sets from them, which
[Configuration](configuration.md) describes.

### Server

| Key | Default | Description |
| --- | --- | --- |
| `server.image.repository` | `ghcr.io/aqora-io/pisa-server` | The server's image, built from `docker/Dockerfile`'s target `server` |
| `server.image.tag` | `""` | Empty is the chart's `appVersion` |
| `server.image.pullPolicy` | `IfNotPresent` | The image's pull policy |
| `server.replicas` | `2` | Replicas, unless `server.autoscaling.enabled` |
| `server.service.type` | `ClusterIP` | The Service's type |
| `server.service.port` | `8000` | The Service's port; the container listens on 8000 |
| `server.resources` | requests `cpu: 500m, memory: 256Mi`; limits `memory: 1Gi` | The server container's resources |
| `server.drainTimeoutSecs` | `360` | On SIGTERM, how long a replica waits for its requests in flight (`PISA_DRAIN_TIMEOUT_SECS`); its grace period is 30 s longer. Keep it above `router.maxWaitSecs` plus the longest job. |
| `server.minAvailable` | `1` | Replicas the PodDisruptionBudget keeps through voluntary disruptions such as node drains |
| `server.autoscaling.enabled` | `false` | A HorizontalPodAutoscaler on CPU in place of `server.replicas` |
| `server.autoscaling.minReplicas` | `2` | The autoscaler's fewest replicas |
| `server.autoscaling.maxReplicas` | `6` | Its most replicas |
| `server.autoscaling.targetCPUUtilizationPercentage` | `70` | The average CPU use, as a share of the request, it aims at |
| `server.corsOrigins` | `[]` | Origins whose pages may call the API from a browser, e.g. `https://app.example.com`, or `"*"` for any (`PISA_CORS_ORIGINS`) |
| `server.extraEnv` | `[]` | More environment variables, as a container's `env` entries, e.g. `SSL_CERT_FILE` |
| `server.nodeSelector` | `{}` | The server pods' node selector |
| `server.tolerations` | `[]` | Their tolerations |
| `server.affinity` | `{}` | Their affinity |

### Release

| Key | Default | Description |
| --- | --- | --- |
| `imagePullSecrets` | `[]` | Secrets for pulling the images from a private registry, e.g. `[{name: regcred}]`, for every pod |
| `serviceAccount.create` | `true` | Create the servers' and workers' service account |
| `serviceAccount.name` | `""` | Its name; empty is `<fullname>` when created, `default` when not |
| `serviceAccount.annotations` | `{}` | Its annotations, e.g. EKS IRSA's `eks.amazonaws.com/role-arn` for the bucket |
| `queue` | `""` | The job queue the release's servers and workers share (`PISA_QUEUE`); empty is `<fullname>` |
| `allowedAxioms` | `[]` | The axioms every proof may use, on the servers and the workers alike (`PISA_ALLOWED_AXIOMS`); empty keeps the default, `propext`, `Classical.choice` and `Quot.sound` |

### Router

| Key | Default | Description |
| --- | --- | --- |
| `router.maxWaitSecs` | `300` | Seconds a request may wait for a free worker (`PISA_MAX_WAIT_SECS`). Keep it above the time a new worker pod takes to start, so requests queued during a scale-up don't fail. |
| `router.maxRetries` | `2` | How many more times a job whose worker died runs (`PISA_MAX_RETRIES`) |
| `router.defaultVersion` | `""` | The version of requests that name none (`PISA_DEFAULT_VERSION`); empty is `worker.version` |
| `router.aliases` | `[]` | `NAME=VERSION` aliases (`PISA_ALIASES`) |

### Worker

`worker` is the release's own pool, and every pool's defaults.

| Key | Default | Description |
| --- | --- | --- |
| `worker.version` | `v4.36.0-54500a67` | The version the pool's image serves, as `scripts/env.py` prints it and `GET /environments` lists it. The pool scales on its version's jobs and those that name none, requests that name none go to it unless `router.defaultVersion` says otherwise, and a worker whose image serves another doesn't start (`PISA_EXPECT_VERSION`). Empty, the pool scales on every job; `pools` need it. |
| `worker.image.repository` | `ghcr.io/aqora-io/pisa-worker` | The workers' image, built from `docker/Dockerfile`'s target `worker` on an environment's project |
| `worker.image.tag` | `""` | Empty is `<appVersion>-<version>`, as `scripts/env.py` tags it |
| `worker.image.pullPolicy` | `IfNotPresent` | The image's pull policy |
| `worker.slots` | `2` | Lean REPL processes per pod, for checks, snapshots and acceptances (`PISA_SLOTS`) |
| `worker.sandbox` | `bwrap` | `bwrap` or `none`: a bubblewrap sandbox per Lean process (`PISA_SANDBOX`). `bwrap` makes the pod privileged. |
| `worker.cgroups` | `true` | Give each Lean process a cgroup with its limits (`PISA_CGROUP_ROOT=/sys/fs/cgroup`), which makes the pod privileged. A worker that can't set them up doesn't start; `false` runs Lean without per-process limits. |
| `worker.memoryMaxMb` | `8000` | Each REPL process's memory limit, in MiB (`PISA_MEMORY_MAX_MB`); also sizes the pod's memory limit |
| `worker.maxRssMb` | `6000` | The resident memory, in MiB, at which a REPL process is replaced (`PISA_MAX_RSS_MB`) |
| `worker.prewarm` | `""` | Headers to import at startup, separated by `;`, e.g. `import Mathlib` (`PISA_PREWARM`) |
| `worker.beamSlots` | `1` | Lean Beam processes per pod, for documents and tactic sessions, beside the slots; 0 serves neither (`PISA_BEAM_SLOTS`) |
| `worker.beamDocuments` | `4` | Documents each Beam process keeps open (`PISA_BEAM_DOCUMENTS`) |
| `worker.beamMemoryMaxMb` | `8000` | Each Beam process's memory limit, in MiB (`PISA_BEAM_MEMORY_MAX_MB`); also sizes the pod's memory limit |
| `worker.drainTimeoutSecs` | `600` | On scale-down, how long a worker finishes its jobs (`PISA_DRAIN_TIMEOUT_SECS`); the pod's grace period is 30 s longer |
| `worker.leaseSecs` | `30` | How long a worker's claim on a job lasts without a heartbeat, which comes every third of it (`PISA_LEASE_SECS`). A job whose worker died waits this long for another; a shorter lease lets a worker's stall hand its job to another while it still runs it. |
| `worker.overheadMemoryMb` | `512` | Pod memory besides the Lean processes: the memory request, and part of the limit |
| `worker.resources` | `{}` | The pod's resources; empty is [computed](#worker-resources) |
| `worker.extraEnv` | `[]` | More environment variables, e.g. `PISA_PROJECT_GIT` and `PISA_PROJECT_REV` |
| `worker.nodeSelector` | `{}` | The worker pods' node selector |
| `worker.tolerations` | `[]` | Their tolerations |
| `worker.affinity` | `{}` | Their affinity |

#### Worker resources

With `worker.resources` empty, a worker pod requests:

- CPU: `slots + beamSlots`, about a core per Lean process (3 by default);
- memory: `overheadMemoryMb` (512 MiB);

and its memory limit is `slots × memoryMaxMb + beamSlots × beamMemoryMaxMb +
overheadMemoryMb`, every process at its cap (24512 MiB by default). The
scheduler places pods by their request, which is small, so the limits of a
node's workers can add up to more memory than it has, and a Lean process
can then be killed for memory its limit promised. Set `worker.resources` to
request what the processes may use.

### Pools

| Key | Default | Description |
| --- | --- | --- |
| `pools` | `[]` | Worker pools for other environments, beside `worker`'s. Each is a Deployment and a ScaledObject of its own, on the release's servers, database and store, and scales on the jobs of its version alone. |
| `pools[].version` | required | The version its image serves |
| `pools[].name` | the version, with dashes for dots | What its Deployment's name ends in |
| `pools[].image` | `worker.image`, tagged `<appVersion>-<version>` | Its image |
| `pools[].minReplicas`, `pools[].maxReplicas` | `keda`'s | Its replicas. With 0 it runs no pod until a request names its version. |
| `pools[].<key>` | `worker.<key>` | Any of `worker`'s keys, e.g. `slots` or `beamSlots` |

### Store

| Key | Default | Description |
| --- | --- | --- |
| `store.s3.bucket` | `""` | Required: the bucket for snapshot modules, which all workers of a release share (`AWS_BUCKET`) |
| `store.s3.region` | `""` | `AWS_REGION` |
| `store.s3.endpoint` | `""` | For S3-compatible stores (MinIO, Garage, R2, …); empty is AWS S3 (`AWS_ENDPOINT`) |
| `store.s3.allowHttp` | `false` | Allow a plain-HTTP endpoint (`AWS_ALLOW_HTTP`) |
| `store.s3.credentialsSecret.name` | `""` | An existing Secret holding the credentials; empty uses the pod's ambient credentials |
| `store.s3.credentialsSecret.accessKeyIdKey` | `AWS_ACCESS_KEY_ID` | Its key for the access key id |
| `store.s3.credentialsSecret.secretAccessKeyKey` | `AWS_SECRET_ACCESS_KEY` | Its key for the secret access key |
| `store.stateTtlSecs` | `86400` | Delete documents, sessions and their states older than this; 0 keeps them (`PISA_STATE_TTL_SECS`) |

### Database

| Key | Default | Description |
| --- | --- | --- |
| `metadata.database.urlSecret.name` | `""` | Required: an existing Secret holding the PostgreSQL URL, e.g. `postgres://pisa:...@db.example.com/pisa?sslmode=require` (`PISA_DATABASE_URL`). KEDA reads the queue through it too. |
| `metadata.database.urlSecret.key` | `url` | Its key |

### KEDA

| Key | Default | Description |
| --- | --- | --- |
| `keda.enabled` | `true` | Scale the workers with KEDA. False makes no ScaledObject or TriggerAuthentication, and runs each pool at `minReplicas`. |
| `keda.minReplicas` | `1` | Every pool's fewest pods, unless it sets its own. 0 lets a pool scale to zero; the first request then waits for a cold worker, so keep `router.maxWaitSecs` above the worker's startup time. |
| `keda.maxReplicas` | `4` | Every pool's most pods, unless it sets its own |
| `keda.scaleDownStabilizationSecs` | `300` | Seconds of low demand before workers are removed |
| `keda.pollingInterval` | `15` | How often KEDA counts the jobs, which bounds how quickly workers react to load |
| `keda.cooldownPeriod` | `300` | With `minReplicas` 0, how long a pool stays up after its last job |
| `keda.jobsPerWorker` | `null` | Outstanding jobs (queued and running) per worker pod; empty is the pool's `slots` |

### Search

| Key | Default | Description |
| --- | --- | --- |
| `search.enabled` | `false` | Declaration search in the servers, over Qdrant |
| `search.version` | `""` | The version of the bundle loaded into Qdrant, its `env_id` (`PISA_SEARCH_VERSION`); empty is `worker.version` |
| `search.embedder` | `openai+https://api.scaleway.ai/v1?model=qwen3-embedding-8b&key_env=SCW_SECRET_KEY` | Embeds queries and snapshot declarations; it must serve the model that embedded the bundle (`PISA_SEARCH_EMBEDDER`) |
| `search.snapshots` | `true` | Search the workers' snapshots; the replica holding a lease in the database indexes them (`PISA_SEARCH_SNAPSHOTS`) |
| `search.describeModel` | `""` | A Claude or OpenAI model that describes every snapshot declaration, so all snapshot code goes to that provider; empty for none (`PISA_SEARCH_DESCRIBE_MODEL`) |
| `search.describeConcurrency` | `8` | Model and embedding requests in flight for snapshots (`PISA_SEARCH_DESCRIBE_CONCURRENCY`) |
| `search.secret.name` | `""` | An existing Secret with the API keys; missing keys are skipped |
| `search.secret.scwSecretKey` | `SCW_SECRET_KEY` | Its key for `SCW_SECRET_KEY` |
| `search.secret.anthropicApiKey` | `ANTHROPIC_API_KEY` | Its key for `ANTHROPIC_API_KEY` |
| `search.secret.openaiApiKey` | `OPENAI_API_KEY` | Its key for `OPENAI_API_KEY` |
| `search.qdrant.url` | `""` | A Qdrant you run; empty is this release's Qdrant subchart (`PISA_SEARCH_QDRANT`) |
| `search.qdrant.apiKeySecret.name` | `""` | An existing Secret holding Qdrant's API key, if it needs one |
| `search.qdrant.apiKeySecret.key` | `api-key` | Its key |
| `qdrant.enabled` | `false` | Deploy the official Qdrant chart, used when `search.qdrant.url` is empty |
| `qdrant.replicaCount` | `3` | Its nodes. Load bundles with `--replication 2`, so that each collection keeps a copy when a node goes. |

The subchart takes the rest of the
[Qdrant chart](https://qdrant.github.io/qdrant-helm)'s values under
`qdrant`.
