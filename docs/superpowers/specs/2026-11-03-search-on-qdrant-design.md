# Replicated search on Qdrant

## Context

`pisa-search` now searches snapshot declarations. Each process keeps the bundle and a snapshot index in memory, follows the TiKV change log, and runs its own describer. Two replicas would each pay the LLM for every module, and each must rebuild about 2 GB of index on start. Search can't run as more than one instance.

**Goal (user's):** run several search replicas for **availability and query throughput**. Retrieval, fusion and state move to Qdrant, so replicas are stateless. TiKV coordinates which replica describes and embeds.

### Decisions made with the user
- **Everything in Qdrant.** All four lanes are Qdrant prefetches: dense, lexical, structural and name. The bundle's declarations and the snapshot declarations live in one collection, so one query ranks both.
- **Fusion in Qdrant:** weighted RRF, with the priors and the exact-name bonus as a formula query.
  - This is accepted knowing that Qdrant's weights rescale a hit's rank, scoring it `1/(k + (r+1)/w − 1)`; they don't multiply its score as ours do.
  - So `Weights` and `k` are **re-tuned** until MathlibQR R@10 and MRR are within 0.01 of today's.
  - Per-hit **`lanes` is dropped** from the API.
  - **Every declaration needs a vector.**
- **Qdrant only.** The in-memory engine goes: the tantivy index, brute-force dense search in the engine, and the in-memory snapshot index. `serve`, `eval` and `query` need Qdrant.
- **Describing and embedding: a leader among the replicas.** A TiKV lease elects one replica, the leader. Only it writes to Qdrant and calls the LLM and the embedder.
- **Entries are designed for many per module.** They live in their own Qdrant collection, and a label scope resolves to module ids there first.
- **Deployment:** a search Deployment in the Helm chart with Qdrant as a chart dependency (or an external URL), plus compose services.

### Facts this rests on (checked in Qdrant's docs and the code)
- **The Query API:**
  - prefetches, each with its own `filter`, `using`, `query` and `limit`, which can nest;
  - RRF with `k` (default 2; ≥ 1.16) and `weights` (≥ 1.17), with zero-based ranks;
  - formula queries (≥ 1.14) with `sum`, `mult`, `ln`, payload keys and conditions (1 or 0), plus `min`/`max` (**≥ 1.19.1**). A missing payload value defaults to 0, or to `defaults`;
  - `facet` returns the distinct values of a keyword field under a filter;
  - `query/batch` takes many queries per request.
- **Sparse vectors** take the `idf` modifier, which uses `ln((N − n + 0.5)/(n + 0.5) + 1)`, the same as tantivy, computed **per shard**. So the collection has one shard, replicated.
- **Qdrant returns only the fused score**, with no per-prefetch provenance; hence `lanes` goes.
- **Today's lanes** (`engine.rs`):
  - lexical is a boolean sum of field-boosted BM25 term queries: prose against name 1.0, informal 2.0, statement 1.5, signature 1.0 and doc 1.2; Lean text against signature 2.0 and name 1.5;
  - structural is IDF over the type's constants;
  - name is the exact name plus edit-distance suggestions;
  - dense is cosine, weighted by coverage.

  After fusion come +1.0 for an exact name, the PageRank prior `0.004·min(ln(1+pr), 4)`, +0.002 for def/theorem intent words, the sorry/generated/tooling filters, and folding of deprecated aliases and `to_additive` pairs.
- **The eval baseline** (`search/eval/v4.35.0-da683984.md`) has per-style rows only at dense weight 1, and totals at the default weight 2. A fresh baseline is needed.
- **Reused as is:** the catalog and its change log (`pisa-kv`), the workers' declaration export, `Described` records in TiKV, the describing logic (`informalize::{Prompts, complete, Context}`), and the protocol (minus `lanes`).

### Calls made by the designer
- **REST over `reqwest`** for Qdrant, not the `qdrant-client` crate. That avoids a second tonic/prost stack next to `tikv-client`.
- **Rendering data lives in the payload**, so `/v1/resolve` and `/v1/decl` read Qdrant too.
- **`load` is an operator step**, not a Helm Job.
- **The eval gate is on totals.** It is overall R@10 and MRR within 0.01 absolute of a fresh baseline from today's engine with default weights. Per-style rows are reported but not gated: q4 has 22 queries, so one query moves it 0.045. Tuning uses the same 922 queries, as the current defaults did.
  - If no configuration reaches the gate, work stops before the old engine is removed, and the numbers come back to you.
- **Library and kind filters move into the lanes**, so lanes return more candidates from the wanted libraries.
- **Vectors right away:** new snapshot declarations are embedded from their docstring view first, then again after their description lands, so "every declaration has a vector" holds within seconds.
- **`load` refuses a bundle** whose vectors cover less than 99% of its declarations (`--min-coverage`).
- **Reloading in place** for the same `env_id` is fine: during a reload, readers may see a mix of old and new base points. The snapshot points are re-encoded afterwards, since `rev` includes the manifest's `field_stats`.
- **Broad label scopes have a cap:** a label scope that resolves to more than 20,000 modules is a 400 asking for more labels or `"all"`.

## Design

### 1. Collections (`crates/pisa-search/src/qdrant.rs`, new: a thin async REST client)
- **`pisa-<env_id>`, the declarations:** one shard, `replication_factor` from config (1 in dev, 2 in production).
  - **Vectors:** `dense` (512 dimensions, cosine, scalar int8 quantization kept in RAM); sparse `lexical`, `consts` and `trigrams`, each with `modifier: idf`.
  - **Point ids:** UUIDs from `sha256("base\0" + name)` or `sha256("snap\0" + m + "\0" + name)`. A manifest point with the nil UUID carries `source: "manifest"` and the loaded bundle's manifest, including `field_stats`.
  - **Payload** (a `PointPayload` struct, convertible to `StoredDecl` for `render`):
    - every point: `source` (`base`/`snapshot`), `name`, `name_lower`, `last_lower`, `kind`, `library`, `module`, `signature`, `type_full`, `docstring?`, `informal?`, flags (`uses_sorry`, `informal_result`, `deprecated_to`, `additive_sibling`, `imports`), `source_url?`, `pagerank`, `hidden` (generated or tooling, via `corpus::{is_generated, is_tooling}`);
    - base points also: `deps` and `used_by` as names (at most 50 users, ranked as `Engine::new` ranks them);
    - snapshot points also: `module_id`, `dirty`, `verified`, `has_entries`, and `rev`, a hash of the module's decls and described records and the manifest's `field_stats`.
  - **Payload indexes:** keyword on `source`, `name`, `name_lower`, `last_lower`, `kind`, `library`, `module_id`; bool on `hidden`, `uses_sorry`, `has_entries`.
- **`pisa-<env_id>-entries`, the snapshot entries:**
  - one point per entry: UUID from the entry id, no vectors (or a placeholder vector config, if Qdrant requires one);
  - payload `{entry, module_id, created, labels: ["k=v", …]}`. Label keys can't contain `=`, so `k=v` splits unambiguously;
  - indexes: keyword on `labels` and `module_id`, integer on `created`.
- **Qdrant version:** pinned at ≥ 1.19.1.

### 2. Encoders (`crates/pisa-search/src/encode.rs`, new; used by load, the indexer and queries)
- **`lexical`:** one sparse vector whose tokens carry their field. Index = the first 4 bytes of `sha256(field ":" term)`, over the field texts and analyzers `index::document` uses today: `name_text`, `tokens`, `latex_to_unicode`, and `pisa_prose`/`pisa_words` from `index::register_tokenizers`.
  - The document value is BM25's tf part, `tf·(k1+1)/(tf + k1·(1−b+b·len/avglen))`, with k1 = 1.2 and b = 0.75. Average lengths per field come from the manifest's `field_stats`.
  - The query value of each (field, term) is that field's boost from `Weights`; prose and Lean terms add up.
- **`consts`:** the token is `"const:" + name` for each distinct `type_ref`, with value 1. The query takes identifier constants at 1.0 and `Symbols::constants` at their weights.
- **`trigrams`:** the lowercased name's trigrams (`pisa_ngram`), with value 1.

### 3. Bundles and loading
- **`build`** still writes `decls.ndjson.zst`, `vectors.bin` and `manifest.json` (plus `field_stats`), and no longer builds `tantivy/`. `Bundle::open` and `Dense` (read, `neighbours`) stay for offline tools: `load`, and informalize's discrimination test (`informalize.rs:866`).
- **`pisa-search load --bundle DIR --qdrant URL [--replication N] [--min-coverage 0.99]`:**
  - checks coverage;
  - creates both collections and their indexes if missing;
  - upserts base points in batches;
  - deletes base points whose ids aren't in the bundle, found by scrolling `source = base` ids;
  - writes the manifest point last.

### 4. Query path (stateless replicas; `engine.rs` rewritten, keeping `parse`, `classify`, `render`, `summary`, `flags` and `short_signature`)
A search request takes up to three Qdrant round trips, each batched across its queries, plus the query embedding as now.
1. **Scopes.** For each query with a label scope, `facet` the entries collection on `module_id` under `labels = "k=v"` for every label, giving the module ids. Over 20,000 modules is a 400.
2. **Fused search** (`query/batch`).
   - **Filter F:**
     - the scope:
       - `none`: `source = base`;
       - `all`: `source = base` or (`source = snapshot` and `has_entries`);
       - labels: `source = base` or `module_id` among the ids from step 1;
     - `library` among the requested ones, `kind` among the requested ones;
     - **visible V:** not `hidden`, and not `uses_sorry` unless `include_sorried`.
   - **Prefetches**, each with filter F ∧ V and a tunable limit (default 100):
     - lexical (sparse);
     - name (`trigrams`);
     - structural (`consts`);
     - dense (the query vector; left out, with a warning, if embedding failed);
     - exact: filter F ∧ `name = <name>` with no query (a filter-only prefetch; if Qdrant needs a query, `{"sample": "random"}`), without V, so an exact match shows whatever it is, including names under 3 characters.

     A lane with nothing to search is left out with its weight.
   - **Fusion:** a nested prefetch of the lanes, `rrf {k, weights}`. Then the outer formula query: `sum[$score, mult[w.pagerank, min[ln(sum[1, pagerank]), w.cap]], mult[w.intent, <kind condition>], mult[w.exact, {name = <name>}]]`, with limit `max(k·3, 30)` and the payload minus `deps`/`used_by`.
3. **Finishing.**
   - Deprecated aliases fold into their target, fetched by name when it isn't among the results. `to_additive` pairs fold into the better-scored one.
   - Sort, take `k`, render.
   - For snapshot hits, fetch each module's entries from the entries collection (in scope, newest first, at most 10), plus `dirty`/`verified` from the payload.
- **`Weights`:** the RRF `k`, the lane weights, the field boosts, the prior's weight and cap, the intent and exact bonuses, and the lane limit. All are tuned with `eval --weights`.
- **`/v1/resolve`:** in one batch, exact name, `name_lower`, `last_lower` (limit 200) and a trigram query (limit 100). Ranked by edit distance, then PageRank, as `Engine::suggest` does. Base only.
- **`/v1/decl`:** a filter on `name`, base only, with `deps`/`used_by` from the payload.
- **`/health`:** reports that Qdrant is reachable, the manifest, and whether this replica is the leader.
- **Protocol:** `Hit.lanes` is removed (`pisa-protocol/src/search.rs`), along with its uses: the Python stub, `query`'s printout, tests and the README. The README's Upgrading section notes it.

### 5. The leader (`crates/pisa-search/src/indexer.rs`, new; it takes over the follower and describer from `snapshots.rs`)
- **Lease:** `(pisa, v, "search", "leader")` → `{owner, until_micros}`.
  - Every replica tries to take or renew it every 10 s with a 30 s TTL, in one transaction: read, then write if it is absent, expired or ours.
  - Before each Qdrant write batch the leader checks that the lease has more than 10 s left, and losing the lease aborts the indexer tasks.
- **On becoming leader, it always reconciles:**
  - scan TiKV `entry` and `decls` as `Snapshots::load` does, with adaptive pages, and each module's `described`;
  - scroll the snapshot points' `(module_id, rev)` and the entry points' ids;
  - upsert the modules whose `rev` differs, and the missing entries;
  - delete what TiKV no longer has;
  - queue every module that lacks descriptions or vectors.

  It then tails the log from the cursor `(pisa, v, "search", "cursor")` minus the 2-minute `LOOKBACK` (from the reconcile's start if there is no cursor), writes the cursor after each poll, and reconciles again every 6 h.
- **Applying a change:**
  - an entry is upserted into the entries collection, or deleted. A module's `has_entries` is updated on its points when it goes between 0 and 1 or more entries. Entry counts come from the new `Catalog::entries_of(m)`, which scans `ref/m/*`;
  - a module is re-read from `decls/m` and `described/m/*`, then its points are upserted, or deleted by `module_id` if it is gone.

  Both are idempotent. New modules go on the describe queue.
- **Describer:** today's `Describer`, with its retries, pauses and per-level saves.
  - It first embeds each new declaration's docstring view, then describes, then embeds the new views.
  - Base dependencies for prompts are fetched from Qdrant by name.
  - Each save writes TiKV first, then upserts the affected points.

### 6. CLI and config
- **`serve`:** `--qdrant` / `PISA_SEARCH_QDRANT`, `--qdrant-key-env`, `--env-id`, plus the existing `--tikv-pd`, `--snapshot-version`, `--embedder`, `--describe-model` and `--describe-concurrency`. `--bundle` goes.
- **`eval` and `query`:** take `--qdrant` and `--env-id`. Eval's lane configurations become prefetch subsets, and the "(covered)" rows go, since coverage is complete.
- **Removed:** the engine's tantivy lanes and `Bundle` use; `Dense::search_many`/`embed_queries` in the engine; `snapshots::SnapshotIndex`, `Combined` and the in-memory scope code.

### 7. Deploy
- **`docker-compose.yml`:**
  - a `qdrant` service, pinned at ≥ 1.19.1, under the `search` profile;
  - `search` runs two replicas against it;
  - a `search-load` one-shot service (profile `search-load`) runs `pisa-search load --bundle /bundle`.
- **Helm (`deploy/helm/pisa`):** `templates/search.yaml` holds:
  - a Deployment (`search.replicas`, default 2; readiness on `/health`);
  - a Service;
  - a PodDisruptionBudget (`minAvailable: 1`);
  - an optional HPA on CPU.

  In `values.yaml`, the `search.*` values set:
  - the image and resources;
  - the embedder spec and describe model;
  - existing-Secret names for `SCW_SECRET_KEY`, `ANTHROPIC_API_KEY` and `OPENAI_API_KEY`;
  - `qdrant.url`, or the `qdrant` subchart (official chart from `https://qdrant.github.io/qdrant-helm`, pinned, `replicaCount: 3`) with `search.qdrant.replication: 2`.

  TiKV comes from `metadata.tikv.pdEndpoints`. `Chart.yaml` gains the dependency.
- **README:** the Search section covers Qdrant, loading, replicas, the leader, the ranking changes and `lanes` going. The Kubernetes section covers the search values and the load step.

### Non-goals
- Sharding a collection across Qdrant nodes: IDF is per shard.
- Qdrant's server-side BM25 or embedding inference.
- Several `env_id`s per server.
- Fencing tokens beyond the lease margin plus reconciles.
- Backfilling pre-snapshot-release modules.
- A Helm Job for `load`.
- Atomic bundle reloads with an alias swap.

## Ranking changes (gated on the eval)
- New RRF semantics and re-tuned weights.
- Name candidates rank by trigram score; `/v1/resolve` keeps edit distance.
- No coverage weighting.
- The prior applies to the fused top results.
- Filters run inside the lanes.
- Lexical hash collisions.
- No length normalization in the structural lane.
- Hit `score` values change scale.

## Implementation order
1. **Baseline:** with today's engine and default weights, run `eval` on the published bundle and save the per-style and total report as the gate's baseline.
2. `qdrant.rs` and `encode.rs`, with unit tests for the encoders and the query JSON. Confirm against a real Qdrant ≥ 1.19.1 that filter-only prefetches, `facet`, `min` and payload-only collections behave as assumed.
3. `load`, and `build` without tantivy.
4. The Qdrant engine, `resolve` and `decl`. Port `tests/search.rs`.
5. **Tune `Weights` and `k` with `eval --weights` until the gate passes.** If it can't, stop and report before going further.
6. Lease, cursor, indexer, entries collection and describer. Port `tests/snapshots.rs`.
7. Remove the in-memory engine and `Hit.lanes`; update `serve`, `eval`, `query`, the clients and the stubs.
8. Compose, Helm, README, the Python test fixture.

## Verification
- **Unit:** encoders (BM25 tf values, field tokens, trigrams), the query JSON per scope, filter and lane mix, folding, and lease transitions (in-memory KV).
- **Integration** (`PISA_TEST_QDRANT_URL`, skipped when unset, like `PISA_TEST_TIKV_PD`; a compose `qdrant` serves it):
  - `tests/search.rs` against a loaded test bundle;
  - the snapshot tests: scope, relabel, delete, restart with no LLM calls, a module with 1,000 entries;
  - two indexers on one KV: only the leader describes, so the mock counts each declaration once; killing it hands over within 30 s.
- **Eval:** the gate in step 6, its report committed under `search/eval/`.
- **End to end:** compose with `--profile search`, two replicas, `scripts/e2e.py --search` against each, then stop the leader and check the snapshot flow through the other.
- **Helm:** `helm lint` and `helm template`; on minikube, two search pods plus the Qdrant subchart, then delete a pod and confirm search keeps answering.
- `cargo test --workspace`, `cargo clippy --workspace`, the Python tests with `PISA_TEST_QDRANT_URL`.
