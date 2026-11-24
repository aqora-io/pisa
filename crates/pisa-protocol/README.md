# pisa-protocol

The types that [pisa](https://github.com/aqora-io/pisa)'s parts exchange. pisa
is a server that checks Lean 4 code.

- `api`: the HTTP API's requests and responses. Their OpenAPI schemas are
  derived from them, and the server serves the result at `/openapi.json`.
- `search`: declaration search's requests and responses.
- `wire`: the jobs that servers queue for workers, and what workers answer.
- `repl`: the Lean REPL's JSON protocol.
- `snapshot`, `labels`, `scope`, `version`: snapshot ids and module names,
  label filters, tenancy scopes, and environment versions.

Most users want [`pisa-client`](https://crates.io/crates/pisa-client), which
re-exports the API's types.

Licensed under either of MIT or Apache-2.0, at your option.
