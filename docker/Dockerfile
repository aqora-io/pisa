# syntax=docker/dockerfile:1.7
# Targets: `server`, `worker`, and `corpus` (the search corpus of the pinned
# project, written out with --output). Build from the repository root:
#   docker build -f docker/Dockerfile --target worker .
#   docker build -f docker/Dockerfile --target corpus --output search/corpus .
# Set PROJECT=lean-core for a small image without Mathlib, for quick tests
# (not for `corpus`, which exports Mathlib and Physlib).
# Add --build-arg REVISION=<commit> to name the commit: the binaries report it
# (`/health`), and the image carries it as its revision label.

FROM rust:1.99-trixie AS build
ARG REVISION=unknown
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    PISA_REVISION=$REVISION cargo build --release -p pisa-worker \
    && cp target/release/pisa-worker /usr/local/bin/

# Its own stage, so worker builds don't wait for search's dependencies.
FROM rust:1.99-trixie AS build-server
ARG REVISION=unknown
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    PISA_REVISION=$REVISION cargo build --release -p pisa-server -p pisa-search \
    && cp target/release/pisa-server target/release/pisa-search /usr/local/bin/

# The server, and `pisa-search` for loading bundles and other offline work.
FROM debian:trixie-slim AS server
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build-server /usr/local/bin/pisa-server /usr/local/bin/pisa-search /usr/local/bin/
EXPOSE 8000
ARG REVISION=unknown
LABEL org.opencontainers.image.revision=$REVISION
ENTRYPOINT ["pisa-server"]

# The Lake project the workers import from: docker/${PROJECT} by default, or
# another, with `--build-context project=<dir>`. Make that a wrapper project
# that requires the library, not the library's own checkout (README, *A pool
# for another project*).
FROM scratch AS project
ARG PROJECT=lean-project
COPY docker/${PROJECT} /

FROM debian:trixie-slim AS lean
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl git bubblewrap \
    && rm -rf /var/lib/apt/lists/*
ENV ELAN_HOME=/opt/elan PATH=/opt/elan/bin:$PATH
ARG LEAN_TOOLCHAIN=leanprover/lean4:v4.36.0-rc2
RUN curl -sSfL https://raw.githubusercontent.com/leanprover/elan/master/elan-init.sh \
    | sh -s -- -y --no-modify-path --default-toolchain ${LEAN_TOOLCHAIN}
# The REPL must be built with exactly the project's toolchain. The patch adds
# `infotreeAt`, which prints the info tree only at given positions.
ARG REPL_REV=v4.36.0-rc2
COPY docker/repl/infotree-at.patch /tmp/infotree-at.patch
RUN git clone --depth 1 --branch ${REPL_REV} https://github.com/leanprover-community/repl /opt/repl \
    && cd /opt/repl && git apply /tmp/infotree-at.patch && lake build \
    && ln -s /opt/repl/.lake/build/bin/repl /usr/local/bin/repl
COPY --from=project / /opt/project
WORKDIR /opt/project
# Dependencies follow the committed lake-manifest.json; without one, resolve them.
# Mathlib, required directly or through another package, comes prebuilt from
# its cache; Physlib compiles from source (slow, once).
RUN if [ ! -f lake-manifest.json ]; then lake update; fi \
    && if grep -q '"name": "mathlib"' lake-manifest.json; then lake exe cache get; fi \
    && lake build
# The library the axiom report runs from, and the `pisa` tool that
# re-verifies snapshot modules through the kernel (imports only Lean core), on
# the project's toolchain. After the project, so editing it doesn't rebuild
# the project.
COPY docker/pisa-lean /opt/pisa
RUN cp /opt/project/lean-toolchain /opt/pisa/lean-toolchain \
    && cd /opt/pisa && lake build

FROM lean AS worker
# Lean Beam's `lake setup-file` replays what a document imports on these
# packages, read-only. A package that enables Lake's artifact cache, as
# Physlib does, writes to its build files whenever Lake replays one; without
# the cache Lake uses the builds where they are. The first replay of a
# Mathlib module that the project's build doesn't reach writes a build file,
# so Mathlib is replayed here. lake comparator judges exports of the worker's
# own builds and builds nothing; its sandbox covers /run/user, which has to
# exist.
RUN if [ -d /opt/project/.lake/packages ]; then \
        find /opt/project/.lake/packages -mindepth 2 -maxdepth 2 -name lakefile.toml \
            -exec sed -i -e '/^enableArtifactCache/d' -e '/^restoreAllArtifacts/d' {} +; \
    fi \
    && if grep -q '"name": "mathlib"' lake-manifest.json; then lake build Mathlib; fi \
    && mkdir -p /run/user
COPY --from=build /usr/local/bin/pisa-worker /usr/local/bin/
ENV PISA_PROJECT=/opt/project PISA_REPL=/usr/local/bin/repl
# A sandbox the worker kills leaves bubblewrap's inner process to PID 1, and
# each one left unreaped counts against its slot's pids.max until Lean can't
# start a thread there. tini reaps them, and passes signals on to the worker.
RUN apt-get update \
    && apt-get install -y --no-install-recommends tini \
    && rm -rf /var/lib/apt/lists/*
ARG REVISION=unknown
LABEL org.opencontainers.image.revision=$REVISION
ENTRYPOINT ["tini", "--", "pisa-worker"]

# Exports every declaration of the project (about 30 minutes) and ingests it.
# Mathlib, Physlib, QuantumInfo and PhyslibAlpha import together, so they are
# exported from one environment; the declarations of the last two record the
# header that loads them besides the default imports.
FROM lean AS search-export
COPY search/exporter /opt/exporter
COPY --from=build-server /usr/local/bin/pisa-search /usr/local/bin/
RUN lake env lean --run /opt/exporter/PisaExport.lean --env all \
        Mathlib Physlib QuantumInfo PhyslibAlpha > /tmp/decls.ndjson \
    && lake env lean --run /opt/exporter/ToAdditive.lean Mathlib Physlib QuantumInfo PhyslibAlpha \
        > /tmp/additive.ndjson \
    && pisa-search ingest --export /tmp/decls.ndjson --export /tmp/additive.ndjson \
        --project /opt/project --out /corpus \
        --library-imports "QuantumInfo=Mathlib,Physlib,QuantumInfo" \
        --library-imports "PhyslibAlpha=Mathlib,Physlib,PhyslibAlpha"

FROM scratch AS corpus
COPY --from=search-export /corpus /
