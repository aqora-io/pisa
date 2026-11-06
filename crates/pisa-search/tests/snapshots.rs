//! Searches snapshot declarations next to a small bundle in Qdrant: the
//! catalog is a database written as workers write it (SQLite in memory, or
//! the PostgreSQL at `PISA_TEST_DATABASE_URL` when that is set), an indexer
//! mirrors it into Qdrant, and the LLM and the embedder are mock servers.
//! Needs a Qdrant at `PISA_TEST_QDRANT_URL`; without one, every test is
//! skipped.
//!
//! The embedder's blocking HTTP client can't be made or dropped on the
//! runtime's threads, so every test shares one, [`Setup::embedder`], and
//! ends with [`Setup::done`].

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use pisa_db::catalog::{Catalog, Module, ModuleMeta, NewEntry, SnapshotDecl, SnapshotDecls};
use pisa_db::db::Db;
use pisa_protocol::api::CheckResult;
use pisa_protocol::search::{Hit, Query, QueryResult, SnapshotScope};
use pisa_search::anthropic::Client;
use pisa_search::corpus::{Corpus, Decl, Manifest};
use pisa_search::dense::{self, Embedder};
use pisa_search::engine::{Engine, Lanes};
use pisa_search::indexer::{Describe, Described, Indexer};
use pisa_search::informalize::Config;
use pisa_search::llm::Llm;
use pisa_search::qdrant::Qdrant;
use pisa_search::symbols::{self, Symbols};
use pisa_search::{index, load, points};
use serde_json::{Value, json};
use tokio::task::JoinHandle;

const MODEL: &str = "words@test";
/// Short leases, so that handovers happen within a test.
const TTL: Duration = Duration::from_secs(3);

/// A bag-of-words vector: texts that share words are near.
fn embed(text: &str) -> Vec<f32> {
    let mut v = vec![0.0f32; dense::DIMS];
    for word in text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2)
    {
        let h = word
            .to_lowercase()
            .bytes()
            .fold(7u64, |h, b| h.wrapping_mul(31).wrapping_add(u64::from(b)));
        v[(h % dense::DIMS as u64) as usize] += 1.0;
    }
    v[0] += 0.01;
    dense::truncate_normalize(v)
}

#[derive(Default)]
struct Mocks {
    llm_calls: AtomicUsize,
    embed_calls: AtomicUsize,
    /// How long each embedding request takes.
    embed_delay_ms: AtomicU64,
    /// How many of the coming embedding requests fail.
    embed_failures: AtomicUsize,
    /// Each embedding request's inputs.
    embedded: Mutex<Vec<Vec<String>>>,
    /// Embedding requests answered, failed ones too.
    embed_answered: AtomicUsize,
}

async fn messages(State(mocks): State<Arc<Mocks>>, Json(body): Json<Value>) -> Json<Value> {
    mocks.llm_calls.fetch_add(1, Ordering::SeqCst);
    let prompt = body["messages"][0]["content"].as_str().unwrap_or_default();
    let name = prompt
        .lines()
        .find_map(|l| l.strip_prefix("name: "))
        .unwrap_or_default();
    let out = json!({
        "informal_name": format!("Twice {name}"),
        "statement_nl": "Multiplying a natural number by two gives the number added to itself.",
        "statement_latex": "2n = n + n",
        "aliases": [],
        "synthetic_queries": [],
    });
    Json(json!({
        "model": body["model"],
        "stop_reason": "end_turn",
        "content": [{"type": "text", "text": out.to_string()}],
        "usage": {"input_tokens": 10, "output_tokens": 10},
    }))
}

async fn tei(
    State(mocks): State<Arc<Mocks>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    mocks.embed_calls.fetch_add(1, Ordering::SeqCst);
    let inputs: Vec<String> = body["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap().to_string())
        .collect();
    mocks.embedded.lock().unwrap().push(inputs.clone());
    let fail = mocks
        .embed_failures
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
        .is_ok();
    let delay = mocks.embed_delay_ms.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(delay)).await;
    mocks.embed_answered.fetch_add(1, Ordering::SeqCst);
    if fail {
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }
    let vectors: Vec<Vec<f32>> = inputs.iter().map(|t| embed(t)).collect();
    Ok(Json(json!(vectors)))
}

