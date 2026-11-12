//! Loads a bundle built from a small handwritten corpus into Qdrant and
//! checks the lanes, fusion, folding and the HTTP API. Needs a Qdrant at
//! `PISA_TEST_QDRANT_URL`; without one, every test is skipped.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use pisa_protocol::search::{Format, Informal, Query};
use pisa_protocol::version::Versions;
use pisa_search::corpus::{self, Corpus, Decl, Manifest};
use pisa_search::engine::{Engine, Lanes};
use pisa_search::qdrant::Qdrant;
use pisa_search::symbols::{self, Symbols};
use pisa_search::{index, load, points};

struct D<'a> {
    name: &'a str,
    kind: &'a str,
    library: &'a str,
    signature: &'a str,
    type_refs: &'a [&'a str],
    value_refs: &'a [&'a str],
    doc: Option<&'a str>,
}

fn d<'a>(
    name: &'a str,
    kind: &'a str,
    library: &'a str,
    signature: &'a str,
    type_refs: &'a [&'a str],
) -> D<'a> {
    D {
        name,
        kind,
        library,
        signature,
        type_refs,
        value_refs: &[],
        doc: None,
    }
}

fn decl(x: &D) -> Decl {
    Decl {
        name: x.name.into(),
        kind: x.kind.into(),
        module: format!("{}.Fixture", x.library),
        library: x.library.into(),
        signature: x.signature.into(),
        type_pp_full: x.signature.into(),
        type_refs: x.type_refs.iter().map(|s| s.to_string()).collect(),
        value_refs: x.value_refs.iter().map(|s| s.to_string()).collect(),
        docstring: x.doc.map(Into::into),
        ..Default::default()
    }
}

