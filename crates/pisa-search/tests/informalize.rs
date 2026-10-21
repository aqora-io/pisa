//! Runs informalization against a mock Messages API.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pisa_search::anthropic::Client;
use pisa_search::corpus::{self, Corpus, Decl};
use pisa_search::informal_store::Store;
use pisa_search::informalize::{self, Config};
use serde_json::{Value, json};

#[derive(Default)]
struct Mock {
    calls: AtomicUsize,
    /// (x-api-key, body) of each request.
    requests: Mutex<Vec<(String, Value)>>,
    status: Mutex<Option<StatusCode>>,
}

async fn messages(
    State(mock): State<Arc<Mock>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let key = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    mock.requests.lock().unwrap().push((key, body.clone()));
    if let Some(status) = *mock.status.lock().unwrap() {
        return (status, Json(json!({"type": "error", "error": {"type": "authentication_error", "message": "invalid x-api-key"}}))).into_response();
    }
    // The first call meets an overloaded API and has to be retried.
    if mock.calls.fetch_add(1, Ordering::SeqCst) == 0 {
        return (
            StatusCode::from_u16(529).unwrap(),
            [("retry-after", "0")],
            Json(json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}})),
        )
            .into_response();
    }
    let prompt = body["messages"][0]["content"].as_str().unwrap_or_default();
    let name = prompt
        .lines()
        .find_map(|l| l.strip_prefix("name: "))
        .unwrap_or_default()
        .to_string();
    let usage = json!({"input_tokens": 100, "output_tokens": 50, "cache_read_input_tokens": 1000});
    if name == "Danger.thing" {
        return Json(json!({"model": body["model"], "stop_reason": "refusal", "content": [], "usage": usage})).into_response();
    }
    let out = json!({
        "informal_name": format!("Description of {name}"),
        // A degenerate answer: valid JSON, no description.
        "statement_nl": if name == "Danger.blank" { String::new() } else { format!("What `{name}` says, with $\\nabla f$.") },
        "statement_latex": "\\nabla (f + g) = \\nabla f + \\nabla g",
        "aliases": [],
        "synthetic_queries": ["gradient of a sum", "\\nabla(f+g)", "grad add"],
    });
    Json(json!({
        "model": body["model"],
        "stop_reason": "end_turn",
        "content": [
            {"type": "thinking", "thinking": "", "signature": "sig"},
            {"type": "text", "text": out.to_string()},
        ],
        "usage": usage,
    }))
    .into_response()
}

