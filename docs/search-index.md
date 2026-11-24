# Search indexes

This page is for whoever serves, builds or publishes a search index: where
the prebuilt indexes are and how to serve one, how to build an index, how to
check its descriptions, and the reports that track its quality. How search
answers is in [Search](search.md).

An index is a bundle, a directory of three files: `manifest.json` (the
version as `env_id`, the bundle's format, its libraries and kinds, the lexical
fields' statistics and the embedding model), `decls.ndjson.zst` (what hits
show, with each type's constants, dependencies and PageRank) and
`vectors.bin`. `pisa-search build` makes one from a corpus and a store of
descriptions and vectors, and `pisa-search load` puts it into Qdrant.

## Getting pisa-search

- From a clone, `cargo build --release -p pisa-search` builds
  `target/release/pisa-search`.
- The server image has it next to `pisa-server` (the Dockerfile's `server`
  target, `ghcr.io/aqora-io/pisa-server:1.0.0`): run it with `--entrypoint
  pisa-search`, as Compose's `search-load` service does.

| Command | What it does |
| --- | --- |
| `ingest` | Turns the exporter's output into a corpus with content hashes |
| `carry` | Carries stored descriptions over to a rebuilt corpus, for declarations unchanged but for docstring whitespace |
| `import-lsv2` | Stores LeanSearch v2's Mathlib descriptions for matching declarations |
| `informalize` | Describes declarations with Claude or an OpenAI model, caching the descriptions |
| `embed` | Embeds declarations through an embedding server, caching the vectors |
| `build` | Builds a bundle from a corpus and the stored descriptions and vectors |
| `load` | Loads a bundle into Qdrant, as the declarations of its `env_id` |
| `query` | Runs one search against a loaded bundle and prints the hits |
| `eval` | Measures retrieval quality on MathlibQR and the evaluation sets |
| `eval-sets` | Writes the physics evaluation sets that need no model spend |
| `validate-descriptions` | Judges stored descriptions for left-out hypotheses, and replaces those that leave some out |

`pisa-search <command> --help` lists each one's flags. The commands that talk
to Qdrant (`load`, `query`, `eval`) take `--qdrant URL`
(`PISA_SEARCH_QDRANT`) and `--qdrant-key-env VAR`
(`PISA_SEARCH_QDRANT_KEY_ENV`), the environment variable that holds Qdrant's
API key, if it needs one.

## Prebuilt indexes

Each index is a GitHub release of `aqora-io/pisa` tagged `index-<version>`,
on the commit that built it; its notes are the bundle's manifest. The current
one is `index-v4.36.0-54500a67`, for `docker/lean-project` (Lean v4.36.0 with
Mathlib, Physlib and CSLib), the version the chart's workers serve by default.
Its assets are archives of the gitignored `search/` directories, each with
`NOTICE`:

| Asset | Directory | Size | What it holds |
| --- | --- | --- | --- |
| `bundle.tar.zst` | `search/bundle` | about 230 MB | The bundle, which `load` puts into Qdrant |
| `store.tar.zst` | `search/store` | about 1.7 GB | The descriptions and vectors. A rebuild that starts from it only describes and embeds new or changed declarations |
| `corpus.tar.zst` | `search/corpus` | about 100 MB | The exported declarations |

The releases are public, so a download needs no login. Swap `bundle` for
`store` or `corpus` to fetch the others;
[the releases page](https://github.com/aqora-io/pisa/releases) lists every
index.

```sh
mkdir -p search
curl -fL https://github.com/aqora-io/pisa/releases/download/index-v4.36.0-54500a67/bundle.tar.zst \
  | tar -I zstd -x -C search
```

`load` refuses a bundle of another format (`bundle format 4 is not 5; rebuild
it`). Build one again from its release's `store` and `corpus` with
`pisa-search build` (see [Building an index](#building-an-index)): it reads
only those, so it describes and embeds nothing, and takes a few minutes.

## Serving an index

A server serves the bundle loaded for its `PISA_SEARCH_VERSION`, from the
Qdrant at `PISA_SEARCH_QDRANT` (see [How it runs](search.md#how-it-runs)).
Load each bundle once, and again after each rebuild.

### With Docker Compose

The `search` profile adds Qdrant (on `127.0.0.1:6333`), and the `search-load`
profile adds a one-off service that loads the bundle in `PISA_SEARCH_BUNDLE`
(by default `./search/bundle`) into it:

```sh
docker compose --profile search-load run --rm search-load    # into Qdrant, once
PISA_SEARCH_QDRANT=http://qdrant:6333 SCW_SECRET_KEY=... \
  docker compose --profile search up --build                  # on :8000 (PISA_PORT), through Traefik
```

Or export `COMPOSE_PROFILES=search` and `PISA_SEARCH_QDRANT=http://qdrant:6333`
(in `.envrc`, say) for search from a plain `docker compose up`.

- The servers serve `PISA_SEARCH_VERSION`, by default `v4.36.0-54500a67`; set
  it to another bundle's `env_id`
  (`jq -r .env_id search/bundle/manifest.json`).
- Queries are embedded with Qwen3-Embedding-8B on Scaleway, with the key in
  `SCW_SECRET_KEY`; without a working key the dense lane fails, and each
  result says so in its `warnings`. `PISA_SEARCH_EMBEDDER` names another
  embedder, which must serve the model that embedded the bundle.
- [Docker Compose](deploy-compose.md#search) has the rest of the stack.

### On Kubernetes

With `search.enabled`, the chart's servers search over Qdrant: the Qdrant
subchart (`qdrant.enabled`, 3 nodes) or `search.qdrant.url`. They serve
`search.version`, by default `worker.version`. Load the bundle through a
port-forward, with two copies of each shard:

```sh
kubectl port-forward svc/<release>-qdrant 6333 &
pisa-search load --bundle search/bundle --qdrant http://localhost:6333 --replication 2
```

Until it has, `/search` answers 503 and `/health` says why. The values are in
[Kubernetes](deploy-kubernetes.md#search).

### Loading

- `load` creates the collections `pisa-<env_id>` and `pisa-<env_id>-entries`
  if they don't exist, writes a point per declaration, removes the bundle
  points that the bundle no longer has, and writes the manifest last, which is
  when servers start answering from it. It waits up to a minute for Qdrant to
  start.
- It refuses a bundle whose vectors cover less than 99% of its declarations
  (`--min-coverage`).
- `--replication N` keeps N copies of each collection's shard (default 1).
- Loading the same `env_id` again replaces its declarations in place, keeping
  the snapshots'; queries meanwhile may see some of each. Restart the servers
  after it: they read the manifest (libraries, kinds, field statistics) when
  they open the index.

To check a loaded index from the command line, `query` prints the top hits
(`--k`, default 5) and how long the search took; without `--embedder` it
searches without the dense lane:

```sh
pisa-search query --qdrant http://localhost:6333 --version v4.36.0-54500a67 \
  --embedder "openai+https://api.scaleway.ai/v1?model=qwen3-embedding-8b&key_env=SCW_SECRET_KEY" \
  "trace of a density matrix is one"
```

### Settings

A server's search settings, each a flag of `pisa-server` and an environment
variable ([Configuration](configuration.md) lists every setting):

| Variable | Flag | Default | Meaning |
| --- | --- | --- | --- |
| `PISA_SEARCH_QDRANT` | `--qdrant` | none | Qdrant's REST URL; without it the server doesn't search |
| `PISA_SEARCH_VERSION` | `--version` | none; required with `--qdrant` | The version whose loaded bundle to serve, the bundle's `env_id` |
| `PISA_SEARCH_QDRANT_KEY_ENV` | `--qdrant-key-env` | none | The environment variable that holds Qdrant's API key |
| `PISA_SEARCH_EMBEDDER` | `--embedder` | none: no dense lane | Embeds queries and snapshot declarations: a TEI URL, or `openai+<base url>?model=<model>[&key_env=<VAR>]` |
| `PISA_SEARCH_SYMBOLS` | `--symbols` | the built-in `search/symbols.toml` | The symbol dictionary |
| `PISA_SEARCH_SNAPSHOTS` | `--snapshots` | `true` | Whether to search the workers' snapshots |
| `PISA_SEARCH_SNAPSHOT_VERSION` | `--snapshot-version` | `--version` | The worker version whose snapshots to search |
| `PISA_SEARCH_DESCRIBE_MODEL` | `--describe-model` | none | The Claude or OpenAI model that describes snapshot declarations (see [what it sends](search.md#what-snapshot-search-sends-to-a-model)) |
| `PISA_SEARCH_DESCRIBE_CONCURRENCY` | `--describe-concurrency` | 8 | Model and embedding requests in flight for snapshots |

## Building an index

A build starts from the previous index's `store` and `corpus`, downloaded
from its release into `search/`, so that only new or changed declarations are
described and embedded; a first build skips the `mv` and `carry` steps. It
needs Docker, `ANTHROPIC_API_KEY` (or `OPENAI_API_KEY` with an OpenAI
`--model`) and `SCW_SECRET_KEY`.

```sh
mkdir -p search/work
# After a version bump, keep the corpus the store's descriptions were made for.
mv search/corpus search/work/corpus-previous
# Export the pinned project's declarations.
docker build -f docker/Dockerfile --target corpus --output search/corpus .
# Keep descriptions of declarations unchanged but for docstring whitespace,
# which Lean releases reformat.
pisa-search carry --from search/work/corpus-previous --corpus search/corpus --informal search/store
# Reuse LeanSearch v2's Mathlib descriptions (downloads 332 MB, verified by sha256).
pisa-search import-lsv2 --corpus search/corpus --file search/lsv2.jsonl --informal search/store
# Describe everything else with Claude (needs ANTHROPIC_API_KEY), and embed every
# declaration as its description lands (needs SCW_SECRET_KEY). Try --sample 500 first.
LIBS=Mathlib,Physlib,PhyslibAlpha,Batteries,Init,Std,QuantumInfo,Cslib
EMBEDDER="openai+https://api.scaleway.ai/v1?model=qwen3-embedding-8b&key_env=SCW_SECRET_KEY"
rm -f search/work/described
pisa-search embed --corpus search/corpus --informal search/store --libraries $LIBS \
  --embedder "$EMBEDDER" --label qwen3-embedding-8b@scaleway --batch 32 --concurrency 32 \
  --follow search/work/described &
pisa-search informalize --corpus search/corpus --informal search/store --libraries $LIBS \
  --concurrency 40 && touch search/work/described; wait
# Build the bundle, load it into Qdrant and serve it on :8000.
pisa-search build --corpus search/corpus --informal search/store \
  --embedding-model qwen3-embedding-8b@scaleway --out search/bundle
docker compose --profile search-load run --rm search-load
PISA_SEARCH_QDRANT=http://qdrant:6333 PISA_SEARCH_VERSION=$(jq -r .env_id search/bundle/manifest.json) \
  docker compose --profile search up --build
```

- **The corpus.** The `corpus` target runs `search/exporter/PisaExport.lean`
  and `ToAdditive.lean` over Mathlib, Physlib, QuantumInfo, PhyslibAlpha and
  Cslib, then `pisa-search ingest` (the Dockerfile's `search-export` stage).
  The export takes about 45 minutes. The target builds on the same `lean`
  stage as the worker image, so it compiles Physlib and CSLib first unless
  Docker has that stage cached. It needs the full project, not
  `PROJECT=lean-core`.
- **ingest** takes the version (`env_id`) and source links from the Lake
  project (`--project`, by default `docker/lean-project`). `--library-imports
  LIB=MOD,…` records the header that loads a library's declarations when the
  default imports don't, and `--previous DIR` reports what changed against an
  earlier corpus.
- **The store.** `--informal` takes a directory or an `s3://bucket/prefix`
  URL (credentials come from `AWS_*`). It holds descriptions and vectors,
  keyed by content hash; each run writes a batch, and later batches win.
- **informalize** uses `--model` (by default `claude-haiku-5-5`; a name
  starting with `claude-` goes to Anthropic, any other to OpenAI). `--sample
  N` describes a pilot sample of about N declarations, whole modules
  stratified by area; `--names FILE` describes exactly those named;
  `--dry-run` prints the plan and the first prompts; `--report FILE` writes
  the cost, the checks and samples to review. It prints what it described,
  the cost and the judge calls. A changed glossary doesn't change the content
  hash: `informalize --force --libraries …` describes those libraries again.
- **embed** caches vectors under `--label` instead of the model's name, and
  `build --embedding-model` names the same label. With `--follow FILE` it runs
  alongside `informalize`, embedding declarations as their descriptions land,
  until the file exists; then it embeds everything left and stops.
- **Cost.** For the current index, Claude Haiku 5.5 described 30,896 of the
  30,903 declarations that `carry` and LeanSearch v2 left without a
  description, for $20.14; 433,006 of its 450,222 declarations have one
  (`search/eval/v4.36.0-54500a67-qdrant.md`).

## Publishing an index

From the pushed commit that built it, with the GitHub CLI logged in with
write access to the repository. Every archive carries `search/NOTICE`, which
the LeanSearch v2 descriptions require.

```sh
ENV_ID=$(jq -r .env_id search/bundle/manifest.json)
mkdir -p search/release
for d in bundle store corpus; do
  tar -I 'zstd -T0' -cf search/release/$d.tar.zst -C search $d NOTICE
done
# A new env_id gets a new release.
gh release create index-$ENV_ID -R aqora-io/pisa --target "$(git rev-parse main)" \
  --title "Search index $ENV_ID" --notes "$(jq -c . search/bundle/manifest.json)" \
  search/release/*.tar.zst
# A rebuild for an env_id that already has a release replaces the assets it
# rebuilt (pack only those); then edit the notes' manifest and which commit
# built what with `gh release edit index-$ENV_ID --notes-file …`.
gh release upload index-$ENV_ID -R aqora-io/pisa --clobber search/release/*.tar.zst
```

## Validating descriptions

LeanSearch v2's descriptions, and any stored description that wasn't judged
when it was written, can leave hypotheses out. `pisa-search
validate-descriptions` judges stored descriptions as `informalize` judges new
ones, describes again those that leave hypotheses out, and writes the
replacements the judge passes as a new batch, which wins over the old
descriptions; rebuild and load the bundle after it. It prints a line per
description judged, and the judge calls and cost.

- `--source` (e.g. `lsv2-v4.28.0-rc1`), `--names` (names, or files of one
  name per line, separated by commas) and `--flagged` (descriptions readers
  [flagged](search.md#post-declarationsnameflags), from the database at
  `--database-url`, by default `PISA_DATABASE_URL`) narrow what it judges.
- `--dry-run` lists what it would judge. Anyone who can reach the search
  service can flag, so review a `--flagged` run's targets this way before
  paying for it.
- `--model` is the model that judges and describes, by default
  `PISA_SEARCH_DESCRIBE_MODEL`, else `claude-haiku-5-5`.

```sh
pisa-search validate-descriptions --corpus search/corpus --informal search/store \
  --source lsv2-v4.28.0-rc1 --names Real.logb_le_logb_of_le,Real.logb_lt_logb
pisa-search validate-descriptions --corpus search/corpus --informal search/store \
  --flagged --database-url "$PISA_DATABASE_URL"
```

## Quality reports

`pisa-search eval` measures recall, MRR and nDCG of a loaded index, per lane
and per query style, on LeanSearch v2's MathlibQR benchmark (`--data`,
downloaded from its pinned commit if the file is absent) and on evaluation
sets (`--set FILE`, repeatable). `--split dev|test` takes one half of each
set, `dev` to tune on and `test` to report on; several `--weights` (JSON, the
fields of `Weights` in `crates/pisa-search/src/engine.rs`) are compared in one
run, embedding the queries once. R@k is the share of a query's answers in its
first k hits. Without `--embedder` there is no dense lane.

```sh
pisa-search eval --qdrant http://localhost:6333 --version v4.36.0-54500a67 \
  --embedder "openai+https://api.scaleway.ai/v1?model=qwen3-embedding-8b&key_env=SCW_SECRET_KEY" \
  --data search/work/MathlibQR.json \
  --set search/eval/sets/physics-papers.json --split test --out search/work/eval.md
```

The sets in `search/eval/sets/`, each split into a `dev` and a `test` half:
- `physics-synthetic`: 300 declarations from each of Physlib, QuantumInfo and
  PhyslibAlpha, asked with the searches the describer wrote for them, in words
  and as a formula. The same call wrote the indexed description, so these
  score high and favour the lexical lane.
- `physics-premises`: 300 theorems from each, their statement sent as a
  `goal`, with the theorems their proof uses as the answers (core logic and
  instances left out, and the theorem itself left out of the hits).
- `physics-papers`: statements as papers state them, from the 50 statements
  of 15 alphaXiv papers, the Palomar registry's physics entries, and
  statements written for areas the libraries cover. Claude chose the answers
  from the corpus without pisa's search, and no physicist reviewed them; a
  statement with no counterpart in the libraries is listed, not scored.
- `mathlib-findings`: searches that failed in a formalization campaign.

`pisa-search eval-sets --corpus DIR --informal STORE --out search/eval/sets`
writes the first two, the same for the same `--seed`.

The reports:
- `search/eval/v4.36.0-54500a67-qdrant.md`: the current index, on MathlibQR
  and every set, lane by lane, compared with the `v4.36.0-rc2-1e541805` index,
  and the counts and cost of its descriptions.
- `search/eval/v4.36.0-rc2-1e541805-qdrant.md` and
  `v4.36.0-rc2-1e541805-physics.md`: the `v4.36.0-rc2-1e541805` index (Lean
  v4.36.0-rc2), on MathlibQR and on the physics sets. The physics report
  records how the default `Weights` were tuned on the dev splits, and why the
  conclusion lane is off.
- `search/bench/2026-10-22/report.md`: the embedding and informalization
  benchmark behind the choice of Qwen3-Embedding-8B and Claude Haiku 5.5.
- `search/pilot/2026-10-22/report.md`: the informalization pilot, with cost,
  check pass rates and samples to review.