fn corpus() -> Corpus {
    let specs = [
        D {
            doc: Some("The trace of a square matrix: the sum of its diagonal entries."),
            ..d(
                "Matrix.trace",
                "def",
                "Mathlib",
                "Matrix.trace {n R} [Fintype n] [AddCommMonoid R] (A : Matrix n n R) : R",
                &["Matrix"],
            )
        },
        d(
            "Matrix",
            "def",
            "Mathlib",
            "Matrix (m n α : Type) : Type",
            &[],
        ),
        D {
            value_refs: &["Matrix.trace"],
            ..d(
                "Matrix.trace_one",
                "theorem",
                "Mathlib",
                "Matrix.trace_one : Matrix.trace 1 = Fintype.card n",
                &["Matrix.trace", "Matrix"],
            )
        },
        D {
            value_refs: &["Matrix.trace"],
            ..d(
                "Matrix.trace_mul_comm",
                "theorem",
                "Mathlib",
                "Matrix.trace_mul_comm (A : Matrix m n R) (B : Matrix n m R) : Matrix.trace (A * B) = Matrix.trace (B * A)",
                &["Matrix.trace", "Matrix"],
            )
        },
        D {
            doc: Some("The square root of a real number."),
            ..d(
                "Real.sqrt",
                "def",
                "Mathlib",
                "Real.sqrt (x : ℝ) : ℝ",
                &["Real"],
            )
        },
        d("Real", "def", "Mathlib", "Real : Type", &[]),
        d(
            "Fintype.card",
            "def",
            "Mathlib",
            "Fintype.card (α : Type) [Fintype α] : ℕ",
            &[],
        ),
        D {
            value_refs: &["Real.sqrt"],
            ..d(
                "Real.sqrt_mul_self",
                "theorem",
                "Mathlib",
                "Real.sqrt_mul_self {a : ℝ} (h : 0 ≤ a) : √(a * a) = a",
                &["Real.sqrt", "Real"],
            )
        },
        d(
            "Real.mul_self_sqrt",
            "theorem",
            "Mathlib",
            "Real.mul_self_sqrt {a : ℝ} (h : 0 ≤ a) : √a * √a = a",
            &["Real.sqrt", "Real"],
        ),
        D {
            doc: Some("Deprecated alias."),
            ..d(
                "Real.sqrt_mul_self_old",
                "theorem",
                "Mathlib",
                "Real.sqrt_mul_self_old {a : ℝ} (h : 0 ≤ a) : √(a * a) = a",
                &["Real.sqrt", "Real"],
            )
        },
        d(
            "Nat.succ_le_succ",
            "theorem",
            "Mathlib",
            "Nat.succ_le_succ {n m : ℕ} : n ≤ m → n.succ ≤ m.succ",
            &["Nat"],
        ),
        d("Nat", "inductive", "Mathlib", "Nat : Type", &[]),
        d(
            "mul_comm",
            "theorem",
            "Mathlib",
            "mul_comm {G} [CommMagma G] (a b : G) : a * b = b * a",
            &["HMul.hMul"],
        ),
        d(
            "add_comm",
            "theorem",
            "Mathlib",
            "add_comm {G} [AddCommMagma G] (a b : G) : a + b = b + a",
            &["HAdd.hAdd"],
        ),
        d("HMul.hMul", "def", "Mathlib", "HMul.hMul : α → β → γ", &[]),
        d("HAdd.hAdd", "def", "Mathlib", "HAdd.hAdd : α → β → γ", &[]),
        D {
            doc: Some("Robertson's uncertainty relation for two observables."),
            ..d(
                "QuantumMechanics.state_uncertainty_of_raw_commutator",
                "theorem",
                "Physlib",
                "QuantumMechanics.state_uncertainty_of_raw_commutator : ΔA ψ * ΔB ψ ≥ |⟪ψ, [A, B] ψ⟫| / 2",
                &[],
            )
        },
        D {
            doc: Some("The harmonic oscillator ladder operators."),
            ..d(
                "QuantumMechanics.HarmonicOscillator.ladderOperator",
                "def",
                "Physlib",
                "QuantumMechanics.HarmonicOscillator.ladderOperator (Q : HarmonicOscillator) : Operator",
                &[],
            )
        },
        D {
            doc: Some("The eigenstates of the harmonic oscillator are orthonormal."),
            ..d(
                "QuantumMechanics.HarmonicOscillator.eigenstates_orthonormal",
                "theorem",
                "Physlib",
                "eigenstates_orthonormal : ⟪Q.eigenstate n, Q.eigenstate n'⟫ = δ[n,n']",
                &[],
            )
        },
        D {
            doc: Some("Equality of matrices built from their entries; trace helper."),
            ..d(
                "Matrix.mk.injEq",
                "theorem",
                "Mathlib",
                "Matrix.mk.injEq : trace",
                &[],
            )
        },
        D {
            doc: Some("A tactic helper for the trace of a matrix."),
            ..d(
                "Mathlib.Tactic.TraceHelper.trace_matrix",
                "def",
                "Mathlib",
                "Mathlib.Tactic.TraceHelper.trace_matrix : Unit",
                &[],
            )
        },
        D {
            doc: Some("The Lorentz group is connected."),
            ..d(
                "LorentzGroup.isConnected",
                "theorem",
                "Physlib",
                "LorentzGroup.isConnected : IsConnected (LorentzGroup d)",
                &[],
            )
        },
    ];
    let mut decls: Vec<Decl> = specs.iter().map(decl).collect();
    let by = |decls: &mut Vec<Decl>, name: &str| decls.iter().position(|d| d.name == name).unwrap();
    let i = by(
        &mut decls,
        "QuantumMechanics.state_uncertainty_of_raw_commutator",
    );
    decls[i].imports = Some(UNCERTAINTY_IMPORTS.into());
    let i = by(&mut decls, "Real.sqrt_mul_self_old");
    decls[i].deprecated_to = Some("Real.sqrt_mul_self".into());
    let i = by(&mut decls, "mul_comm");
    decls[i].additive_sibling = Some("add_comm".into());
    let i = by(&mut decls, "add_comm");
    decls[i].additive_sibling = Some("mul_comm".into());
    let i = by(
        &mut decls,
        "QuantumMechanics.HarmonicOscillator.eigenstates_orthonormal",
    );
    decls[i].uses_sorry = true;
    // Conclusion keys as `PisaExport` prints a discrimination-tree path.
    let keys = |ks: &[&str]| ks.iter().map(|k| k.to_string()).collect::<Vec<_>>();
    for (name, ks) in [
        (
            "Matrix.trace_one",
            &[
                "Eq",
                "*",
                "Matrix.trace",
                "*",
                "*",
                "*",
                "*",
                "1",
                "Fintype.card",
                "*",
                "*",
            ][..],
        ),
        (
            "Matrix.trace_mul_comm",
            &[
                "Eq",
                "*",
                "Matrix.trace",
                "*",
                "*",
                "*",
                "*",
                "HMul.hMul",
                "*",
                "*",
                "Matrix.trace",
            ],
        ),
        (
            "Real.sqrt_mul_self",
            &[
                "Eq",
                "Real",
                "Real.sqrt",
                "HMul.hMul",
                "Real",
                "Real",
                "Real",
                "*",
                "*",
                "*",
            ],
        ),
        (
            "Real.mul_self_sqrt",
            &[
                "Eq",
                "Real",
                "HMul.hMul",
                "Real",
                "Real",
                "Real",
                "Real.sqrt",
                "*",
                "Real.sqrt",
                "*",
                "*",
            ],
        ),
    ] {
        let i = by(&mut decls, name);
        decls[i].concl_keys = keys(ks);
    }
    let i = by(&mut decls, "Matrix.trace");
    decls[i].value = Some("∑ i, A i i".into());
    decls.sort_by(|a, b| a.name.cmp(&b.name));
    corpus::compute_hashes(&mut decls);
    let mut libraries = std::collections::BTreeMap::new();
    for d in &decls {
        *libraries.entry(d.library.clone()).or_insert(0) += 1;
    }
    Corpus {
        manifest: Manifest {
            env_id: "v4.35.0-test".into(),
            decls: decls.len(),
            libraries,
            sources: Default::default(),
        },
        decls,
        modules: Vec::new(),
    }
}