async fn serve(mock: Arc<Mock>) -> String {
    let app = axum::Router::new()
        .route("/v1/messages", axum::routing::post(messages))
        .with_state(mock);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

fn decl(name: &str, kind: &str, signature: &str, type_refs: &[&str]) -> Decl {
    Decl {
        name: name.into(),
        kind: kind.into(),
        module: "Physlib.Space.Basic".into(),
        library: "Physlib".into(),
        signature: signature.into(),
        type_pp_full: signature.into(),
        type_refs: type_refs.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

fn corpus() -> Corpus {
    let mut decls = vec![
        decl("Space", "def", "Space (d : ℕ := 3) : Type", &[]),
        decl(
            "Space.grad",
            "def",
            "Space.grad {d : ℕ} (f : Space d → ℝ) : Space d → EuclideanSpace ℝ (Fin d)",
            &["Space"],
        ),
        decl(
            "Space.grad_add",
            "theorem",
            "Space.grad_add (f g : Space → ℝ) : Space.grad (f + g) = Space.grad f + Space.grad g",
            &["Space.grad", "Space"],
        ),
        decl("Danger.thing", "def", "Danger.thing : ℕ", &[]),
        decl("Danger.blank", "def", "Danger.blank : ℕ", &[]),
        Decl {
            informal: true,
            docstring: Some(
                "Ehrenfest's theorem: expectation values follow classical equations of motion."
                    .into(),
            ),
            ..decl(
                "QuantumMechanics.ehrenfest_theorem",
                "def",
                "QuantumMechanics.ehrenfest_theorem : InformalLemma",
                &[],
            )
        },
    ];
    corpus::compute_hashes(&mut decls);
    Corpus {
        manifest: Default::default(),
        decls,
        modules: Vec::new(),
    }
}

fn config() -> Config {
    Config {
        model: "claude-haiku-5-5".into(),
        effort: "low".into(),
        max_tokens: 4096,
        concurrency: 2,
    }
}

#[tokio::test]
async fn describes_in_dependency_order_and_caches_by_content_hash() {
    let mock = Arc::new(Mock::default());
    let client = Client::new(&serve(mock.clone()).await, "test-key".into());
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().to_str().unwrap()).unwrap();
    let c = corpus();
    let targets: Vec<&Decl> = c.decls.iter().collect();

    let outcomes = informalize::run(
        &client,
        &config(),
        &store,
        &c,
        &targets,
        HashMap::new(),
        "1-test",
    )
    .await
    .unwrap();

    // Five model calls (the informal result needs none): one refused, one
    // answered without a description.
    assert_eq!(outcomes.len(), 5);
    let mut failed: Vec<(&str, &str)> = outcomes
        .iter()
        .filter_map(|o| Some((o.name.as_str(), o.error.as_deref()?)))
        .collect();
    failed.sort();
    assert_eq!(
        failed,
        [
            ("Danger.blank", "empty description"),
            ("Danger.thing", "stopped: refusal")
        ]
    );
    let ok = outcomes
        .iter()
        .find(|o| o.name == "Space.grad_add")
        .unwrap();
    assert!(ok.checks.passed(), "{:?}", ok.checks);
    assert!(ok.cost.unwrap() > 0.0);

    let requests = mock.requests.lock().unwrap().clone();
    assert!(requests.iter().all(|(key, _)| key == "test-key"));
    let body = &requests[0].1;
    assert_eq!(body["model"], "claude-haiku-5-5");
    assert_eq!(body["output_config"]["effort"], "low");
    assert_eq!(body["output_config"]["format"]["type"], "json_schema");
    assert_eq!(body["system"][1]["cache_control"]["type"], "ephemeral");
    // The overloaded first call was retried with the same body.
    assert_eq!(requests.len(), 6);
    assert_eq!(
        requests.iter().filter(|(_, b)| *b == requests[0].1).count(),
        2
    );

    // Each declaration is described after what it uses, and its prompt shows those descriptions.
    let prompts: Vec<&str> = requests
        .iter()
        .map(|(_, b)| b["messages"][0]["content"].as_str().unwrap())
        .collect();
    let position = |name: &str| {
        prompts
            .iter()
            .rposition(|p| p.contains(&format!("name: {name}\n")))
            .unwrap()
    };
    assert!(position("Space") < position("Space.grad"));
    assert!(position("Space.grad") < position("Space.grad_add"));
    let grad_add = prompts[position("Space.grad_add")];
    assert!(
        grad_add.contains("- `Space.grad`: Description of Space.grad."),
        "{grad_add}"
    );

    let stored = store.load_all().await.unwrap();
    let by_name: HashMap<&str, _> = stored.values().map(|e| (e.name.as_str(), e)).collect();
    assert_eq!(by_name.len(), 4);
    assert_eq!(
        by_name["Space.grad_add"].informal.source,
        "claude-haiku-5-5"
    );
    assert_eq!(by_name["Space.grad_add"].synthetic_queries.len(), 3);
    assert_eq!(
        by_name["QuantumMechanics.ehrenfest_theorem"]
            .informal
            .source,
        informalize::PHYSLIB_DOCSTRING
    );
    let grad = c.decls.iter().find(|d| d.name == "Space.grad").unwrap();
    assert_eq!(stored[&grad.content_hash].name, "Space.grad");
    // The store records model, tokens and cost but never the key.
    let raw: String = std::fs::read_dir(dir.path().join("informal"))
        .unwrap()
        .map(|f| {
            let bytes = std::fs::read(f.unwrap().path()).unwrap();
            String::from_utf8_lossy(&zstd::decode_all(&bytes[..]).unwrap()).into_owned()
        })
        .collect();
    assert!(raw.contains("\"cost_usd\"") && !raw.contains("test-key"));
}

#[tokio::test]
async fn stops_on_authentication_errors() {
    let mock = Arc::new(Mock::default());
    *mock.status.lock().unwrap() = Some(StatusCode::UNAUTHORIZED);
    let client = Client::new(&serve(mock.clone()).await, "bad-key".into());
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().to_str().unwrap()).unwrap();
    let c = corpus();
    let targets: Vec<&Decl> = c.decls.iter().collect();
    let err = informalize::run(
        &client,
        &config(),
        &store,
        &c,
        &targets,
        HashMap::new(),
        "1-test",
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("authentication failed (401)"),
        "{err}"
    );
}
