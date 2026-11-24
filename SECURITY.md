# Security

## Reporting a vulnerability

Report it privately, through the repository's Security tab ("Report a
vulnerability"), not in a public issue. We aim to answer within a week, and
to agree with you on when to disclose it.

Fixes go into the latest 1.x release.

## What pisa does and doesn't protect

pisa runs code that its callers send, so its workers are built to contain it:

- Each Lean process runs in a bubblewrap sandbox, with no network and its
  own cgroup limits on memory and processes. `PISA_SANDBOX` turns the
  sandbox on (`bwrap`); Compose and the chart set it, and the binary's
  default is `none`.
- A verification policy rejects metaprogramming and unsafe options before
  the code runs.
- A proof is judged by the axioms it depends on.

What it leaves to you:

- **Authentication.** pisa has none. Anyone who reaches a server can check
  code, create and delete snapshots, and read every scope's records by
  naming its scope. Put the servers behind your own gateway.
- **Privileged workers.** Bubblewrap and cgroups need privileged containers,
  as Compose and the chart run them. Run workers on nodes kept for them.
- **Model providers.** With a describe model configured, snapshot search
  sends the code of every snapshot to that model's provider:
  [Search](docs/search.md#what-snapshot-search-sends-to-a-model).