fn base_decl(name: &str, signature: &str, doc: &str) -> Decl {
    Decl {
        name: name.into(),
        kind: "theorem".into(),
        module: "Mathlib.Fixture".into(),
        library: "Mathlib".into(),
        signature: signature.into(),
        type_pp_full: signature.into(),
        type_refs: vec!["Nat".into()],
        docstring: Some(doc.into()),
        ..Default::default()
    }
}

struct Setup {
    q: Qdrant,
    env_id: String,
    db: Db,
    /// The catalog's version: the test's own on PostgreSQL, which tests
    /// share.
    version: String,
    catalog: Catalog,
    mocks: Arc<Mocks>,
    url: String,
    embedder: Embedder,
    tasks: Vec<JoinHandle<()>>,
    _dir: tempfile::TempDir,
}

/// Mock servers, and a bundle of two Mathlib theorems with vectors from
/// [`embed`] loaded into collections of the test's own; `None` without a
/// Qdrant.
async fn setup() -> Option<Setup> {
    let Ok(qdrant) = std::env::var("PISA_TEST_QDRANT_URL") else {
        eprintln!("skipped: PISA_TEST_QDRANT_URL is unset");
        return None;
    };
    let mocks = Arc::new(Mocks::default());
    let app = axum::Router::new()
        .route("/v1/messages", axum::routing::post(messages))
        .route("/embed", axum::routing::post(tei))
        .with_state(mocks.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    static N: AtomicUsize = AtomicUsize::new(0);
    let env_id = format!(
        "snaptest-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    );
    let mut decls = vec![
        base_decl(
            "Nat.add_comm",
            "Nat.add_comm (n m : ℕ) : n + m = m + n",
            "Addition of natural numbers is commutative.",
        ),
        base_decl(
            "Nat.mul_one",
            "Nat.mul_one (n : ℕ) : n * 1 = n",
            "Multiplying by one leaves a number unchanged.",
        ),
    ];
    decls.sort_by(|a, b| a.name.cmp(&b.name));
    let corpus = Corpus {
        manifest: Manifest {
            env_id: env_id.clone(),
            decls: decls.len(),
            libraries: BTreeMap::from([("Mathlib".into(), decls.len())]),
            sources: Default::default(),
        },
        decls,
        modules: Vec::new(),
    };
    let vectors: HashMap<String, Vec<f32>> = corpus
        .decls
        .iter()
        .map(|d| (d.name.clone(), embed(&dense::document_view(d, None))))
        .collect();
    let dir = tempfile::tempdir().unwrap();
    index::build(
        &corpus,
        &HashMap::new(),
        Some((MODEL, &vectors)),
        dir.path(),
    )
    .unwrap();
    let q = Qdrant::new(&qdrant, None).unwrap();
    load::load(dir.path(), &q, 1, 1.0).await.unwrap();
    let (db, version) = match std::env::var("PISA_TEST_DATABASE_URL") {
        Ok(url) => (Db::connect(Some(&url)).await.unwrap(), env_id.clone()),
        Err(_) => (Db::memory().await, "v".to_string()),
    };
    let tei = url.clone();
    let embedder = tokio::task::spawn_blocking(move || Embedder::parse(&tei).unwrap());
    Some(Setup {
        q,
        env_id,
        catalog: Catalog::new(db.clone(), version.clone()),
        db,
        version,
        mocks,
        embedder: embedder.await.unwrap(),
        url,
        tasks: Vec::new(),
        _dir: dir,
    })
}

impl Setup {
    /// An engine that searches snapshots too.
    async fn engine(&self) -> Arc<Engine> {
        Arc::new(self.open().await)
    }

    /// [`Setup::engine`], for a test to adjust first.
    async fn open(&self) -> Engine {
        let symbols = Symbols::parse(symbols::DEFAULT).unwrap();
        let embedder = Some(self.embedder.clone());
        let mut e = Engine::open(self.q.clone(), &self.env_id, symbols, embedder)
            .await
            .unwrap();
        e.snapshots = true;
        e
    }

    /// Waits until the embedder has answered `n` requests.
    async fn answered(&self, n: usize) {
        until_true(async || self.mocks.embed_answered.load(Ordering::SeqCst) >= n).await;
    }

    fn how(&self, llm: bool) -> Describe {
        Describe {
            llm: llm.then(|| {
                (
                    Llm::Anthropic(Client::new(&self.url, "key".into())),
                    Config {
                        model: "claude-haiku-5-5".into(),
                        effort: "low".into(),
                        max_tokens: 1000,
                        concurrency: 2,
                    },
                )
            }),
            embedder: Some(self.embedder.clone()),
        }
    }

    /// Starts an indexer for `engine`; returns its task's index.
    fn index(&mut self, engine: &Arc<Engine>, llm: bool) -> usize {
        let indexer = Indexer::new(
            engine.clone(),
            self.db.clone(),
            self.version.clone(),
            self.how(llm),
            2,
            TTL,
        );
        self.tasks.push(tokio::spawn(indexer.run()));
        self.tasks.len() - 1
    }

    /// Stops the tasks, drops what may hold blocking clients off the async
    /// threads, and drops the test's collections.
    async fn done<T: Send + 'static>(self, keep: T) {
        for task in &self.tasks {
            task.abort();
        }
        for task in self.tasks {
            let _ = task.await;
        }
        for c in [
            points::collection(&self.env_id),
            points::entries_collection(&self.env_id),
        ] {
            self.q.drop(&c).await.unwrap();
        }
        let (embedder, keep) = (self.embedder, keep);
        tokio::task::spawn_blocking(move || drop((embedder, keep)))
            .await
            .unwrap();
    }
}

fn meta(id: &str) -> ModuleMeta {
    ModuleMeta {
        imports: Vec::new(),
        modules: vec![Module {
            id: id.into(),
            build: "b".into(),
            files: Vec::new(),
        }],
        header: format!("import Mathlib\nimport PisaSnap.s{id}"),
        code: String::new(),
        dirty: false,
        verified: false,
        macros: false,
        result: CheckResult::default(),
        created: 0,
    }
}

fn snapshot_decl(name: &str, signature: &str, doc: &str) -> SnapshotDecl {
    SnapshotDecl {
        name: name.into(),
        kind: "theorem".into(),
        signature: signature.into(),
        type_pp_full: signature.into(),
        type_refs: vec!["Nat".into(), "double".into()],
        value_refs: Vec::new(),
        docstring: Some(doc.into()),
        deprecated_to: None,
        uses_sorry: false,
    }
}

fn labelled(labels: &[(&str, &str)]) -> NewEntry {
    NewEntry {
        labels: labels
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        key: None,
        replaces: None,
    }
}

/// Stores module `id` with `decls` and an entry with `labels`, as a worker
/// does; returns the entry.
async fn create(
    catalog: &Catalog,
    id: &str,
    decls: Vec<SnapshotDecl>,
    labels: &[(&str, &str)],
) -> String {
    let m = meta(id);
    let decls = SnapshotDecls::new(&m, decls);
    let (e, _, _) = catalog
        .commit(id, &m, Some(&decls), labelled(labels))
        .await
        .unwrap();
    e
}

fn doubling() -> Vec<SnapshotDecl> {
    vec![snapshot_decl(
        "double_eq",
        "double_eq (n : ℕ) : double n = n + n",
        "Doubling a number adds it to itself.",
    )]
}

fn labels(pairs: &[(&str, &str)]) -> SnapshotScope {
    SnapshotScope::Labels(
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    )
}

async fn search(e: &Engine, q: Query) -> Vec<Hit> {
    e.search(&q).await.unwrap().hits
}

async fn result(e: &Engine, q: &Query) -> QueryResult {
    e.search(q).await.unwrap()
}

/// Whether `r` came from every lane.
fn complete(r: &QueryResult) -> bool {
    r.warnings.is_empty() && !r.dense_pending
}

fn about_doubling(snapshots: SnapshotScope) -> Query {
    Query {
        nl: Some("doubling a number adds it to itself".into()),
        snapshots,
        ..Default::default()
    }
}

fn names(hits: &[Hit]) -> Vec<&str> {
    hits.iter().map(|h| h.name.as_str()).collect()
}

/// Waits until `f` holds of the hits for `q`.
async fn until(e: &Engine, q: Query, f: impl Fn(&[Hit]) -> bool) -> Vec<Hit> {
    for _ in 0..200 {
        let hits = search(e, q.clone()).await;
        if f(&hits) {
            return hits;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("never matched; last: {:?}", names(&search(e, q).await));
}

/// Waits until `f` holds.
async fn until_true(f: impl AsyncFn() -> bool) {
    for _ in 0..200 {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("never happened");
}

fn has(hits: &[Hit], name: &str) -> bool {
    hits.iter().any(|h| h.name == name)
}

/// Module `m`'s stored record for declaration `name`.
async fn described(catalog: &Catalog, m: &str, name: &str) -> Option<Described> {
    let records = catalog.described(m).await.unwrap();
    let (_, record) = records.into_iter().find(|(n, _)| n == name)?;
    Some(serde_json::from_value(record).unwrap())
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshots_are_searched_by_scope() {
    let Some(mut s) = setup().await else { return };
    let entry = create(&s.catalog, "a1", doubling(), &[("run", "1")]).await;
    let e = s.engine().await;
    s.index(&e, false);

    // Indexed, and embedded, so the dense lane ranks it first.
    until_true(async || {
        described(&s.catalog, "a1", "double_eq")
            .await
            .is_some_and(|r| r.vector.is_some())
    })
    .await;
    let hits = until(&e, about_doubling(SnapshotScope::All), |hits| {
        hits.first().is_some_and(|h| h.name == "double_eq")
    })
    .await;
    let hit = &hits[0];
    assert_eq!(hit.library, "PisaSnap");
    assert_eq!(hit.module, "PisaSnap.sa1");
    assert_eq!(
        hit.flags.imports.as_deref(),
        Some("import Mathlib\nimport PisaSnap.sa1")
    );
    assert!(hit.text.contains("import PisaSnap.sa1"), "{}", hit.text);
    let snap = hit.snapshot.as_ref().unwrap();
    assert_eq!(snap.entries, [entry]);
    assert!(!snap.dirty && !snap.verified);

    let hits = search(&e, about_doubling(labels(&[("run", "1")]))).await;
    assert_eq!(hits[0].name, "double_eq");
    for scope in [
        SnapshotScope::None,
        labels(&[("run", "2")]),
        labels(&[("run", "1"), ("x", "y")]),
    ] {
        let hits = search(&e, about_doubling(scope.clone())).await;
        assert!(!has(&hits, "double_eq"), "{scope:?}: {:?}", names(&hits));
        assert!(hits.iter().all(|h| h.snapshot.is_none()));
    }

    // By name, exactly or misspelled, within the scope only.
    let by_name = |name: &str, scope| Query {
        name: Some(name.into()),
        snapshots: scope,
        ..Default::default()
    };
    let exact = search(&e, by_name("double_eq", SnapshotScope::All)).await;
    assert_eq!(exact[0].name, "double_eq");
    let misspelled = search(&e, by_name("doubel_eq", SnapshotScope::All)).await;
    assert_eq!(misspelled[0].name, "double_eq");
    let none = search(&e, by_name("double_eq", SnapshotScope::None)).await;
    assert!(!has(&none, "double_eq"));
    s.done(e).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_snapshot_declaration_opens_by_its_module() {
    let Some(mut s) = setup().await else { return };
    create(&s.catalog, "a1", doubling(), &[]).await;
    let e = s.engine().await;
    s.index(&e, false);
    until(&e, about_doubling(SnapshotScope::All), |hits| {
        has(hits, "double_eq")
    })
    .await;

    let d = e.detail("double_eq", Some("PisaSnap.sa1")).await.unwrap();
    let d = d.expect("the snapshot's declaration");
    assert_eq!(
        (d.library.as_str(), d.module.as_str()),
        ("PisaSnap", "PisaSnap.sa1")
    );
    assert_eq!(d.signature, "double_eq (n : ℕ) : double n = n + n");
    // Without its module, a name is the bundle's only.
    assert!(e.detail("double_eq", None).await.unwrap().is_none());
    // The bundle's declarations open from a snapshot's too, as its
    // dependencies would.
    let d = e
        .detail("Nat.add_comm", Some("PisaSnap.sa1"))
        .await
        .unwrap();
    assert_eq!(d.unwrap().module, "Mathlib.Fixture");
    s.done(e).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn scopes_are_checked() {
    let Some(s) = setup().await else { return };
    let e = s.engine().await;
    let symbols = Symbols::parse(symbols::DEFAULT).unwrap();
    let plain = Engine::open(s.q.clone(), &s.env_id, symbols, None)
        .await
        .unwrap();
    let mut q = about_doubling(SnapshotScope::All);
    assert!(plain.check_filters(&mut q).is_err());
    assert!(e.check_filters(&mut q).is_ok());
    let mut q = about_doubling(SnapshotScope::Labels(BTreeMap::new()));
    assert!(e.check_filters(&mut q).unwrap_err().contains("\"all\""));
    let mut q = about_doubling(labels(&[("bad key", "v")]));
    assert!(e.check_filters(&mut q).is_err());

    // `PisaSnap` names the snapshots, which a scope must ask for.
    let mut q = Query {
        libraries: vec!["pisasnap".into()],
        ..about_doubling(SnapshotScope::None)
    };
    assert!(e.check_filters(&mut q).is_err());
    q.snapshots = SnapshotScope::All;
    e.check_filters(&mut q).unwrap();
    assert_eq!(q.libraries, ["PisaSnap"]);
    assert!(plain.check_filters(&mut q).is_err());
    s.done((e, plain)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshots_only() {
    let Some(mut s) = setup().await else { return };
    create(&s.catalog, "a1", doubling(), &[]).await;
    let e = s.engine().await;
    s.index(&e, false);
    let q = Query {
        nl: Some("addition of natural numbers".into()),
        libraries: vec!["PisaSnap".into()],
        ..about_doubling(SnapshotScope::All)
    };
    let hits = until(&e, q, |hits| !hits.is_empty()).await;
    assert!(
        hits.iter().all(|h| h.library == "PisaSnap"),
        "{:?}",
        names(&hits)
    );
    s.done(e).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_query_is_embedded_once() {
    let Some(s) = setup().await else { return };
    let e = s.engine().await;
    let before = s.mocks.embed_calls.load(Ordering::SeqCst);
    let q = about_doubling(SnapshotScope::None);
    let r = result(&e, &q).await;
    assert!(complete(&r), "{r:?}");
    // Other filters, the same text.
    search(&e, Query { k: 3, ..q }).await;
    assert_eq!(s.mocks.embed_calls.load(Ordering::SeqCst), before + 1);
    s.done(e).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_slow_embedding_is_kept_for_the_next_search() {
    let Some(s) = setup().await else { return };
    s.mocks.embed_delay_ms.store(1000, Ordering::SeqCst);
    let mut e = s.open().await;
    e.embed_wait = Duration::from_millis(300);
    let q = about_doubling(SnapshotScope::None);

    let started = Instant::now();
    let r = result(&e, &q).await;
    assert!(started.elapsed() < Duration::from_millis(900));
    assert!(r.dense_pending);
    assert!(r.warnings[0].contains("still being embedded"), "{r:?}");

    s.answered(1).await;
    let r = result(&e, &q).await;
    assert!(complete(&r), "{r:?}");
    assert_eq!(s.mocks.embed_calls.load(Ordering::SeqCst), 1);
    s.done(e).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_abandoned_search_still_caches_its_vector() {
    let Some(s) = setup().await else { return };
    s.mocks.embed_delay_ms.store(500, Ordering::SeqCst);
    let e = s.engine().await;
    let q = about_doubling(SnapshotScope::None);
    // Dropped while it waits, as a server drops a request whose client left.
    let gone = tokio::time::timeout(Duration::from_millis(100), e.search(&q)).await;
    assert!(gone.is_err());

    s.answered(1).await;
    let r = result(&e, &q).await;
    assert!(complete(&r), "{r:?}");
    assert_eq!(s.mocks.embed_calls.load(Ordering::SeqCst), 1);
    s.done(e).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_searches_share_one_embedding() {
    let Some(s) = setup().await else { return };
    s.mocks.embed_delay_ms.store(1000, Ordering::SeqCst);
    let mut e = s.open().await;
    e.embed_wait = Duration::from_secs(3);
    let qs: Vec<Query> = (1..=5)
        .map(|k| Query {
            k,
            ..about_doubling(SnapshotScope::None)
        })
        .collect();
    let rs = futures::future::join_all(qs.iter().map(|q| e.search(q))).await;
    for r in rs {
        let r = r.unwrap();
        assert!(complete(&r), "{r:?}");
    }
    assert_eq!(s.mocks.embed_calls.load(Ordering::SeqCst), 1);

    // A text three times in one batch goes into its request once.
    let t = Query {
        nl: Some("twice a number is the number plus itself".into()),
        ..Default::default()
    };
    let u = Query {
        nl: Some("one is a unit for multiplication".into()),
        ..Default::default()
    };
    let batch = [t.clone(), t.clone(), t, u];
    let rs = e.search_batch(&batch, Lanes::ALL).await.unwrap();
    assert!(rs.iter().all(complete), "{rs:?}");
    assert_eq!(s.mocks.embed_calls.load(Ordering::SeqCst), 2);
    assert_eq!(s.mocks.embedded.lock().unwrap()[1].len(), 2);
    s.done(e).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_embedding_is_not_cached() {
    let Some(s) = setup().await else { return };
    s.mocks.embed_failures.store(1, Ordering::SeqCst);
    let e = s.engine().await;
    let q = about_doubling(SnapshotScope::None);
    let r = result(&e, &q).await;
    assert!(!r.dense_pending);
    assert!(
        r.warnings[0].starts_with("dense lane unavailable") && r.warnings[0].contains("500"),
        "{r:?}"
    );

    let r = result(&e, &q).await;
    assert!(complete(&r), "{r:?}");
    assert_eq!(s.mocks.embed_calls.load(Ordering::SeqCst), 2);
    s.done(e).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn too_many_embeddings_skip_the_dense_lane() {
    let Some(s) = setup().await else { return };
    s.mocks.embed_delay_ms.store(1000, Ordering::SeqCst);
    let mut e = s.open().await;
    e.embed_wait = Duration::from_millis(300);
    let qs: Vec<Query> = (0..33)
        .map(|i| Query {
            nl: Some(format!("doubling number {i}")),
            ..Default::default()
        })
        .collect();
    let rs: Vec<QueryResult> = futures::future::join_all(qs.iter().map(|q| e.search(q)))
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect();
    let busy: Vec<usize> = (0..rs.len())
        .filter(|&i| rs[i].warnings.iter().any(|w| w.contains("already running")))
        .collect();
    assert_eq!(busy.len(), 1, "{rs:?}");
    assert!(!rs[busy[0]].dense_pending);
    assert_eq!(rs.iter().filter(|r| r.dense_pending).count(), 32);

    // Nothing was kept for the text left out, and the permits come back.
    s.answered(32).await;
    s.mocks.embed_delay_ms.store(0, Ordering::SeqCst);
    let left_out = &qs[busy[0]];
    until_true(async || complete(&result(&e, left_out).await)).await;
    assert_eq!(s.mocks.embed_calls.load(Ordering::SeqCst), 33);
    s.done(e).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn descriptions_and_vectors_are_made_once_and_kept() {
    let Some(mut s) = setup().await else { return };
    create(&s.catalog, "a1", doubling(), &[]).await;
    let e = s.engine().await;
    let first = s.index(&e, true);

    // Found by the description's words, which the docstring doesn't have.
    let q = Query {
        nl: Some("twice a natural number".into()),
        ..about_doubling(SnapshotScope::All)
    };
    let hits = until(&e, q.clone(), |hits| {
        hits.first()
            .is_some_and(|h| h.informal_name.as_deref() == Some("Twice double_eq"))
    })
    .await;
    assert_eq!(hits[0].name, "double_eq");
    until_true(async || {
        described(&s.catalog, "a1", "double_eq")
            .await
            .is_some_and(|r| r.informal.is_some() && r.vector.is_some())
    })
    .await;
    assert_eq!(s.mocks.llm_calls.load(Ordering::SeqCst), 1);

    // Another replica takes over and asks for nothing.
    s.tasks[first].abort();
    let (llm, embeds) = (
        s.mocks.llm_calls.load(Ordering::SeqCst),
        s.mocks.embed_calls.load(Ordering::SeqCst),
    );
    let again = s.engine().await;
    s.index(&again, true);
    until_true(async || again.leader.load(Ordering::Acquire)).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(s.mocks.llm_calls.load(Ordering::SeqCst), llm);
    assert_eq!(s.mocks.embed_calls.load(Ordering::SeqCst), embeds);
    let hits = search(&e, q).await;
    assert_eq!(hits[0].informal_name.as_deref(), Some("Twice double_eq"));
    s.done((e, again)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_model_declarations_are_embedded_from_docstrings() {
    let Some(mut s) = setup().await else { return };
    create(&s.catalog, "a1", doubling(), &[]).await;
    let e = s.engine().await;
    s.index(&e, false);
    until_true(async || {
        described(&s.catalog, "a1", "double_eq")
            .await
            .is_some_and(|r| r.vector.is_some() && r.informal.is_none())
    })
    .await;
    let hits = until(&e, about_doubling(SnapshotScope::All), |hits| {
        hits.first().is_some_and(|h| h.name == "double_eq")
    })
    .await;
    assert!(hits[0].informal_name.is_none());
    assert_eq!(s.mocks.llm_calls.load(Ordering::SeqCst), 0);
    s.done(e).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn relabels_and_deletes_are_followed() {
    let Some(mut s) = setup().await else { return };
    let entry = create(&s.catalog, "a1", doubling(), &[("run", "1")]).await;
    let e = s.engine().await;
    s.index(&e, false);
    let run = |v: &str| about_doubling(labels(&[("run", v)]));
    until(&e, run("1"), |h| has(h, "double_eq")).await;

    let patch = BTreeMap::from([("run".to_string(), Some("2".to_string()))]);
    s.catalog.set_labels(&entry, &patch).await.unwrap();
    until(&e, run("2"), |h| has(h, "double_eq")).await;
    assert!(!has(&search(&e, run("1")).await, "double_eq"));

    s.catalog.remove_entry(&entry).await.unwrap();
    until(&e, about_doubling(SnapshotScope::All), |h| {
        !has(h, "double_eq")
    })
    .await;
    s.done(e).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn one_leader_describes_and_another_takes_over() {
    let Some(mut s) = setup().await else { return };
    create(&s.catalog, "a1", doubling(), &[]).await;
    let (one, two) = (s.engine().await, s.engine().await);
    let first = s.index(&one, true);
    until_true(async || one.leader.load(Ordering::Acquire)).await;
    s.index(&two, true);
    until_true(async || {
        described(&s.catalog, "a1", "double_eq")
            .await
            .is_some_and(|r| r.informal.is_some())
    })
    .await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(s.mocks.llm_calls.load(Ordering::SeqCst), 1);
    assert!(!two.leader.load(Ordering::Acquire));

    // The leader goes; the other takes over once its lease ends.
    s.tasks[first].abort();
    until_true(async || two.leader.load(Ordering::Acquire)).await;
    let other = vec![snapshot_decl(
        "halve_eq",
        "halve_eq (n : ℕ) : halve (2 * n) = n",
        "Halving undoes doubling.",
    )];
    create(&s.catalog, "b2", other, &[]).await;
    let q = Query {
        name: Some("halve_eq".into()),
        ..about_doubling(SnapshotScope::All)
    };
    let hits = until(&two, q, |h| has(h, "halve_eq")).await;
    assert_eq!(hits[0].module, "PisaSnap.sb2");
    s.done((one, two)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_module_with_many_entries() {
    let Some(mut s) = setup().await else { return };
    create(&s.catalog, "a1", doubling(), &[("run", "0")]).await;
    let mut entries = Vec::new();
    for i in 1..1000 {
        let run = i.to_string();
        let (e, _) = s
            .catalog
            .add_entry("a1", labelled(&[("run", &run)]))
            .await
            .unwrap()
            .unwrap();
        entries.push(e);
    }
    let e = s.engine().await;
    s.index(&e, false);
    let q = about_doubling(labels(&[("run", "500")]));
    let hits = until(&e, q, |h| has(h, "double_eq")).await;
    let hit = hits.iter().find(|h| h.name == "double_eq").unwrap();
    assert_eq!(
        hit.snapshot.as_ref().unwrap().entries,
        [entries[499].clone()]
    );
    let all = search(&e, about_doubling(SnapshotScope::All)).await;
    let hit = all.iter().find(|h| h.name == "double_eq").unwrap();
    assert_eq!(hit.snapshot.as_ref().unwrap().entries.len(), 10);
    s.done(e).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_name_in_two_modules_is_two_hits() {
    let Some(mut s) = setup().await else { return };
    create(&s.catalog, "a1", doubling(), &[("run", "1")]).await;
    create(&s.catalog, "b2", doubling(), &[("run", "2")]).await;
    let e = s.engine().await;
    s.index(&e, false);
    let q = Query {
        name: Some("double_eq".into()),
        ..about_doubling(SnapshotScope::All)
    };
    let hits = until(&e, q, |h| {
        h.iter().filter(|h| h.name == "double_eq").count() == 2
    })
    .await;
    let mut modules: Vec<&str> = hits
        .iter()
        .filter(|h| h.name == "double_eq")
        .map(|h| h.module.as_str())
        .collect();
    modules.sort();
    assert_eq!(modules, ["PisaSnap.sa1", "PisaSnap.sb2"]);
    s.done(e).await;
}
