# Contributing

Thank you for helping with pisa. [Development](docs/development.md) says how
the repository is laid out, and how to build it and run its tests.

## Changes

- Open an issue first for anything larger than a fix, so we can agree on the
  shape before you write it.
- A change comes with its tests. A bug fix comes with a test that fails
  without it.
- CI must pass. Its checks are:
  - `cargo fmt --all --check`;
  - `cargo clippy --workspace --all-targets -- -D warnings`;
  - the workspace's tests, the real-Lean tests and the clients' tests;
  - the end-to-end script on a lean-core stack.
- A change to the API regenerates `openapi.json`
  (`PISA_UPDATE_OPENAPI=1 cargo test -p pisa-server --test openapi`) and
  `web/packages/client/src/openapi.ts` (`pnpm -C web generate`), and updates
  the clients and their tests.
- A change to the Python client's methods regenerates its async stubs:
  `PISA_UPDATE_STUBS=1 pytest tests/test_stubs.py` in `crates/pisa-python`.
- A change that users or operators see gets a line in
  [CHANGELOG.md](CHANGELOG.md), under a heading for the next release.
- Commit messages follow
  [Conventional Commits](https://www.conventionalcommits.org): `feat:`,
  `fix:`, `docs:`, `refactor:`, `build:`, with `!` for a breaking change.

By contributing, you agree that your contribution is licensed under the
project's terms: MIT OR Apache-2.0, at the user's option.

## Releasing

A release is a tag `v<version>` on `main`. The release workflow
(`.github/workflows/release.yml`) publishes everything but one image.

1. Set the version everywhere:
   - `version` in `Cargo.toml`'s `[workspace.package]`, and the version
     of `pisa-protocol` in its `[workspace.dependencies]`;
   - `web/packages/client/package.json` and
     `web/packages/react/package.json`;
   - `version` and `appVersion` in `deploy/helm/pisa/Chart.yaml`.

   `cargo test -p pisa-tests --test release` checks that they agree.
   Regenerate `openapi.json` and `openapi.ts`, which carry the version.
2. Give the release a section in `CHANGELOG.md`, `## [<version>] - <date>`.
   The GitHub release's notes are that section.
3. Merge to `main`, then tag and push: `git tag v<version> && git push
   origin v<version>`.

   The workflow checks that the tag matches the workspace's version. It then
   publishes:
   - the server's image, and docker/lean-core's worker image, to ghcr.io;
   - the chart, to `oci://ghcr.io/aqora-io/charts`;
   - the Python client to PyPI;
   - the TypeScript clients to npm;
   - `pisa-protocol` and `pisa-client` to crates.io;
   - a GitHub release.
4. Push docker/lean-project's worker image by hand. It compiles Mathlib,
   Physlib and CSLib, which takes too long and too much disk for CI:

   ```sh
   version=$(python3 scripts/env.py label docker/lean-project)
   image=ghcr.io/aqora-io/pisa-worker:<release>-$version
   docker build -f docker/Dockerfile --target worker \
     --build-arg REVISION=$(git rev-parse HEAD) -t "$image" .
   docker push "$image"
   ```

   The chart's `worker.version` is that version, so its default image is
   this one.

Search indexes are released apart, as `index-<version>` GitHub releases:
see [Building a search index](docs/search-index.md).

### Setting up the registries

Do this once, before the first release.

- **ghcr.io:** the workflow's own token pushes the images and the chart.
  After the first release, make the `pisa-server`, `pisa-worker` and
  `charts/pisa` packages public in the organization's package settings.
- **PyPI:** add the repository as a trusted publisher of `pisa-client`:
  workflow `release.yml`, environment `pypi`.
- **npm:** create the `npm` environment with a secret `NPM_TOKEN`, a
  granular token that may publish `@aqora/pisa-client` and
  `@aqora/pisa-react`.
- **crates.io:** create the `crates-io` environment with a secret
  `CARGO_REGISTRY_TOKEN`, a token that may publish `pisa-protocol` and
  `pisa-client`.
