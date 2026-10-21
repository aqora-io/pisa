# syntax=docker/dockerfile:1.7
# Targets: `router` and `worker`. Build from the repository root:
#   docker build -f docker/Dockerfile --target worker .
# Set PROJECT=lean-core for a small image without Mathlib, for quick tests.

FROM rust:1.99-trixie AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release -p pisa-router -p pisa-worker \
    && cp target/release/pisa-router target/release/pisa-worker /usr/local/bin/

FROM debian:trixie-slim AS router
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /usr/local/bin/pisa-router /usr/local/bin/
EXPOSE 8000 7070
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
# Mathlib comes prebuilt from its cache; Physlib compiles from source (slow, once).
RUN lake update \
    && if [ -d .lake/packages/mathlib ]; then lake exe cache get; fi \
    && lake build

FROM lean AS worker
COPY --from=build /usr/local/bin/pisa-worker /usr/local/bin/
ENV PISA_PROJECT=/opt/project PISA_REPL=/usr/local/bin/repl
ENTRYPOINT ["pisa-worker"]
