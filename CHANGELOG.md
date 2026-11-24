# Changelog

pisa follows [semantic versioning](https://semver.org). Each release lists
what changed for those who run pisa and for those who call it.

## [1.0.0]

The first public release.

- **Starting fresh.** Databases and stores made by builds before 1.0 aren't
  supported. Start 1.0 with an empty database and store: the schema is one
  migration, and no compatibility code remains.
- **Server and workers.**
  - Checks, batches, proof sessions, documents on Lean Beam, snapshots,
    runs, audits, acceptance with `lake comparator`, rebuilds, settles,
    amendments and refutations.
  - Jobs, with async requests on every long endpoint. Cancelling one
    withdraws it if it is queued, and stops a running check, session
    check, snapshot verify or acceptance.
  - Scopes.
  - Environments with a pool each.
  - Declaration search over Qdrant, snapshots included.
- **Packages.**
  - Crates `pisa-protocol` and `pisa-client`.
  - PyPI `pisa-client`.
  - npm `@aqora/pisa-client` and `@aqora/pisa-react`.
  - The Helm chart, at `oci://ghcr.io/aqora-io/charts/pisa`.
  - The images `ghcr.io/aqora-io/pisa-server` and
    `ghcr.io/aqora-io/pisa-worker`. A worker image is tagged
    `<release>-<environment version>`: `1.0.0-v4.36.0-ca45ccf8` has Lean
    v4.36.0 alone, and `1.0.0-v4.36.0-54500a67` has Mathlib, Physlib and
    CSLib too.
- **Changes from the builds before 1.0**, for those who ran one:
  - `PISA_SEARCH_TEI` is now `PISA_SEARCH_EMBEDDER`.
  - `PISA_SEARCH_NO_SNAPSHOTS` is replaced by `PISA_SEARCH_SNAPSHOTS`.
  - `PISA_SEARCH_ENV_ID` and `--env-id` are now `PISA_SEARCH_VERSION` and
    `--version`.
  - The chart's `search.envId` is now `search.version`.
  - `GET /snapshots/accept` takes `?against=`.
  - `POST /snapshots/{id}/verify` takes its `timeout` and `again` as a
    JSON body.
  - `DELETE /sessions/{id}` is new.
  - An acceptance's pairs no longer have `challenge_module` and
    `solution_module`, and the pair error kind `build_failed` is gone.
  - A worker's lease defaults to 30 s.
  - The chart's images come from ghcr.io.
