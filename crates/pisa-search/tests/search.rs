//! Loads a bundle built from a small handwritten corpus into Qdrant and
//! checks the lanes, fusion, folding and the HTTP API. Needs a Qdrant at
//! `PISA_TEST_QDRANT_URL`; without one, every test is skipped.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use pisa_protocol::search::{Informal, Query};
use pisa_search::corpus::{self, Corpus, Decl, Manifest};
use pisa_search::engine::Engine;
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

fn nl(text: &str) -> Query {
    Query {
        nl: Some(text.into()),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn natural_language_finds_descriptions_and_docstrings() {
    let Some(t) = engine().await else { return };
    let hits = t.names(nl("trace of the identity matrix")).await;
    assert_eq!(hits[0], "Matrix.trace_one", "{hits:?}");
    let hits = t.names(nl("uncertainty relation for observables")).await;
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
            q: Some("Real.sqrt_mul_sefl".into()),
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
            q: Some("sqrt of a times a".into()),
            k: 10,
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
        .search(&nl("commutativity of multiplication a * b = b * a"))
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
    let q = nl("eigenstates of the harmonic oscillator are orthonormal");
    assert!(
        !t.names(q.clone())
            .await
            .iter()
            .any(|n| n.ends_with("eigenstates_orthonormal"))
    );
    let r = t
        .engine
        .search(&Query {
            include_sorried: true,
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
            k: 50,
            ..nl("the")
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
            ..nl("trace")
        })
        .await;
    assert_eq!(hits[0], "Matrix.trace");
    t.done().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn results_are_deterministic_and_rendered_compactly() {
    let Some(t) = engine().await else { return };
    let q = nl("trace matrix");
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
    let d = t.engine.detail("Matrix.trace").await.unwrap().unwrap();
    assert_eq!(d.deps, ["Matrix"]);
    assert!(d.used_by.contains(&"Matrix.trace_one".to_string()));
    assert!(t.engine.detail("Nope").await.unwrap().is_none());
    t.done().await;
}

/// Serves `t`'s engine; returns its address.
async fn serve(t: &Loaded) -> std::net::SocketAddr {
    let q = t.q.clone();
    let symbols = Symbols::parse(symbols::DEFAULT).unwrap();
    let engine = Engine::open(q, &t.env_id, symbols, None).await.unwrap();
    let app = pisa_search::server::router(Arc::new(engine));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

#[tokio::test(flavor = "multi_thread")]
async fn http_api() {
    let Some(t) = engine().await else { return };
    let addr = serve(&t).await;
    let c = reqwest::Client::new();
    let url = |p: &str| format!("http://{addr}{p}");

    let r: serde_json::Value = c
        .post(url("/v1/search"))
        .json(&serde_json::json!({"queries": [{"nl": "trace of the identity matrix"}, {"name": "Nat.succ_le_succ", "verify": true}]}))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(r["env_id"], t.env_id);
    assert_eq!(r["results"][0]["hits"][0]["name"], "Matrix.trace_one");
    assert_eq!(r["results"][1]["hits"][0]["name"], "Nat.succ_le_succ");
    assert_eq!(r["results"][1]["verification"], "unavailable");

    let r: serde_json::Value = c
        .post(url("/v1/resolve"))
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

    let r = c.get(url("/v1/decl/Matrix.trace")).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let r = c
        .get(url("/v1/decl/Real.sqrt_mul_sefl"))
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
        .post(url("/v1/search"))
        .json(&serde_json::json!({"queries": [], "env_id": "other"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    let r = c
        .post(url("/v1/search"))
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
    t.done().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn rust_client() {
    use pisa_client::{ErrorKind, SearchClient};

    let Some(t) = engine().await else { return };
    let addr = serve(&t).await;
    let c = SearchClient::new(&format!("http://{addr}")).unwrap();
    c.health().await.unwrap();

    let r = c
        .search(&Query {
            nl: Some("trace of the identity matrix".into()),
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

    let r = c
        .resolve(["Real.sqrt_mul_sefl", "Matrix.trace"])
        .await
        .unwrap();
    assert!(!r[0].exists && r[0].suggestions[0].name == "Real.sqrt_mul_self");
    assert!(r[1].exists);

    let d = c.decl("Matrix.trace").await.unwrap();
    assert_eq!(d.deps, ["Matrix"]);
    let err = c.decl("Real.sqrt_mul_sefl").await.unwrap_err();
    assert_eq!(err.kind(), Some(ErrorKind::NotFound));

    let pinned = c.clone().with_env("other");
    let err = pinned.search(&Query::default()).await.unwrap_err();
    assert_eq!(err.kind(), Some(ErrorKind::NotFound));
    assert!(c.with_env(&t.env_id).resolve(["Matrix"]).await.unwrap()[0].exists);
    t.done().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn queries_skip_blanks_noise_and_bad_filters() {
    let Some(t) = engine().await else { return };
    assert!(
        t.names(Query {
            q: Some("   ".into()),
            ..Default::default()
        })
        .await
        .is_empty()
    );
    assert!(
        t.names(Query {
            nl: Some("trace".into()),
            k: 0,
            ..Default::default()
        })
        .await
        .is_empty()
    );

    // Generated helpers and tooling stay out of results unless named exactly.
    let trace = t
        .names(Query {
            nl: Some("trace of a matrix".into()),
            k: 20,
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
            q: Some("Matrix.mk.injEq".into()),
            k: 1,
            ..Default::default()
        })
        .await;
    assert_eq!(named, ["Matrix.mk.injEq"]);

    // So does a declaration that depends on sorry, flagged.
    let sorried = "QuantumMechanics.HarmonicOscillator.eigenstates_orthonormal";
    let hits = t
        .engine
        .search(&Query {
            q: Some(sorried.into()),
            k: 1,
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
