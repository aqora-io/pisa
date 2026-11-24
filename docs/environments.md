# Environments and pools

This page is for operators who serve Lean code on another toolchain or
another library than the default: what an environment and its version are,
how `scripts/env.py` builds a worker image for one, and how its pool of
workers runs beside the others with Compose or the Helm chart.

## Environments and versions

An environment is a Lean toolchain and the Lake project its workers build:
the libraries requests may import. Its version labels it:
`<toolchain>-<hash>`, such as `v4.36.0-54500a67`, where the hash is the
first 8 hex digits of a SHA-256 over the project's `lake-manifest.json`, its
lakefile and its top-level `.lean` files. A worker computes its version from
its project (`PISA_PROJECT`) as it starts, unless `PISA_VERSION` names one.

A worker serves one environment, and the workers of one version are a pool.
Another toolchain, another library, or another Mathlib is another
environment, with a pool of its own on the same servers, database, store
and queue. A request reaches a pool by naming its version (see
[Reaching a pool](#reaching-a-pool)).

## The environments in this repository

| Project | Version | What it has | Image |
| --- | --- | --- | --- |
| `docker/lean-project` | `v4.36.0-54500a67` | Lean v4.36.0 with Mathlib, Physlib and CSLib | `ghcr.io/aqora-io/pisa-worker:1.0.0-v4.36.0-54500a67`, about 20 GB |
| `docker/lean-core` | `v4.36.0-ca45ccf8` | Lean v4.36.0 alone | `ghcr.io/aqora-io/pisa-worker:1.0.0-v4.36.0-ca45ccf8` |

Compose builds its workers on one of them (`PROJECT`), and the chart's
`worker` pool serves `docker/lean-project`'s by default.

To move `docker/lean-project` to another Lean release, change its
`lean-toolchain` and its requirements' `rev`s together, and `LEAN_TOOLCHAIN`
and `REPL_REV` in `docker/Dockerfile` with them. The image applies
`docker/repl/infotree-at.patch` to the REPL, which adds `infotreeAt` (see
[Goals and types at positions](checking.md#goals-and-types-at-positions)):
it must apply to the new `REPL_REV` too.

## An environment's file

`scripts/env.py` builds an environment from a TOML file you write, which
says what it is. For Mathlib on Lean v4.35.0, say
`environments/mathlib-4-35.toml`:

```toml
toolchain = "leanprover/lean4:v4.35.0"
imports = ["Mathlib"]   # the modules its workers build, which requests may import

[[require]]             # as in a lakefile.toml
name = "mathlib"
git = "https://github.com/leanprover-community/mathlib4.git"
rev = "v4.35.0"
```

| Key | Default | What it is |
| --- | --- | --- |
| `toolchain` | required | The toolchain, as a `lean-toolchain` file says it |
| `imports` | `[]` | The modules the image builds, which requests may import. Workers replay the builds read-only, so list what agents will import. |
| `repl` | the toolchain's tag, e.g. `v4.35.0` | The tag of [leanprover-community/repl](https://github.com/leanprover-community/repl) to build |
| `[[require]]` | none | The project's requirements, as in a `lakefile.toml`; each has a `name`, and every value is a string |

The script refuses other keys. The toolchain has to be the one the
requirements are on: Lake moves a project to the toolchain of a requirement
on a newer one, and the script then stops and says which toolchain to
name.

## scripts/env.py

### build

```sh
python3 scripts/env.py build environments/mathlib-4-35.toml
```

1. It writes the environment's Lake project beside the file, in a
   directory of its name (`environments/mathlib-4-35/`): a wrapper project
   like `docker/lean-project`, with a `lakefile.toml` that requires what the
   file requires, its `lean-toolchain`, and `PisaProject.lean`, which imports
   the file's `imports`.
2. It has Lake resolve the project's `lake-manifest.json`, in Docker
   (`docker/Dockerfile`'s target `manifest`).
3. It builds `docker/Dockerfile`'s target `worker` on that project, and tags
   the image `<repository>:<release>-<version>` (see
   [Image tags](#image-tags)).
4. It prints the version and the image:

   ```text
   version=v4.35.0-<hash>
   image=pisa-worker:1.0.0-v4.35.0-<hash>
   ```

Docker's output goes to stderr. The project doesn't say which file it came
from, so two files that say the same are one environment.

Commit the file and the directory: the manifest pins what the version
hashes. The script keeps the manifest while the files it writes are
unchanged, and resolves it again when the file changes, or with `--update`.

| Flag | Default | What it does |
| --- | --- | --- |
| `--out <dir>` | beside the file, named as it is | Where the project goes |
| `--update` | off | Resolve the dependencies again, as to follow a branch |
| `--project-only` | off | Write the project and print its version; build no image |
| `--repository <name>` | `pisa-worker` | The image's name, without its tag, e.g. `ghcr.io/<you>/pisa-worker` |
| `--revision <commit>` | `git rev-parse HEAD` | pisa's commit, which the image reports in `/health` and its label |
| `--push` | off | Push the image once it is built |

### label

```sh
python3 scripts/env.py label docker/lean-core    # v4.36.0-ca45ccf8
```

prints the version of a Lake project, as its workers compute it. A project
without a `lake-manifest.json` gets one in its image, as the image runs
`lake update` for it, which makes its version another; the script says so on
stderr.

## Image tags

A worker image is tagged `<release>-<version>`: pisa's release, as
`Cargo.toml` has it, and the environment's version, such as
`1.0.0-v4.36.0-54500a67`. The release says which pisa worker the image runs,
and the version which environment.
`scripts/env.py build` tags images so, and the chart's pools look for that
tag by default, with the chart's `appVersion` as the release.

## Building by hand

`scripts/env.py` does what follows; this is for doing it without the
script.

### The project

A wrapper, like `docker/lean-project`: a `lakefile.toml` that requires the
library at a commit, its `lake-manifest.json` and `lean-toolchain`, and a
library file importing the modules agents will import, which the image
builds. Not the library's own checkout: an
[export](acceptance.md#the-export) requires the project's packages, and a
checkout's own code isn't one. For a checkout, set `PISA_PROJECT_GIT` and
`PISA_PROJECT_REV` on its workers (the chart's `extraEnv`) to where its code
comes from, or exports of runs that import it are refused.

### The image

```sh
docker build -f docker/Dockerfile --target worker \
  --build-context project=<dir> \
  --build-arg LEAN_TOOLCHAIN=<its toolchain> --build-arg REPL_REV=<the same Lean> \
  --build-arg REVISION=<pisa's commit> \
  -t pisa-worker:1.0.0-<version> .
```

takes the project from `<dir>` in place of `docker/${PROJECT}`. A project
without a `lake-manifest.json` is resolved in the image. Mathlib, if the
manifest has it, comes from its cache; the rest compiles once. The image
also builds:

- the REPL at `REPL_REV`, with pisa's patch, which has to apply to that
  tag;
- pisa's Lean tool and library (`docker/pisa-lean`) on the project's
  toolchain, which has to be Lean 4.34.1 or later: the kernel re-check uses
  `Lean.Kernel.Environment.replay`.

Lean Beam comes with the toolchain from Lean 4.36. A pool on an earlier one
serves checks and snapshots, and no documents or sessions, so give it no
Beam slots: `PISA_BEAM_SLOTS=0`, or `beamSlots: 0` in the chart.

## Running a pool

### With Compose

A service for the pool extends `worker` with its image. Compose reads a
`docker-compose.override.yml` beside `docker-compose.yml` with it, and
resolves `extends: file:` against the project directory:

```yaml
services:
  # Requests that name no version go to the default pool.
  server:
    environment:
      PISA_DEFAULT_VERSION: v4.36.0-54500a67
  worker-mathlib-4-35:
    extends: { file: docker-compose.yml, service: worker }
    build: !reset null        # its image is scripts/env.py's, not Compose's
    image: pisa-worker:1.0.0-v4.35.0-<hash>
    deploy: { replicas: 1 }
    environment:
      PISA_BEAM_SLOTS: 0      # Lean Beam needs Lean 4.36
```

Without `build: !reset null` (Compose 2.24 or later) the service would
inherit `worker`'s build, and `docker compose up --build` would build
`docker/${PROJECT}` under the pool's tag. Without a default version, a
request that names none goes to whichever pool takes it first, and a
snapshot read that names none is a 400 once two versions are live.

### Chart pools

`worker` is the chart's own pool, and each entry of `pools` another: a
Deployment and, with KEDA, a ScaledObject of its own, which scales on the
jobs of its version alone.

```yaml
worker:
  version: v4.36.0-54500a67    # required with pools: what its image serves
pools:
  - version: v4.36.0-ca45ccf8  # docker/lean-core's, published with each release
    minReplicas: 0
    maxReplicas: 2
  - version: v4.35.0-<hash>    # as scripts/env.py printed it
    image: { repository: <registry>/pisa-worker }
    beamSlots: 0               # Lean Beam needs Lean 4.36
    minReplicas: 0
    maxReplicas: 2
```

- A pool has `worker`'s settings, and overrides those it names, `slots` or
  `prewarm` say. Maps, such as `nodeSelector` or `image`, merge with
  `worker`'s; lists, such as `tolerations` or `extraEnv`, replace them.
- Its image is `worker.image.repository` tagged
  `<appVersion>-<version>`, as `scripts/env.py` tags it, unless its `image`
  says otherwise; `worker.image.tag` doesn't carry over.
- Its Deployment and ScaledObject are `<fullname>-worker-<name>`, `name`
  being its version with dashes for dots unless set, and `<fullname>` the
  release's name if it contains `pisa`, else `<release>-pisa`.
- `minReplicas` and `maxReplicas` are `keda`'s unless set. Without KEDA
  (`keda.enabled: false`) a pool runs `minReplicas` pods.
- With 0 a pool runs no pod while it has no job: a check or a snapshot
  request that names its version starts one, and waits for it, the image's
  pull included, so keep `router.maxWaitSecs` above that. A session or a
  document doesn't: while no worker of the version serves documents it is a
  503 `unavailable` at once, so send a check first, or keep a pool agents
  prove on at one replica.
- With `worker.version` set, `worker`'s pool counts its version's jobs and
  those of no version, and requests that name none go to it
  (`router.defaultVersion` is it by default). Without it, and without
  `pools`, it counts every job.

[Deploying on Kubernetes](deploy-kubernetes.md) has the chart's other
values.

## Reaching a pool

- A request names a version in `version`: the label, or an alias that
  `PISA_ALIASES` (`name=version`, comma-separated; the chart's
  `router.aliases`) sets on the servers. A request that names none gets
  `PISA_DEFAULT_VERSION` (the chart's `router.defaultVersion`).
- Snapshot, document, session and state ids carry their version, so
  requests about them name none.
- `/health` lists each live pool in `pools`, with its toolchain and the
  Lake packages its project pins, so an agent can pick the pool that has a
  library.
- A worker is told the version its pool names, `PISA_EXPECT_VERSION`, which
  the chart sets from the pool's `version`, and doesn't start if its
  image's project has another: a pool that scaled on one version's jobs and
  ran them on another would never drain them.

## Listing environments

`GET /environments` lists every version a worker has registered, with the
project behind it, whether or not it has live workers (see
[`GET /environments`](api.md#get-environments)):

- `toolchain` and `packages` are what its project pins, as `/health`'s pools
  have them.
- `files` has the project's files by name: `lean-toolchain`, the lakefile,
  the manifest and the top-level `.lean` files.
- `created` is when a worker first registered it.
- `pool` has its live workers, and is absent while it has none, as when
  its pool is scaled to zero or removed.

An environment stays listed after its workers are gone, as its snapshots
stay, so what a snapshot's version was can be read, and built again.
Written to a directory, its `files` are the project again, of the same
version:

```sh
curl -s localhost:8000/environments | python3 -c '
import json, pathlib, sys
version = sys.argv[1]
env = next(e for e in json.load(sys.stdin)["environments"] if e["version"] == version)
out = pathlib.Path(version)
out.mkdir()
for name, text in env["files"].items():
    (out / name).write_text(text)
' v4.36.0-54500a67
python3 scripts/env.py label v4.36.0-54500a67    # the same version
```

and [the image](#the-image) builds on that directory. Workers started with
a version of their own choosing (`PISA_VERSION`) list what the latest of
them serves.

## What a pool lacks

- **Search.** Search indexes one version (`PISA_SEARCH_VERSION`), so agents
  on another pool find names with `#check`, `#print` and `exact?`. A search
  that names another version answers `not_found`, saying which version
  search covers and that `/health` lists the pools.
- **Documents and sessions on toolchains before Lean 4.36**, which ship no
  Lean Beam (see [The image](#the-image)).
- **Waking for documents.** A pool scaled to zero starts for a check or a
  snapshot request, not for a session or a document.
- **Cheap acceptance of large libraries.**
  [Acceptance](acceptance.md) replays what each pair imports, so a large
  library costs minutes per pair.
- **Building from the API.** The chart doesn't build images, and a new pool
  takes a `helm upgrade`.
