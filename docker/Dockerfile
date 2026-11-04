# syntax=docker/dockerfile:1.7
# Targets: `router`, `worker`, `search`, and `corpus` (the search corpus of the
# pinned project, written out with --output). Build from the repository root:
#   docker build -f docker/Dockerfile --target worker .
#   docker build -f docker/Dockerfile --target corpus --output search/corpus .
# Set PROJECT=lean-core for a small image without Mathlib, for quick tests
# (not for `corpus`, which exports Mathlib and Physlib).

FROM rust:1.99-trixie AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release -p pisa-router -p pisa-worker \
    && cp target/release/pisa-router target/release/pisa-worker /usr/local/bin/

# Its own stage, so router and worker builds don't wait for tantivy.
FROM rust:1.99-trixie AS build-search
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release -p pisa-search \
    && cp target/release/pisa-search /usr/local/bin/

FROM debian:trixie-slim AS router
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /usr/local/bin/pisa-router /usr/local/bin/
EXPOSE 8000
ENTRYPOINT ["pisa-router"]

FROM debian:trixie-slim AS lean
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl git bubblewrap \
    && rm -rf /var/lib/apt/lists/*
ENV ELAN_HOME=/opt/elan PATH=/opt/elan/bin:$PATH
ARG LEAN_TOOLCHAIN=leanprover/lean4:v4.35.0
RUN curl -sSfL https://raw.githubusercontent.com/leanprover/elan/master/elan-init.sh \
    | sh -s -- -y --no-modify-path --default-toolchain ${LEAN_TOOLCHAIN}
# The REPL must be built with exactly the project's toolchain.
ARG REPL_REV=v4.35.0
RUN git clone --depth 1 --branch ${REPL_REV} https://github.com/leanprover-community/repl /opt/repl \
    && cd /opt/repl && lake build \
    && ln -s /opt/repl/.lake/build/bin/repl /usr/local/bin/repl
ARG PROJECT=lean-project
COPY docker/${PROJECT} /opt/project
WORKDIR /opt/project
# Dependencies follow the committed lake-manifest.json; without one, resolve them.
# Mathlib comes prebuilt from its cache; Physlib compiles from source (slow, once).
RUN if [ ! -f lake-manifest.json ]; then lake update; fi \
    && if grep -q mathlib lakefile.toml; then lake exe cache get; fi \
    && lake build
# The snapshot kernel re-verifier, run with `lean --run` (imports only Lean
# core). After `lake build`, so editing it doesn't recompile the project.
COPY docker/verifier /opt/verifier

FROM lean AS worker
COPY --from=build /usr/local/bin/pisa-worker /usr/local/bin/
ENV PISA_PROJECT=/opt/project PISA_REPL=/usr/local/bin/repl
ENTRYPOINT ["pisa-worker"]

FROM debian:trixie-slim AS search
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build-search /usr/local/bin/pisa-search /usr/local/bin/
EXPOSE 8100
ENTRYPOINT ["pisa-search"]
CMD ["serve"]

# Exports every declaration of the project (about 40 minutes) and ingests it.
# QuantumInfo can't be imported alongside Mathlib (both declare
# `Commute.log_left`), so it comes from a second environment and its
# declarations record the header that loads them.
FROM lean AS search-export
COPY search/exporter /opt/exporter
COPY --from=build-search /usr/local/bin/pisa-search /usr/local/bin/
RUN lake env lean --run /opt/exporter/PisaExport.lean --env mathlib-physlib Mathlib Physlib \
        > /tmp/decls.ndjson \
    && lake env lean --run /opt/exporter/PisaExport.lean --env physlib-quantuminfo Physlib QuantumInfo \
        > /tmp/quantuminfo.ndjson \
    && lake env lean --run /opt/exporter/ToAdditive.lean Mathlib Physlib > /tmp/additive.ndjson \
    && pisa-search ingest --export /tmp/decls.ndjson --export /tmp/quantuminfo.ndjson \
        --export /tmp/additive.ndjson --project /opt/project --out /corpus \
        --env-imports "physlib-quantuminfo=import Physlib; import QuantumInfo"

FROM scratch AS corpus
COPY --from=search-export /corpus /