/// The engine over the test corpus, loaded into a collection of its own,
/// or `None` (the test is skipped) without a Qdrant.
async fn engine() -> Option<Loaded> {
    let Ok(url) = std::env::var("PISA_TEST_QDRANT_URL") else {
        eprintln!("skipped: PISA_TEST_QDRANT_URL is unset");
        return None;
    };
    static N: AtomicUsize = AtomicUsize::new(0);
    let env_id = format!(
        "test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    );
    let dir = tempfile::tempdir().unwrap();
    let mut c = corpus();
    c.manifest.env_id = env_id.clone();
    let mut informal = HashMap::new();
    informal.insert(
        "Matrix.trace_one".to_string(),
        Informal {
            informal_name: "Trace of the identity matrix".into(),
            statement_nl: "The trace of the identity matrix equals the dimension.".into(),
            statement_latex: Some(r"\operatorname{Tr}(I_n) = n".into()),
            source: "test".into(),
            ..Default::default()
        },
    );
    index::build(&c, &informal, None, dir.path()).unwrap();
    let q = Qdrant::new(&url, None).unwrap();
    load::load(dir.path(), &q, 1, 0.0).await.unwrap();
    let symbols = Symbols::parse(symbols::DEFAULT).unwrap();
    let engine = Engine::open(q.clone(), &env_id, symbols, None)
        .await
        .unwrap();
    Some(Loaded {
        engine,
        q,
        env_id,
        _dir: dir,
    })
}

struct Loaded {
    engine: Engine,
    q: Qdrant,
    env_id: String,
    _dir: tempfile::TempDir,
}

impl Loaded {
    async fn names(&self, q: Query) -> Vec<String> {
        let r = self.engine.search(&q).await.unwrap();
        r.hits.into_iter().map(|h| h.name).collect()
    }

    /// Drops the test's collections.
    async fn done(self) {
        for c in [
            points::collection(&self.env_id),
            points::entries_collection(&self.env_id),
        ] {
            self.q.drop(&c).await.unwrap();
        }
    }
}

fn description(text: &str) -> Query {
    Query {
        description: Some(text.into()),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn natural_language_finds_descriptions_and_docstrings() {
    let Some(t) = engine().await else { return };
    let hits = t.names(description("trace of the identity matrix")).await;
    assert_eq!(hits[0], "Matrix.trace_one", "{hits:?}");
    let hits = t
        .names(description("uncertainty relation for observables"))
        .await;
    assert_eq!(
        hits[0], "QuantumMechanics.state_uncertainty_of_raw_commutator",
        "{hits:?}"
    );
    t.done().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn latex_and_patterns_reach_the_right_declarations() {
    let Some(t) = engine().await else { return };
    let hits = t
        .names(Query {
            latex: Some(r"\operatorname{Tr}(I) = n".into()),
            ..Default::default()
        })
        .await;
    assert_eq!(hits[0], "Matrix.trace_one", "{hits:?}");
    let hits = t
        .names(Query {
            pattern: Some("Matrix.trace (?A * ?B) = Matrix.trace (?B * ?A)".into()),
            ..Default::default()
        })
        .await;
    assert_eq!(hits[0], "Matrix.trace_mul_comm", "{hits:?}");
    t.done().await;
}

/// The header a fixture declaration records, as one outside the default
/// imports does.
const UNCERTAINTY_IMPORTS: &str = "import Mathlib\nimport Physlib\nimport QuantumInfo";

#[tokio::test(flavor = "multi_thread")]
async fn a_name_resolves_with_the_imports_that_load_it() {
    let Some(t) = engine().await else { return };
    let names = [
        "QuantumMechanics.state_uncertainty_of_raw_commutator",
        "QuantumMechanics.state_uncertainty_of_raw_comutator",
    ];
    let r = t.engine.resolve(&names.map(String::from)).await.unwrap();
    assert_eq!(r[0].imports.as_deref(), Some(UNCERTAINTY_IMPORTS));
    assert_eq!(
        r[1].suggestions[0].imports.as_deref(),
        Some(UNCERTAINTY_IMPORTS)
    );
    t.done().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn misspelled_names_resolve_with_suggestions() {
    let Some(t) = engine().await else { return };
    let names = [
        "Real.sqrt_mul_sefl",
        "Real.sqrt_mul_self",
        "Real.sqrt_mul_self_old",
    ];
    let r = t.engine.resolve(&names.map(String::from)).await.unwrap();
    assert!(!r[0].exists);
    assert_eq!(r[0].suggestions[0].name, "Real.sqrt_mul_self");
    assert!(r[1].exists && r[1].suggestions.is_empty());
    assert_eq!(r[2].deprecated_to.as_deref(), Some("Real.sqrt_mul_self"));
    // A free-text query that is a name goes through the name lane.
    let hits = t
        .names(Query {
            text: Some("Real.sqrt_mul_sefl".into()),
            ..Default::default()
        })
        .await;
    assert_eq!(hits[0], "Real.sqrt_mul_self", "{hits:?}");
    t.done().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn folding_flags_and_filters() {
    let Some(t) = engine().await else { return };
    // The deprecated alias folds into its replacement.
    let hits = t
        .names(Query {
            text: Some("sqrt of a times a".into()),
            limit: 10,
            ..Default::default()
        })
        .await;
    assert!(hits.contains(&"Real.sqrt_mul_self".to_string()), "{hits:?}");
    assert!(
        !hits.contains(&"Real.sqrt_mul_self_old".to_string()),
        "{hits:?}"
    );

    // to_additive pairs show once, naming the sibling.
    let r = t
        .engine
        .search(&description(
            "commutativity of multiplication a * b = b * a",
        ))
        .await
        .unwrap();
    let comm: Vec<_> = r
        .hits
        .iter()
        .filter(|h| h.name.ends_with("_comm") && !h.name.contains('.'))
        .collect();
    assert_eq!(
        comm.len(),
        1,
        "{:?}",
        r.hits.iter().map(|h| &h.name).collect::<Vec<_>>()
    );
    assert!(comm[0].flags.additive_sibling.is_some());

    // Sorried results are excluded unless asked for, then flagged.
    let q = description("eigenstates of the harmonic oscillator are orthonormal");
    assert!(
        !t.names(q.clone())
            .await
            .iter()
            .any(|n| n.ends_with("eigenstates_orthonormal"))
    );
    let r = t
        .engine
        .search(&Query {
            include_sorried: Some(true),
            ..q
        })
        .await
        .unwrap();
    let hit = r
        .hits
        .iter()
        .find(|h| h.name.ends_with("eigenstates_orthonormal"))
        .unwrap();
    assert!(hit.flags.uses_sorry && hit.text.contains("depends on sorry"));

    let hits = t
        .names(Query {
            libraries: vec!["Physlib".into()],
            limit: 50,
            ..description("the")
        })
        .await;
    assert!(
        hits.iter()
            .all(|n| !n.starts_with("Matrix") && !n.starts_with("Real")),
        "{hits:?}"
    );
    let hits = t
        .names(Query {
            kinds: vec!["def".into()],
            ..description("trace")
        })
        .await;
    assert_eq!(hits[0], "Matrix.trace");
    t.done().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn results_are_deterministic_and_rendered_compactly() {
    let Some(t) = engine().await else { return };
    let q = description("trace matrix");
    assert_eq!(t.names(q.clone()).await, t.names(q.clone()).await);
    let hit = &t.engine.search(&q).await.unwrap().hits[0];
    assert!(
        hit.text.starts_with(&format!("1. {}", hit.name)),
        "{}",
        hit.text
    );
    assert!(hit.text.lines().count() <= 4);
    t.done().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn detail_lists_dependencies_and_users() {
    let Some(t) = engine().await else { return };
    let d = t
        .engine
        .detail("Matrix.trace", None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(d.dependencies, ["Matrix"]);
    assert!(d.used_by.contains(&"Matrix.trace_one".to_string()));
    assert!(t.engine.detail("Nope", None).await.unwrap().is_none());
    t.done().await;
}

/// Serves `t`'s engine, searching the snapshots of version `snapshots` if
/// any; returns its address.
async fn serve(t: &Loaded, snapshots: Option<&str>) -> std::net::SocketAddr {
    let q = t.q.clone();
    let symbols = Symbols::parse(symbols::DEFAULT).unwrap();
    let mut engine = Engine::open(q, &t.env_id, symbols, None).await.unwrap();
    engine.snapshots = snapshots.map(str::to_string);
    let search = pisa_search::server::Search::ready(Arc::new(engine), Versions::default());
    let app = pisa_search::server::router(search);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

#[tokio::test(flavor = "multi_thread")]
async fn http_api() {
    let Some(t) = engine().await else { return };
    let addr = serve(&t, None).await;
    let c = reqwest::Client::new();
    let url = |p: &str| format!("http://{addr}{p}");

    let r: serde_json::Value = c
        .post(url("/search"))
        .json(&serde_json::json!({"queries": [{"text": "trace of the identity matrix", "limit": 3}, {"name": "Nat.succ_le_succ"}]}))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(r["version"], t.env_id);
    let hits = r["results"][0]["hits"].as_array().unwrap();
    assert!(hits.len() <= 3, "{r}");
    assert_eq!(hits[0]["name"], "Matrix.trace_one");
    assert_eq!(hits[0]["title"], "Trace of the identity matrix");
    // A bundle declaration is no snapshot's.
    for field in ["snapshot_ids", "status", "verified"] {
        assert!(hits[0].get(field).is_none(), "{field}: {r}");
    }
    assert_eq!(r["results"][1]["hits"][0]["name"], "Nat.succ_le_succ");

    let r: serde_json::Value = c
        .post(url("/resolve"))
        .json(&serde_json::json!({"names": ["Real.sqrt_mul_sefl"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["results"][0]["exists"], false);
    assert_eq!(
        r["results"][0]["suggestions"][0]["name"],
        "Real.sqrt_mul_self"
    );

    let r: serde_json::Value = c
        .get(url("/declarations/Matrix.trace"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["version"], t.env_id);
    assert_eq!(
        r["declaration"]["dependencies"],
        serde_json::json!(["Matrix"])
    );
    let r = c
        .get(url("/declarations/Real.sqrt_mul_sefl"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    let body: serde_json::Value = r.json().await.unwrap();
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("did you mean Real.sqrt_mul_self")
    );

    let r = c
        .post(url("/search"))
        .json(&serde_json::json!({"queries": [], "version": "other"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    let r = c
        .get(url("/declarations/Matrix.trace?version=other"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    let r = c
        .post(url("/search"))
        .json(&serde_json::json!({"nope": 1}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    let health: serde_json::Value = c
        .get(url("/health"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["qdrant"], true);
    assert_eq!(health["version"], t.env_id);
    t.done().await;
}

/// A server whose snapshots are another version's answers for that
/// version too, from its index.
#[tokio::test(flavor = "multi_thread")]
async fn the_snapshot_version_is_accepted() {
    let Some(t) = engine().await else { return };
    let addr = serve(&t, Some("snapshots-v")).await;
    let c = reqwest::Client::new();
    let search = |version: &str| {
        c.post(format!("http://{addr}/search"))
            .json(&serde_json::json!({"queries": [{"name": "Matrix.trace"}], "version": version}))
            .send()
    };
    let r = search("snapshots-v").await.unwrap();
    assert_eq!(r.status(), 200);
    let r: serde_json::Value = r.json().await.unwrap();
    assert_eq!(r["version"], t.env_id);
    assert_eq!(r["results"][0]["hits"][0]["name"], "Matrix.trace");
    assert_eq!(search("other").await.unwrap().status(), 404);
    t.done().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn rust_client() {
    use pisa_client::{Client, ErrorKind, Format};

    let Some(t) = engine().await else { return };
    let addr = serve(&t, None).await;
    let c = Client::new(&format!("http://{addr}")).unwrap();
    c.health().await.unwrap();

    let r = c
        .search(&Query {
            description: Some("trace of the identity matrix".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(r.hits[0].name, "Matrix.trace_one");
    let batch = c
        .search_batch(vec![
            Query {
                name: Some("Nat.succ_le_succ".into()),
                ..Default::default()
            },
            Query::default(),
        ])
        .await
        .unwrap();
    assert_eq!(batch.len(), 2);
    assert_eq!(batch[0].hits[0].name, "Nat.succ_le_succ");
    let text = c
        .search(&Query {
            name: Some("Nat.succ_le_succ".into()),
            format: Format::Text,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(text.hits.is_empty());
    assert!(text.text.unwrap().starts_with("1. Nat.succ_le_succ"));

    let r = c
        .resolve(["Real.sqrt_mul_sefl", "Matrix.trace"])
        .await
        .unwrap();
    assert!(!r[0].exists && r[0].suggestions[0].name == "Real.sqrt_mul_self");
    assert!(r[1].exists);

    let d = c.get_declaration("Matrix.trace", None).await.unwrap();
    assert_eq!(d.dependencies, ["Matrix"]);
    let err = c
        .get_declaration("Real.sqrt_mul_sefl", None)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), Some(ErrorKind::NotFound));

    let pinned = c.clone().with_version("other");
    let err = pinned.search(&Query::default()).await.unwrap_err();
    assert_eq!(err.kind(), Some(ErrorKind::NotFound));
    let pinned = c.with_version(&t.env_id);
    assert!(pinned.resolve(["Matrix"]).await.unwrap()[0].exists);
    t.done().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn queries_skip_blanks_noise_and_bad_filters() {
    let Some(t) = engine().await else { return };
    assert!(
        t.names(Query {
            text: Some("   ".into()),
            ..Default::default()
        })
        .await
        .is_empty()
    );
    assert!(
        t.names(Query {
            description: Some("trace".into()),
            limit: 0,
            ..Default::default()
        })
        .await
        .is_empty()
    );

    // Generated helpers and tooling stay out of results unless named exactly.
    let trace = t
        .names(Query {
            description: Some("trace of a matrix".into()),
            limit: 20,
            ..Default::default()
        })
        .await;
    assert!(trace.contains(&"Matrix.trace".to_string()), "{trace:?}");
    assert!(
        !trace
            .iter()
            .any(|n| n.contains("injEq") || n.contains("Tactic")),
        "{trace:?}"
    );
    let named = t
        .names(Query {
            text: Some("Matrix.mk.injEq".into()),
            limit: 1,
            ..Default::default()
        })
        .await;
    assert_eq!(named, ["Matrix.mk.injEq"]);

    // So does a declaration that depends on sorry, flagged.
    let sorried = "QuantumMechanics.HarmonicOscillator.eigenstates_orthonormal";
    let hits = t
        .engine
        .search(&Query {
            text: Some(sorried.into()),
            limit: 1,
            ..Default::default()
        })
        .await
        .unwrap()
        .hits;
    assert_eq!(hits[0].name, sorried);
    assert!(hits[0].flags.uses_sorry);

    let mut q = Query {
        libraries: vec!["physlib".into()],
        kinds: vec!["THEOREM".into()],
        ..Default::default()
    };
    t.engine.check_filters(&mut q).unwrap();
    assert_eq!(
        (q.libraries, q.kinds),
        (vec!["Physlib".to_string()], vec!["theorem".to_string()])
    );
    let mut q = Query {
        libraries: vec!["nope".into()],
        ..Default::default()
    };
    let err = t.engine.check_filters(&mut q).unwrap_err();
    assert!(err.contains("known: Mathlib, Physlib"), "{err}");
    t.done().await;
}

fn only(lanes: &str) -> Lanes {
    Lanes {
        lexical: lanes.contains("lexical"),
        name: lanes.contains("name"),
        structural: lanes.contains("structural"),
        concl: lanes.contains("concl"),
        dense: false,
    }
}

/// A goal's target finds the theorem that concludes it, through the
/// conclusion lane, whatever its hypotheses mention.
#[tokio::test(flavor = "multi_thread")]
async fn a_goal_finds_the_theorem_that_concludes_it() {
    let Some(t) = engine().await else { return };
    let goal = Query {
        goal: Some("x : ℝ\nh : Real.sqrt x = 0\n⊢ Matrix.trace 1 = Fintype.card n".into()),
        ..Default::default()
    };
    let r = t
        .engine
        .search_batch(std::slice::from_ref(&goal), only("concl"))
        .await
        .unwrap();
    let hits: Vec<&str> = r[0].hits.iter().map(|h| h.name.as_str()).collect();
    assert_eq!(hits.first(), Some(&"Matrix.trace_one"), "{hits:?}");
    // Real.sqrt is only in a hypothesis.
    assert!(!hits.iter().any(|h| h.starts_with("Real.")), "{hits:?}");
    // All lanes together put it first too.
    assert_eq!(t.names(goal).await[0], "Matrix.trace_one");
    t.done().await;
}

/// `consts` names constants exactly, for the structural and conclusion lanes.
#[tokio::test(flavor = "multi_thread")]
async fn consts_name_the_constants_a_search_is_about() {
    let Some(t) = engine().await else { return };
    let q = Query {
        consts: vec!["Fintype.card".into()],
        ..Default::default()
    };
    let r = t
        .engine
        .search_batch(std::slice::from_ref(&q), only("concl"))
        .await
        .unwrap();
    assert_eq!(
        r[0].hits.first().map(|h| h.name.as_str()),
        Some("Matrix.trace_one")
    );
    // The structural lane reads a declaration's type, which here mentions
    // `Matrix.trace` but not `Fintype.card`.
    let q = Query {
        consts: vec!["Matrix.trace".into()],
        ..Default::default()
    };
    let r = t
        .engine
        .search_batch(std::slice::from_ref(&q), only("structural"))
        .await
        .unwrap();
    let hits: Vec<&str> = r[0].hits.iter().map(|h| h.name.as_str()).collect();
    assert!(hits.contains(&"Matrix.trace_one"), "{hits:?}");
    t.done().await;
}

/// A collection loaded before the conclusion lane has no room for it: the
/// load says to drop it rather than fail half way.
#[tokio::test(flavor = "multi_thread")]
async fn a_collection_without_the_conclusion_lane_is_refused() {
    let Ok(url) = std::env::var("PISA_TEST_QDRANT_URL") else {
        return;
    };
    let q = Qdrant::new(&url, None).unwrap();
    let env_id = format!("test-{}-old", std::process::id());
    let collection = points::collection(&env_id);
    let mut config = points::collection_config(1);
    config["sparse_vectors"]
        .as_object_mut()
        .unwrap()
        .remove("concl");
    q.create(&collection, &config).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut c = corpus();
    c.manifest.env_id = env_id.clone();
    index::build(&c, &HashMap::new(), None, dir.path()).unwrap();
    let e = load::load(dir.path(), &q, 1, 0.0).await.unwrap_err();
    assert!(e.to_string().contains("concl"), "{e}");
    q.drop(&collection).await.unwrap();
}

/// A definition's hit, and its detail, say what it is defined as; a
/// theorem's say nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_definition_carries_its_value() {
    let Some(t) = engine().await else { return };
    let r = t
        .engine
        .search(&description("trace of a matrix"))
        .await
        .unwrap();
    let trace = r.hits.iter().find(|h| h.name == "Matrix.trace").unwrap();
    assert_eq!(trace.value.as_deref(), Some("∑ i, A i i"));
    assert!(trace.text.contains("\n   := ∑ i, A i i"), "{}", trace.text);
    let theorem = r.hits.iter().find(|h| h.kind == "theorem").unwrap();
    assert_eq!(theorem.value, None);
    let d = t
        .engine
        .detail("Matrix.trace", None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(d.value.as_deref(), Some("∑ i, A i i"));
    t.done().await;
}

/// `format: "text"` answers with the hits rendered, one after another, and
/// no hits.
#[tokio::test(flavor = "multi_thread")]
async fn a_text_result_is_its_hits_rendered() {
    let Some(t) = engine().await else { return };
    let q = Query {
        limit: 3,
        ..description("trace of a matrix")
    };
    let full = t.engine.search(&q).await.unwrap();
    assert_eq!(full.text, None);
    let text = t
        .engine
        .search(&Query {
            format: Format::Text,
            ..q
        })
        .await
        .unwrap();
    assert!(text.hits.is_empty());
    let rendered: Vec<&str> = full.hits.iter().map(|h| h.text.as_str()).collect();
    assert_eq!(text.text, Some(rendered.join("\n\n")));
    t.done().await;
}
