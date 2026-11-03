# Metadata in a KV store, with labelled snapshot entries

## Context

Today every piece of worker metadata is a JSON object in the S3 bucket next to the blobs it describes:

- `snapshots/<v>/<id>.json` is a `SnapshotMeta`, plus empty `snapshots/<v>/<parent>.children/<child>` marker objects so deletes can cascade.
- `states/<v>/<hash>.json` is a `StateMeta`, next to `states/<v>/<hash>.olean.zst`.
- `sessions/<v>/<id>.json` is a `SessionMeta`.

S3 can't list "the snapshots labelled X". Its "write the metadata last" ordering also leaves races that a transaction would close. For example, `verify_module` re-reads and then rewrites `<id>.json` (snapshots.rs:719-729), so a delete that lands between the two steps is undone.

**Goal (user's):** API callers attach their own key/value labels to what they create, change them later, and list by label. Labels from different callers stay separate. All metadata moves into a key/value store (TiKV in deployments); the blobs (module files, pickles) stay in S3.

### Decisions made with the user
- **Entries over shared modules.** A *module* is what a snapshot is today: code compiled to `PisaSnap.s<hash(code)>`, shared and content-addressed, with `verified`, `dirty` and the check result. An *entry* is one create's handle on a module, with its own id, labels and creation time.
  - Every `POST /snapshots` makes a new entry. If the module already exists it isn't rebuilt.
  - Nothing merges across creates.
  - Code still imports modules, not entries.
- **Labels** belong to entries. They can be set and removed after creation.
- **Filters:** exact `key=value` pairs, ANDed. Listing returns entries, newest first, paginated with a cursor, e.g. `GET /snapshots?label=project:foo&label=kind:lemma`.
- **Delete** drops only the entry. A module is removed once no entry references it and no kept module imports it. So nobody's delete can take away someone else's entry or the modules under it.
- **Retries:** an optional client-chosen idempotency key on create. The same key returns the same entry; without a key, every create is new.
- **API break is fine.** Entry ids replace module names in `/snapshots/{…}` URLs. The in-repo Rust/Python clients, tests and the e2e script are updated in the same change.
- **Store:** a pluggable KV layer with a **TiKV** backend and an **in-memory** backend for dev and tests.
- **Scope:** **all** metadata moves: modules, entries, child links, proof states, sessions.
- **Migration:** none. Deployments start with an empty metadata store, and old S3 JSON objects are ignored.
- **TiKV topology:** one dedicated cluster per pisa deployment, plaintext. So keys use a fixed `pisa` root, and `PISA_TIKV_PD` is the only connection setting.

### Non-goals
- OR, prefix or exists matching; filtering on module fields (verified, dirty, imports).
- Labels on states or sessions.
- Auth or ownership: anyone who has an entry id can change or delete that entry.
- A module-level delete or purge.
- Moving blobs out of S3.
- Letting the router talk to the KV store: list, get, label and delete jobs go to "any worker of the version", like get/delete/verify do now.
- pisa-search's description/vector store (`informal_store.rs`).
- Migrating or cleaning up old S3 metadata objects.

## Design

### 1. KV layer: `crates/pisa-worker/src/kv.rs` (new)

A small transactional API, kept low-level so the index logic exists once, above the backends:

```rust
pub enum Kv { Memory(MemoryKv), Tikv(TikvKv) }   // Clone; enum dispatch, no async_trait needed
impl Kv { pub async fn begin(&self) -> Result<Txn, KvError>; }
impl Txn {
    async fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>, KvError>;
    async fn scan(&mut self, start: Vec<u8>, end: Vec<u8>, limit: u32) -> Result<Vec<(Vec<u8>, Vec<u8>)>, KvError>;
    async fn put(&mut self, key: Vec<u8>, value: Vec<u8>) -> Result<(), KvError>;
    async fn delete(&mut self, key: Vec<u8>) -> Result<(), KvError>;
    /// Adds `key` to the write set without changing it, so a concurrent write conflicts.
    async fn lock(&mut self, key: Vec<u8>) -> Result<(), KvError>;
    async fn commit(self) -> Result<(), KvError>;   // KvError::Conflict → caller retries
}
```

- **Memory backend:** `Arc<tokio::sync::Mutex<BTreeMap<Vec<u8>, Vec<u8>>>>`. `begin` takes the owned lock, so transactions are serialized and can't conflict. Writes are buffered and applied on commit; dropping a transaction rolls it back. Rule: never begin a transaction while holding one, and never hold one across a Lean run.
- **TiKV backend:** `tikv-client = { version = "0.4", default-features = false }` (0.4.0 is on crates.io; turning off default features drops the prometheus push dependency). Use `TransactionClient::new(pd_endpoints)` with optimistic transactions; `lock` maps to `lock_keys`. Write conflicts map to `KvError::Conflict`. TiKV's optimistic transactions only catch write–write conflicts, which is why `lock` exists: a transaction that relies on a key it only read must lock it.
- A retry helper with a few attempts runs a transaction body again on `Conflict`. Its shape is the builder's call. The builder may add `batch_get` for listing.
- **Key encoding:** keys are tuples of strings. A string component is encoded as its bytes with `0x00` escaped to `0x00 0xFF`, followed by a `0x00` terminator (FoundationDB-style). This keeps order and makes "all keys under this prefix" a range scan, which matters because versions, label keys and values, and idempotency keys are arbitrary strings. Every key starts with the fixed root `"pisa"` and then the version. Unit-test ordering and prefix ranges, including strings containing `0x00`.

### 2. Key layout (per version `v`; `T` = timestamp as 16-digit zero-padded hex, `R` = `u64::MAX - created` in the same form)

| Key | Value | Purpose |
|---|---|---|
| `(pisa, v, mod, m)` | `ModuleMeta` JSON | today's `SnapshotMeta` (imports, modules, header, code, dirty, verified, macros, result, created) |
| `(pisa, v, child, parent, m)` | empty | `m` imports `parent` (replaces `.children/` markers) |
| `(pisa, v, ref, m, e)` | empty | entry `e` points at module `m` |
| `(pisa, v, entry, e)` | `EntryMeta` JSON: `{module, labels, created, key?}` | an entry |
| `(pisa, v, new, R, e)` | empty | every entry, newest first |
| `(pisa, v, label, key, value, R, e)` | empty | one per label, newest first |
| `(pisa, v, key, k)` | entry id | idempotency key → entry |
| `(pisa, v, state, hash)` | `StateMeta` JSON + `created` | replaces `states/<v>/<hash>.json` |
| `(pisa, v, session, id)` | `SessionMeta` JSON + `created` | replaces `sessions/<v>/<id>.json` |
| `(pisa, v, age, T, kind, id)` | empty | sweep scans this up to the cutoff |

**Entry ids** (my call): `uuid::Uuid::new_v4().simple()`, 32 hex characters. They are random, so an entry id isn't guessable from the code.

### 3. Snapshots (`crates/pisa-worker/src/snapshots.rs`)

`Snapshots` holds the `Kv` as well as the `ObjectStore`. Module blobs keep their S3 paths (`snapshots/<v>/<m>.<ext>.zst`). `SnapshotMeta` is renamed `ModuleMeta`, with the same fields. Internals that work on modules (`ensure`, `ensure_one`, `fetch`, `build`, `verify_chain`, `verify_module`, the confirmation cache) keep working on module ids, with lookups moved from S3 to `mod/m`.

**Create** (`create(pool, job)`; the job now carries `labels` and an optional `key`):
1. Validate labels and key, and check the imports parse, as now.
2. **Key given and `key/k` exists:** load that entry. If its module is `hash(code)`, return the entry as is (the request's labels are ignored). Otherwise return an in-band `bad_request` ("idempotency key already used for different code").
3. **Module `mod/m` exists:** if `verify: true` and it isn't verified, verify the chain first (as `verify_existing` does now). Then add the entry in one transaction, which locks `mod/m` so a concurrent clean-up conflicts. If the module disappeared, start the create over, which now builds it.
4. **Otherwise build** as now. `commit` uploads the blobs to S3 first, then runs one transaction:
   - Lock and read each parent's `mod` record; any missing parent is `not_found`.
   - Write `mod/m`, unless another worker just built the same code, in which case keep theirs.
   - Write a `child/<parent>/m` per import.
   - Add the entry.
5. Creates that keep nothing (Lean errors, policy, timeouts) write no entry and no key, so a retry with the same key can still succeed.

*Adding an entry* writes `entry/e`, `ref/m/e`, `new/R/e`, a `label/k/v/R/e` per label, and `key/k` if a key was given. With a key, the transaction reads `key/k` first and handles it as in step 2 if it's there. Two creates racing on the same key then both write `key/k`, so one conflicts, retries and finds the other's entry.

**Delete** (`delete(e)`):
1. In one transaction: read `entry/e` (missing is `not_found`), delete it and every key listed under "Adding an entry", then try to clean up its module.
2. **Cleaning up module `m`:** lock and read `mod/m`. If any `ref/m/*` or `child/m/*` key exists (a scan with limit 1 each), stop. Otherwise delete `mod/m` and the `child/<parent>/m` key under each of its imports.
3. After commit, delete the removed module's S3 blobs and `discard` it locally. Then try to clean up each of its parents in its own transaction, and so on up the chain.
4. Return `{entry, removed: [module names]}`.

A crash partway up the chain leaves unreferenced modules behind. That's harmless but leaks storage; accepted, as blob leaks are today.

**Verify** (`verify(e)`): resolve the entry to its module and run `verify_chain` on it. `verify_module` does its read-modify-write of `mod/m` in one transaction; a module removed meanwhile stays removed (`not_found`). `verified` and `dirty` are module facts, so every entry on that module reflects the result.

**Get** (`get(e)`): the entry plus its module. Returns `SnapshotInfo { entry, snapshot (module name), imports (module names), header, code, labels, dirty, verified, result, created (the entry's) }`.

**Set labels** (`set_labels(e, set, remove)`): one transaction applies the removes, then the sets. It updates the `label` keys (deleting the old value's key on change) and writes `entry/e`. Returns the entry's summary.

**List** (`list(labels, limit, after)`):
- No labels: scan `new` from the cursor.
- With labels: scan the `label` keys of the first pair from the cursor. For each `(R, e)`, check the other pairs with point gets of their exact keys (`label/k2/v2/R/e`). Continue in pages until there are `limit` matches or the range is exhausted.
- Then fetch each entry and its module.
- The cursor is the opaque `R`+`e` of the last item returned. Callers should put their most selective label first; document this.

**Label and key rules (my call; in `pisa-protocol` so router and worker agree):**
- Label key: 1–128 bytes of `[A-Za-z0-9._/-]`, so `key:value` splits on the first `:`.
- Label value: 0–512 bytes of UTF-8.
- At most 64 labels per entry.
- Idempotency key: 1–256 bytes of `[A-Za-z0-9._:/-]`.
- A violation is `bad_request` (400), in-band on create as other create errors are.

### 4. States and sessions (`crates/pisa-worker/src/store.rs`)

`Store` holds `inner: Arc<dyn ObjectStore>` (pickles only) and a `kv: Kv`.
- `put_state`: put the pickle to S3, then in a transaction put `state/hash` with `created`, put its `age` key, and delete the old `age` key if the record already existed (pickles are content-addressed).
- `state_meta`, `session` and `put_session` go to KV. A session now has no S3 object at all.
- `state_pickle`: a missing pickle becomes `NotFound`, not internal; it can only happen at the TTL boundary.
- `sweep(ttl)`:
  1. Scan `age` from the start to the cutoff in batches. In a transaction per batch, delete the records and their `age` keys.
  2. Run the existing S3 listing sweep over `states/` for pickles older than the TTL, which also catches orphans whose metadata write failed.

  Entries and modules never expire.

### 5. Protocol and router

- `pisa-protocol/src/api.rs`:
  - `SnapshotRequest` gains `labels: BTreeMap<String, String>` and `key: Option<String>`.
  - `SnapshotResponse` gains `entry: Option<String>` (absent when nothing was kept) and `labels`.
  - `SnapshotInfo` as above.
  - New `SnapshotSummary { entry, snapshot, imports, header, labels, dirty, verified, created }`. My call: no `code` or `result`, which can be large; `GET /snapshots/{entry}` has them.
  - New `SnapshotList { entries: Vec<SnapshotSummary>, next: Option<String> }`.
  - New `LabelsRequest { set: BTreeMap, remove: Vec<String> }`.
  - `Deleted` becomes `{ entry, removed: Vec<String> }`.
  - `SnapshotVerified` gains `entry`.
  - Validation helpers.
- `pisa-protocol/src/wire.rs`:
  - `SnapshotCreateJob` gains `labels` and `key` (default, skip if empty).
  - `SnapshotJob { snapshot }` becomes `SnapshotJob { entry }`.
  - New `Job::SnapshotList(SnapshotListJob { labels: Vec<(String, String)>, limit, after })` and `Job::SnapshotLabels(SnapshotLabelsJob { entry, set, remove })`, with matching `JobResult` variants.
- `pisa-router/src/http.rs` and `snapshots.rs`:
  - `.route("/snapshots", post(create).get(list))`
  - `.route("/snapshots/{entry}", get(get).patch(labels).delete(delete))`
  - `.route("/snapshots/{entry}/verify", post(verify))`

  Create keeps its routing by the next header, since its module is known from the code. The rest use the existing `submit` helper. The `?label=` parameter repeats, which `axum::extract::Query` (serde_urlencoded) can't put into a `Vec`, so the list handler parses `RawQuery` itself (e.g. with `form_urlencoded`/`url`, already in the tree through reqwest). Query: `version`, `label` (repeated), `limit` (default 100, max 1000), `after`.
- `pisa-worker/src/lib.rs`: route the new and renamed jobs in `run_job`.

### 6. Config and wiring

- `config.rs`: `--metadata` / `PISA_METADATA` = `memory` | `tikv` (default `memory`), and `--tikv-pd` / `PISA_TIKV_PD` (comma-separated PD endpoints, required for `tikv`). `PISA_STORE` keeps choosing the blob store. Add a `kv()` constructor next to `object_store()`.
- `lib.rs`: `run_with_store(cfg, store, shutdown)` becomes `run_with_stores(cfg, store, kv, shutdown)`, and both `Snapshots` and `Store` get the `Kv`.

### 7. Clients and docs

- `pisa-client`:
  - Snapshot methods take entry ids.
  - Add `list_snapshots(labels, limit, after)` and `set_snapshot_labels(entry, set, remove)`.
  - Add a way to pass labels and a key on create. One method taking `SnapshotOptions { verify, labels, key }` is the builder's call.
  - `delete_snapshot(entry)` returns the removed module names.
  - Response types gain `entry` and `labels`.
- `pisa-python`: the same in `src/lib.rs`, `_native.pyi` and `tests/test_client.py`.
- `README.md`:
  - Rewrite the snapshot endpoint rows and the Snapshots section around entries vs modules.
  - Document labels, filters and pagination, idempotency keys, and delete/clean-up (replacing "a delete also removes every snapshot that imports it" under Retention).
  - Document the `PISA_METADATA` / `PISA_TIKV_PD` config and the storage notes: metadata in TiKV, blobs in S3.

### 8. Deploy

- `docker-compose.yml`: add `pd` and `tikv` services, single node, pinned `pingcap/pd` and `pingcap/tikv` images of the same version. TiKV needs `--advertise-addr tikv:20160` and PD `--advertise-client-urls http://pd:2379`, because clients dial the address TiKV registers. It may also need a raised `nofile` ulimit. Workers get `PISA_METADATA: tikv` and `PISA_TIKV_PD: pd:2379`, and `depends_on` them.
- `deploy/minikube/tikv.yaml` (new): a throwaway single PD plus single TiKV with emptyDir storage and stable Service DNS names used as advertise addresses, in the style of `garage.yaml`. `deploy/minikube/values.yaml` points at it.
- `deploy/helm/pisa`: `values.yaml` gets `metadata.tikv.pdEndpoints` (required), and `templates/worker.yaml` sets `PISA_METADATA=tikv` and `PISA_TIKV_PD`. Like S3 today, the chart doesn't deploy TiKV itself; the docs point production users at TiDB Operator.

## Files

New: `crates/pisa-worker/src/kv.rs`, `deploy/minikube/tikv.yaml`.

Modified:
- Cargo: `Cargo.toml` (workspace dep `tikv-client`), `crates/pisa-worker/Cargo.toml`.
- Worker: `crates/pisa-worker/src/{config,lib,snapshots,store,sessions}.rs`.
- Protocol: `crates/pisa-protocol/src/{api,wire}.rs`.
- Router: `crates/pisa-router/src/{http,snapshots}.rs`.
- Clients: `crates/pisa-client/src/lib.rs` and `crates/pisa-python/{src/lib.rs,python/pisa_client/_native.pyi,tests/test_client.py}`.
- Tests: `crates/pisa-tests/tests/{common/mod.rs,stage1.rs,stage4.rs,client.rs}`.
- Scripts and deploy: `scripts/e2e.py`, `docker-compose.yml`, `deploy/helm/pisa/{values.yaml,templates/worker.yaml}`, `deploy/minikube/values.yaml`, `README.md`.

**Version control:** the working copy (jj change `tyvtqxqx`, no description) holds the uncommitted verify-on-request work (`SnapshotVerify` job, `verify_existing`, new stage4 tests), and this design builds on it. Start with `jj new` so this work is its own change on top of that one. Don't squash, rebase or push.

## Verification

1. **Unit tests** (`cargo test -p pisa-worker -p pisa-protocol`):
   - Key encoding order and prefix ranges.
   - Memory `Kv` semantics: buffered writes, rollback on drop, scan bounds and limits.
   - The `store.rs` round trip and sweep, rewritten for KV.
   - Label and key validation.
2. **Integration tests** (`cargo test -p pisa-tests`; the `Cluster` gets a shared `pub kv: Kv`). Port `stage4.rs` from module names to entry ids. `stored_objects` lists S3 blobs and, separately, KV keys. The stored-report tampering test does it through `mod/m`. New and changed tests:
   - Same code created twice gives two entries with separate labels, one module and one build. This replaces "creates are idempotent".
   - The same idempotency key returns the same entry; the same key with different code is a 400.
   - PATCH set and remove updates listings.
   - Newest-first pagination with `after`.
   - AND of two labels.
   - Listings are per version.
   - Deleting one of two entries on a module keeps the module; deleting the last removes it and its blobs.
   - A module that others' modules import stays until they go, and then goes up the chain.
   - Deletes leave no `label`, `new`, `ref`, `child` or `key` keys behind.
   - Unknown entries are 404.
   - A clean-up during verification leaves the module removed.
3. **TiKV backend:**
   - The same `Kv` contract tests, run against TiKV when `PISA_TEST_TIKV_PD` is set and skipped otherwise.
   - `docker compose up --build`, then `scripts/e2e.py` ported to entries and extended with: create twice with different labels → list filtered → PATCH → delete one → module still importable → delete the other → module gone.
4. **Clients:** `cargo test -p pisa-tests --test client` and the Python `tests/test_client.py`.
5. **Minikube:** `kubectl apply -f deploy/minikube/tikv.yaml`, install the chart with the minikube values, and run `scripts/e2e.py` against it.
