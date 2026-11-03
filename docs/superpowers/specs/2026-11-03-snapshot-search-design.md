# Snapshot declarations in search

## Context

`pisa-search` finds Mathlib/Physlib declarations through four lanes (lexical BM25, name, structural, dense) fused with RRF, over one immutable bundle loaded at startup. Each declaration has an LLM-written description (`informalize`) and an embedding (`embed`). Snapshots, the library code that clients build chunk by chunk with `POST /snapshots`, are invisible to it. An agent building a library can't ask "do I already have a lemma that says X?" about its own snapshots.

**Goal (user's):** describe and embed the declarations inside snapshots the way the base libraries are, and let `/v1/search` cover snapshots: those whose entries carry given labels, all of them, or none. Base libraries are searched as well. Describing works with Claude or OpenAI, and embedding with Scaleway or TEI.

### Decisions made with the user
- **Async.** `POST /snapshots` doesn't wait for search. A new snapshot becomes searchable within seconds to a minute. Name, signature and docstring are searchable first; the description and vector land after the LLM and embedder return. LLM and embedder keys live only on the search service.
- **Search reads TiKV directly.** Workers append a change-log key in the same transaction as every snapshot mutation, and search tails it. Snapshot search therefore needs `PISA_METADATA=tikv`, because the in-memory KV can't be shared between processes.
- **Scale:** up to about 1M snapshot declarations per deployment. The snapshot index stays in memory and is rebuilt on startup from TiKV, with no LLM or embedder calls on a restart.
- **Label scope = matching modules only.** A label scope covers the modules of entries that carry every given label (AND), not the snapshots those import.
- **Every module is described, with no cap.** Starting the search service with a describe model (Claude or OpenAI) is the operator choosing to send every snapshot's code to that provider. Without one, nothing goes to an LLM: declarations are indexed and embedded from their docstring view.

### Calls made by the designer
- **Snapshot hits in filters:** a snapshot hit's `library` is `PisaSnap`, so `libraries: ["PisaSnap"]` means "snapshots only". There's no separate `base: false` flag.
- **Where search state lives:** descriptions and vectors for snapshots live in TiKV (`described/m/<name>`, written by search), not in the informal store. That way one prefix scan restores them on startup, and they are deleted along with the module.
- **What hits carry:** `Hit.snapshot` lists up to 10 in-scope entries, plus `dirty` and `verified`. Dirty hits are shown, not filtered out.
- **Ranking across both indexes:** results from snapshots and from the bundle are merged per lane by score, with BM25 statistics computed over both indexes, before RRF. They are not separate lanes. With scope `none`, today's code path runs unchanged.
- **Retries:** a failed description is retried 3 times with backoff. An auth error pauses the queue instead of stopping the server.
- **No backfill:** modules built before this change aren't made searchable.

### Facts this rests on (from the code)
- The bundle is immutable. Ids are positions in the sorted corpus, and PageRank and the name maps are global (`index.rs:159`, `engine.rs:183`). So snapshots get a separate index, searched next to the bundle.
- Snapshots record almost nothing per declaration. `ModuleMeta.result.declarations` has name, kind, axioms and hashes, but no signature, refs or docstring (`verify.rs:17`). Only workers run Lean, so a worker must extract the rest at build time.
- `Llm::for_model` (`llm.rs`) already covers Claude and OpenAI. `Embedder::parse` (`dense.rs:106`) covers TEI and OpenAI-compatible APIs (Scaleway).
- Nothing notifies other services of snapshot changes today, and relabels and deletes leave nothing a poller could see (`catalog.rs`).

## Design

### 1. Shared KV crate: `crates/pisa-kv` (new)

Move `crates/pisa-worker/src/kv.rs` and `catalog.rs` into it. `catalog.rs` depends only on `kv` and `pisa-protocol`. `pisa-worker` re-exports them (`pub use pisa_kv::{kv, catalog};`), so `pisa_worker::kv::…` paths in `pisa-tests` keep working. `pisa-search` depends on `pisa-kv`. Add `pisa-kv` to `[workspace.dependencies]`. `Catalog::with_max_scan` is `#[cfg(test)]` (`catalog.rs:200`) but pisa-worker's own test uses it (`snapshots.rs:1289`): gate it behind a `test-util` feature that pisa-worker's dev-dependency turns on.

### 2. Worker: export, change log, cleanup

**New KV keys** (per version `v`; `T` = microseconds since the epoch as 16 hex digits):

| Key | Value | Written by |
|---|---|---|
| `(pisa, v, "decls", m)` | `SnapshotDecls` JSON | worker, in `Catalog::commit`'s transaction |
| `(pisa, v, "described", m, name)` | `Described` JSON (a description and a vector) | search |
| `(pisa, v, "log", T, <uuid>)` | `{"entry"?: e, "module"?: m}` | worker, in the same transaction as each mutation |

**Export at build** (`snapshots.rs::build`):
- In the REPL warmed by `warm_up_with(header)` (`snapshots.rs:697-706`), run a `run_cmd`, `module_export(module)` in `policy.rs` next to `module_report`. It runs in **both** paths: when the declarations come from `report_module`, and when they come from the kernel re-check (macros, `verify: true`). `report_module` only runs in the first (`snapshots.rs:684-716`).
- It iterates `env.header.moduleData[idx].constNames` and skips internal and private names and Lean's blacklisted names.
- For each declaration it prints one JSON record with what `search/exporter/PisaExport.lean::exportDecl` produces: `name, kind` (the exporter's `kindOf`: structure/class/instance/…), `signature` (`ppSignature`), `type_pp_full`, `type_refs`, `value_refs` (non-internal), `docstring` and `deprecated_to`.
  - Built as `attrs` and `range` aren't: neither is indexed, and the simp extension isn't reachable from a module-system header.
  - A module-system header doesn't load imported docstrings, so module-system snapshots export none. Checked against Lean v4.35.0.
- `uses_sorry` comes from the module's `result.declarations` axioms (`sorryAx`). The record also carries the module's `dirty` and `verified`, so search needn't read `mod/m`.
- **Builder's call:** a trimmed copy of the exporter logic as a Rust string constant, the way `AXIOM_REPORT` is done, or sharing the Lean text with `PisaExport.lean`.
- `SnapshotDecls { module: "PisaSnap.s…", header, decls: Vec<SnapshotDecl> }` goes in `pisa-kv::catalog`. `header` is the header `POST /snapshots` returned, which imports this module.
- A failed or timed-out export is logged and stores no `decls` record. It never fails the create. Such a module isn't searchable.
- `record_recheck` (verify) refreshes `uses_sorry`, `dirty` and `verified` in `decls/m` in the same transaction as `update_module`, and logs.
- Cost: one extra REPL command on a slot that is already warm, a fraction of a second on small chunks.

**Change log:** `Catalog::commit`, `add_entry`, `set_labels`, `remove_entry`, `cleanup` and `update_module` each put one `log` key naming the entry and/or module they touched. The value carries ids only. Readers re-read current state, so applying the log is idempotent and order-free.

**Cleanup:** `Catalog::clean` deletes `decls/m` and every `described/m/…` key with `mod/m`.

**Pruning:** every worker deletes `log` keys older than 24 h (constant `LOG_RETENTION`) every 10 min, in batches like `store.rs::sweep`, on either backend, so the in-memory one doesn't grow either. Not the state sweep: that only runs when `PISA_STATE_TTL_SECS > 0` (`lib.rs:117`, default 0).

### 3. Search: mirror and tailer (`crates/pisa-search/src/snapshots.rs`, new)

- **State:** entries `e → (module, labels, created)`, a label index `(k, v) → {e}`, and modules `m → (SnapshotDecls, Described?)`, kept in memory.
- **Startup:**
  1. Note `T0 = now − LOOKBACK` (`LOOKBACK` = 2 min, to cover transactions that commit after their `T` and clock skew between workers).
  2. Full-scan `entry/*` and `decls/*` in pages, reading each module's `described/*` with it. A page whose read fails is read again at half the size, down to one record, because records large together can exceed a TiKV response. Modules are queued oldest first, by their first entry.
  3. Tail the log from `T0`. Until the first full load finishes, results that asked for snapshots carry a warning ("snapshot index loading").
- **Tailing:** every 2 s, scan `log/[last − LOOKBACK, ∞)`. For each touched id, re-read `entry/e`, `decls/m` and `mod/m` existence, then update the mirror and the index. A module with no entry left, or with no `mod/m` record, is dropped. If the tailer falls more than `LOG_RETENTION / 2` behind (TiKV unreachable), it reloads everything.
- Every module goes on the describe queue at load, and each new one as it arrives; one with nothing to do is done at once.

### 4. Search: describing and embedding

- **Reuse from `informalize.rs`:** `needs_description` (L151), `levels` (L165), `system()` (L89) and the output schema (L101), `Context::prompt` (L456) and `Context::check` (L499), and `repair_escapes`. Factor out a "describe one declaration given a context" function that both the batch `run` and the server use. Don't fork the prompt.
- **Context for dependencies:** base declarations use the bundle's `StoredDecl.informal`; snapshot declarations use the mirror. Modules are processed oldest first, and within a module by `levels`. A dependency not yet described is shown by signature, as `shown_deps` already does.
- **LLM:** `--describe-model` / `PISA_SEARCH_DESCRIBE_MODEL` (e.g. `claude-haiku-5-5` or an OpenAI model) goes through `Llm::for_model`, with keys from `ANTHROPIC_API_KEY` / `OPENAI_API_KEY`. `--describe-concurrency` defaults to 8. Unset means no descriptions: snapshot declarations are embedded from their docstring view.
- **Queue:** with a describe model set, every new module joins the describe queue, oldest first. There is no budget cap. A module removed while queued is dropped from the queue.
- **Embedding:** the server's existing `--embedder` (`PISA_SEARCH_TEI`), so vectors share the bundle's space. Use `dense::view` for the document text and the bulk HTTP client with retries, not `query_http`. A declaration is embedded when its module's describing pass ends, or at once without an LLM. A description that lands later changes the view, and the declaration is embedded again. An embedding failure pauses embedding for a minute. **Constraint (docs):** the embedder must be the model that built the bundle's vectors. Snapshot vectors are recorded under the bundle manifest's `embedding_model` label (e.g. `qwen3-embedding-8b@scaleway`), not `Embedder::model()`. Without bundle vectors the server has no embedder (`main.rs:246`), and then snapshot declarations get none either.
- **Persisting:** after each dependency level, so a failure or a restart loses at most one level's work. One `described/m/<name>` key per declaration = `{informal?, attempts?, error?, view?, vector?}`. `vector` is int8 in hex, and `view` the hex `text_hash` (embedding model + document view) it embeds. Search writes them in a transaction that first locks `mod/m`, and skips the write if the module is gone, so nothing is orphaned. Restarts read them back and pay for nothing. Per-declaration keys keep every record small, so nothing needs splitting.
- **Retries (my call):** a declaration whose description failed (the call failed after `llm::send`'s retries, a refusal, `max_tokens`, invalid JSON or empty output; `informalize.rs:548-563`) is retried with backoff up to 3 attempts, then left at its docstring view. An auth error (401/403) pauses the whole queue and logs; the queue retries every 5 min instead of exiting as the batch `run` does (`informalize.rs:686`). Declarations with no description are described once a model is configured. Changing the model doesn't describe anything again.
- **Re-embedding:** a vector whose `view` isn't the current view's hash (a new description, or a bundle with another embedding model) is ignored and redone from the stored description, with no LLM calls.
- **Checks:** the LaTeX and cited-name checks go into `meta`, as in `informalize`. They don't block anything.
- **Two replicas** would each describe new modules, and the last write wins. Coordination is out of scope.

### 5. Search: snapshot index and query integration

**`SnapshotIndex`** (in `snapshots.rs`), behind an `RwLock`, owned by `Engine` as `Option<Arc<…>>`:
- A tantivy index in RAM, built with `index::schema()` plus a fast `module` u64 field (module slot). It is created fresh on startup and rebuilt from the mirror. The bundle's 420k declarations take 101 MB of tantivy, so 1M takes about 250 MB.
- The same tokenizers (`index.rs:122`).
- Documents are added and deleted per module (delete-by-term on `module`). A commit plus reader reload happens at most every second.
- `StoredDecl`s in a slab of slots. A snapshot declaration's id is `base.len() + slot`, so the whole engine keeps using `u32` ids, and `Engine::decl(id)` dispatches.
  - `library = "PisaSnap"`, `module = "PisaSnap.s…"`, `imports = Some(header)`, `pagerank = 1.0` (the bundle's mean), `deps` empty.
- Name maps (`by_name → Vec`, `by_lower`, `by_last`) over slots, and an int8 vector slab with the same quantization as `Dense`. The vectors live in the slots only, not in the records kept per module.
- Slots and module numbers freed by a removal are reused after the next commit, when no search can see their old documents.
- A request read-locks the index once, before its queries go to rayon: a rayon task that locked it itself could run another query's task while waiting on a nested `par_iter`, lock again and wait behind a writer forever.

**Queries:** when `snapshots` is `none`, today's code path runs unchanged. Otherwise each lane runs on the bundle and on the snapshot index, restricted to in-scope module slots, and the two lists are merged by score before RRF:
- **Lexical and structural:** BM25 with one combined `Bm25StatisticsProvider`, the sum of both searchers' doc counts, token counts and doc freqs, via `Searcher::search_with_statistics_provider` (tantivy 0.27). That makes the scores comparable. `Engine::top` returns `(id, score)`.
  - The scope filter is a `Must` term-set clause on `module`, or a collector over a slot bitset (builder's call).
- **Dense:** the cosine scores are comparable as they are. `Dense::nearest` returns `(id, score)`. The snapshot scan visits only in-scope slots.
- **Name:** exact matches first, then by Levenshtein distance, then PageRank, over both. The exact-name bonus applies to every in-scope declaration with that name, and names can repeat across modules.
- The coverage weight of the dense lane, priors, the sorry and generated filters, deprecated folding and rendering stay as they are.
- `render` already prints `flags.imports`, so a snapshot hit's `text` shows the header that loads it.

**Scope resolution:** `All` means every module that has at least one entry. `Labels(map)` intersects the label index's entry sets, smallest first, then maps entries to module slots, giving a slot bitset per query.

### 6. API (`crates/pisa-protocol/src/search.rs`)
- `Query.snapshots: SnapshotScope`, default `None`. In JSON: `"none"`, `"all"`, or `{"labels": {"k": "v", …}}`.
  - Labels are checked with `labels::check_labels`, at most 64 of them; an empty map is a 400 that says to use `"all"`.
  - Any scope other than `none` on a server without snapshot search is a 400.
- `libraries` gains `PisaSnap` as a known library while snapshot search is on. So `{"snapshots": "all", "libraries": ["PisaSnap"]}` searches only snapshots. `libraries: ["PisaSnap"]` with scope `none` is a 400. The known `kinds` include the snapshot index's kinds.
- `Hit.snapshot: Option<SnapshotHit { entries: Vec<String>, dirty: bool, verified: bool }>`, my call:
  - `entries`: the in-scope entries that keep the module, newest first, at most 10;
  - `dirty` and `verified`: the module's, so a hit relying on a user `axiom` is visibly untrusted. Such hits are not filtered out.
  - `module` is the snapshot module, and `flags.imports` its header.
- `/v1/resolve` and `/v1/decl` don't change.

### 7. Config, deploy, clients, docs
- **`serve` flags:** `--tikv-pd` / `PISA_TIKV_PD`, which turns snapshot search on; `--snapshot-version`, default the bundle's `env_id`, for workers that set `PISA_VERSION`; `--describe-model`; `--describe-concurrency`.
- **`docker-compose.yml` `search` service:** `PISA_TIKV_PD=pd:2379`, `PISA_SEARCH_DESCRIBE_MODEL`, `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`.
- **Clients:** add the `snapshots` field to Rust `pisa_client::SearchClient`, and Python `SearchClient.search(..., snapshots="all" | {"labels": {...}})` with stubs.
- **README:** in Search, cover snapshot search:
  - scope and freshness;
  - with a describe model set, every snapshot's code goes to that provider, and every module's signatures and docstrings go to the embedder;
  - cost, about $0.0003 per declaration with Haiku;
  - the embedder constraint and the TiKV requirement. In Snapshots, add a line pointing to it. List the new KV keys where the keys are documented (`catalog.rs` module docs).

### Non-goals
- Backfilling modules built before this change. They have no `decls` record and stay unsearchable. Re-creating the same code adds an entry without rebuilding, so that doesn't help either.
- A search Helm chart (none exists today).
- Coordinating several search replicas.
- Snapshot declarations in `/v1/resolve` and `/v1/decl`.
- Including imported snapshots in a label scope.
- PageRank over snapshot declarations.
- Snapshot search with `PISA_METADATA=memory` across processes.

## Implementation order
1. Create `pisa-kv` (move, plus the `test-util` feature) → `cargo test --workspace` green.
2. Worker: `decls` record + export `run_cmd`, change log, cleanup, pruning. Extend `fake-repl` (`crates/pisa-tests/src/bin`) to answer the export command the way it answers the `PISA_DECLS` report.
3. Protocol: `SnapshotScope`, `SnapshotHit`.
4. Search: mirror and tailer, then the snapshot index, then lane merging and scope, then describe and embed, then `serve` flags.
5. Clients, compose, README.

## Verification
- **Unit:**
  - `SnapshotScope` serde round-trips and rejections;
  - log key order and the lookback rescan;
  - label intersection;
  - snapshot index add/remove per module;
  - merged lanes rank a strong snapshot match above weaker base hits;
  - `snapshots: none` returns exactly today's results on the test bundle (`tests/search.rs`).
- **Search integration** (`crates/pisa-search/tests/`): shared `Kv::memory()`; write module, entry and `decls` records through `pisa_kv::catalog`. Then run the tailer and indexer against the mock Anthropic/OpenAI and embeddings servers already in `tests/informalize.rs`, and assert:
  - a hit appears under `all` and under its label, and not under another label or `none`;
  - a relabel moves it;
  - a delete removes it;
  - a restart rebuilds it from `described` with no LLM calls (the mock counts requests);
  - with a describe model, every new module is described (the mock sees each declaration once);
  - without one, nothing is sent to the mock LLM, and declarations are still found by name and docstring and get a vector.
- **Worker integration** (`pisa-tests`, fake-repl): a create writes `decls/m` and a `log` key; a delete removes `decls`/`described`; a sweep prunes old log keys.
- **End to end** (`scripts/e2e.py --mathlib`, compose with `--profile search`, real Lean + TiKV):
  1. Create a labelled snapshot with a docstring'd lemma.
  2. Poll `/v1/search` with `{"snapshots": {"labels": {...}}, "nl": …}` until the hit appears, within 60 s.
  3. Check its `flags.imports` header compiles with `/check`.
  4. Relabel, delete, and check the hit disappears.
  5. Run with `claude-haiku-5-5` + Scaleway (which matches the published bundle's vectors), then again with an OpenAI describe model.
- **TEI:** the compose `tei` service runs Qwen3-Embedding-0.6B, which doesn't match the 8B bundle. So TEI is covered by the search integration test, against a mock TEI `/embed` server (`Embedder::parse` with a bare URL), plus one e2e run that checks only the snapshot side: hits appear, move with labels and go away on delete. Ranking against the base libraries isn't checked in that run.
- `cargo test --workspace`, `cargo clippy --workspace`, and the Python tests (`crates/pisa-python/tests`).
