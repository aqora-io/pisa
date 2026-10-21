//! Builds a bundle from a small handwritten corpus and checks the lanes,
//! fusion, folding and the HTTP API.

use std::collections::HashMap;
use std::sync::Arc;

use pisa_protocol::search::{Informal, Query};
use pisa_search::corpus::{self, Corpus, Decl, Manifest};
use pisa_search::engine::Engine;
use pisa_search::index::{self, Bundle};
use pisa_search::symbols::{self, Symbols};

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

fn engine() -> (tempfile::TempDir, Engine) {
    let dir = tempfile::tempdir().unwrap();
    let c = corpus();
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
    let e = Engine::new(
        Bundle::open(dir.path()).unwrap(),
        Symbols::parse(symbols::DEFAULT).unwrap(),
    )
    .unwrap();
    (dir, e)
}

fn names(e: &Engine, q: Query) -> Vec<String> {
    e.search(&q).hits.into_iter().map(|h| h.name).collect()
}

fn nl(text: &str) -> Query {
    Query {
        nl: Some(text.into()),
        ..Default::default()
    }
}

#[test]
fn natural_language_finds_descriptions_and_docstrings() {
    let (_d, e) = engine();
    let hits = names(&e, nl("trace of the identity matrix"));
    assert_eq!(hits[0], "Matrix.trace_one", "{hits:?}");
    let hits = names(&e, nl("uncertainty relation for observables"));
    assert_eq!(
        hits[0], "QuantumMechanics.state_uncertainty_of_raw_commutator",
        "{hits:?}"
    );
}

#[test]
fn latex_and_patterns_reach_the_right_declarations() {
    let (_d, e) = engine();
    let hits = names(
        &e,
        Query {
            latex: Some(r"\operatorname{Tr}(I) = n".into()),
            ..Default::default()
        },
    );
    assert_eq!(hits[0], "Matrix.trace_one", "{hits:?}");
    let r = e.search(&Query {
        pattern: Some("Matrix.trace (?A * ?B) = Matrix.trace (?B * ?A)".into()),
        ..Default::default()
    });
    assert_eq!(r.hits[0].name, "Matrix.trace_mul_comm");
    assert!(
        r.hits[0].lanes.contains(&"structural".to_string()),
        "{:?}",
        r.hits[0].lanes
    );
}

#[test]
fn misspelled_names_resolve_with_suggestions() {
    let (_d, e) = engine();
    let r = e.resolve("Real.sqrt_mul_sefl");
    assert!(!r.exists);
    assert_eq!(r.suggestions[0].name, "Real.sqrt_mul_self");
    let r = e.resolve("Real.sqrt_mul_self");
    assert!(r.exists && r.suggestions.is_empty());
    let r = e.resolve("Real.sqrt_mul_self_old");
    assert_eq!(r.deprecated_to.as_deref(), Some("Real.sqrt_mul_self"));
    // A free-text query that is a name goes through the name lane.
    let hits = names(
        &e,
        Query {
            q: Some("Real.sqrt_mul_sefl".into()),
            ..Default::default()
        },
    );
    assert_eq!(hits[0], "Real.sqrt_mul_self", "{hits:?}");
}

#[test]
fn folding_flags_and_filters() {
    let (_d, e) = engine();
    // The deprecated alias folds into its replacement.
    let hits = names(
        &e,
        Query {
            q: Some("sqrt of a times a".into()),
            k: 10,
            ..Default::default()
        },
    );
    assert!(hits.contains(&"Real.sqrt_mul_self".to_string()), "{hits:?}");
    assert!(
        !hits.contains(&"Real.sqrt_mul_self_old".to_string()),
        "{hits:?}"
    );

    // to_additive pairs show once, naming the sibling.
    let r = e.search(&nl("commutativity of multiplication a * b = b * a"));
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
        !names(&e, q.clone())
            .iter()
            .any(|n| n.ends_with("eigenstates_orthonormal"))
    );
    let r = e.search(&Query {
        include_sorried: true,
        ..q
    });
    let hit = r
        .hits
        .iter()
        .find(|h| h.name.ends_with("eigenstates_orthonormal"))
        .unwrap();
    assert!(hit.flags.uses_sorry && hit.text.contains("depends on sorry"));

    let hits = names(
        &e,
        Query {
            libraries: vec!["Physlib".into()],
            k: 50,
            ..nl("the")
        },
    );
    assert!(
        hits.iter()
            .all(|n| !n.starts_with("Matrix") && !n.starts_with("Real")),
        "{hits:?}"
    );
    let hits = names(
        &e,
        Query {
            kinds: vec!["def".into()],
            ..nl("trace")
        },
    );
    assert_eq!(hits[0], "Matrix.trace");
}

#[test]
fn results_are_deterministic_and_rendered_compactly() {
    let (_d, e) = engine();
    let q = nl("trace matrix");
    assert_eq!(names(&e, q.clone()), names(&e, q.clone()));
    let hit = &e.search(&q).hits[0];
    assert!(
        hit.text.starts_with(&format!("1. {}", hit.name)),
        "{}",
        hit.text
    );
    assert!(hit.text.lines().count() <= 4);
}

#[test]
fn detail_lists_dependencies_and_users() {
    let (_d, e) = engine();
    let d = e.detail("Matrix.trace").unwrap();
    assert_eq!(d.deps, ["Matrix"]);
    assert!(d.used_by.contains(&"Matrix.trace_one".to_string()));
    assert!(e.detail("Nope").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn http_api() {
    let (_d, e) = engine();
    let app = pisa_search::server::router(Arc::new(e));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let c = reqwest::Client::new();
    let url = |p: &str| format!("http://{addr}{p}");

    let r: serde_json::Value = c
        .post(url("/v1/search"))
        .json(&serde_json::json!({"queries": [{"nl": "trace of the identity matrix"}, {"name": "Nat.succ_le_succ", "verify": true}]}))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(r["env_id"], "v4.35.0-test");
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
}
