# pisa

pisa serves Lean 4 to machines. Agents, pipelines and services send it Lean
code over HTTP, and it checks the code, runs proof sessions, keeps accepted
code as modules that later code imports, judges agents' runs against their
specs, and searches declarations. It runs unmodified upstream
[Lean REPL](https://github.com/leanprover-community/repl) and
[Lean Beam](https://github.com/leanprover/lean4) processes in sandboxed
workers, behind servers that hold no state and scale out.

## What it does

- **Checks.** `POST /check` elaborates code and judges it. A proof counts by
  the axioms it depends on, not by string matching. A verification policy
  rejects metaprogramming and unsafe options before the code runs. Each
  Lean process runs in a bubblewrap sandbox, with its own cgroup limits.
- **Proof sessions.** A session opens code with `sorry`s, and a tactic runs
  from a `sorry` or from another state. Any worker can continue a session.
  A final check re-runs the assembled proof and confirms it proves the
  original statements.
- **Documents.** Code open on Lean Beam answers goals and types at
  positions, and tries candidate tactics at a `sorry`, without elaborating
  it again.
- **Snapshots.** Accepted code is kept as a compiled module that later code
  imports instead of checking it again. Labels, replacement, and a kernel
  re-check of each module and its imports come with it.
- **Runs.** A leader hands targets in a spec snapshot to agents, whose
  snapshots fill them. pisa audits the run against its spec, accepts it
  with `lake comparator` and six kernels, and rebuilds and settles it as the
  spec changes.
- **Search.** Declarations of Mathlib, Physlib, CSLib and the snapshots, by
  natural language, LaTeX, Lean or name, with prebuilt indexes.
- **Environments.** A worker pool per Lean toolchain and set of libraries;
  each request names the version it runs in.
- **Scale.** Servers queue jobs in PostgreSQL, and workers claim them,
  preferring those whose imports they already have loaded. On Kubernetes,
  KEDA scales the workers on the queue.
- **Clients** for Rust, Python, TypeScript and React.

## Quickstart

You need Docker with Compose and privileged containers, on a host with
cgroup v2. `PROJECT=lean-core` builds workers with Lean alone, which is
quick. Leave it out for the default environment, which compiles Mathlib,
Physlib and CSLib: that build takes far longer, and needs much more memory
and disk ([Docker Compose](docs/deploy-compose.md)).

```sh
git clone https://github.com/aqora-io/pisa && cd pisa
PROJECT=lean-core docker compose up -d --build
until curl -fs localhost:8000/health >/dev/null; do sleep 2; done

curl -s localhost:8000/check -H 'content-type: application/json' \
  -d '{"code": "theorem t : 2 + 2 = 4 := by decide"}'
```

The answer says whether the code is accepted (`ok`), with Lean's
`messages`, the `sorries` left, and why it isn't (`reasons`).

From the clients:

```python
# pip install pisa-client
from pisa_client import Client

client = Client("http://localhost:8000")
assert client.check("theorem t : 2 + 2 = 4 := by decide")["ok"]
```

```ts
// npm install @aqora/pisa-client
import { PisaClient } from "@aqora/pisa-client";

const client = new PisaClient({ url: "http://localhost:8000" });
console.log((await client.check("theorem t : 2 + 2 = 4 := by decide")).ok);
```

```rust
// cargo add pisa-client tokio --features tokio/full
let client = pisa_client::Client::new("http://localhost:8000")?;
assert!(client.check("theorem t : 2 + 2 = 4 := by decide").await?.ok);
```

To run it on Kubernetes, install the chart from
`oci://ghcr.io/aqora-io/charts/pisa`
([Kubernetes](docs/deploy-kubernetes.md)).

## Concepts

| Term | Meaning |
| --- | --- |
| Check | One request to elaborate code and judge it |
| Document | Code open on Lean Beam, for goals and probes |
| Session, state | A document for tactic search; a state is a proof state in it |
| Snapshot, module | Accepted code kept as a module; a snapshot names it, with labels |
| Run, spec, head | A spec snapshot states targets, a run is the snapshots that fill them, and a head is the newest of a line of replacements |
| Version, environment, pool | An environment is a toolchain and its libraries, its version a label, and its pool the workers that serve it |
| Scope | Labels that keep tenants' snapshots and runs apart |
| Job | A request queued for a worker, which a client can follow |

[Concepts](docs/concepts.md) explains each.

## Documentation

- Using pisa:
  - [Concepts](docs/concepts.md)
  - [The HTTP API](docs/api.md)
  - [Checking, documents and sessions](docs/checking.md)
  - [Snapshots](docs/snapshots.md)
  - [Runs of agents](docs/runs.md)
  - [Accepting a run](docs/acceptance.md)
  - [Search](docs/search.md)
  - [Clients](docs/clients.md)
- Running pisa:
  - [Docker Compose](docs/deploy-compose.md)
  - [Kubernetes](docs/deploy-kubernetes.md)
  - [Environments](docs/environments.md)
  - [Configuration](docs/configuration.md)
  - [Operations](docs/operations.md)
  - [Building a search index](docs/search-index.md)
- Working on pisa:
  - [Development](docs/development.md)
  - [Contributing and releasing](CONTRIBUTING.md)
  - [Changelog](CHANGELOG.md)

The complete API schema is [`openapi.json`](openapi.json), which every
server also serves at `/openapi.json`.

## Status

pisa is at 1.0, and its versions follow
[semantic versioning](https://semver.org).

Not built yet:

- authentication;
- per-tenant fairness;
- writing a finished tactic script back into its `sorry`.

[Checking](docs/checking.md#limits) lists the limits.

## Security

pisa has no authentication: put it behind your own gateway. Workers run Lean
in privileged containers, for bubblewrap and cgroups. With a describe model
configured, snapshot search sends snapshot code to that model's provider.
[SECURITY.md](SECURITY.md) says how to report a vulnerability.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option. The search indexes reuse
LeanSearch v2's descriptions, under Apache-2.0: see
[search/NOTICE](search/NOTICE).
