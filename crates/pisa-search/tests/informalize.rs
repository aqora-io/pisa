//! Runs informalization against mock Anthropic and OpenAI APIs, and the
//! embedder against a mock OpenAI-compatible embeddings endpoint.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pisa_protocol::search::Informal;
use pisa_search::anthropic::Client;
use pisa_search::corpus::{self, Corpus, Decl};
use pisa_search::informal_store::{Entry, Store};
use pisa_search::informalize::{self, Config};
use pisa_search::llm::Llm;
use pisa_search::{dense, openai};
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
    if judging(&body) {
        return answer(&body, json!({"missing": []}), usage).into_response();
    }
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

/// Whether `body` asks the judge rather than the describer, on either API.
fn judging(body: &Value) -> bool {
    let schema = |format: &Value| format["schema"]["properties"].get("missing").is_some();
    schema(&body["output_config"]["format"]) || schema(&body["text"]["format"])
}

/// An Anthropic answer of `out`.
fn answer(body: &Value, out: Value, usage: Value) -> Json<Value> {
    Json(json!({
        "model": body["model"],
        "stop_reason": "end_turn",
        "content": [{"type": "text", "text": out.to_string()}],
        "usage": usage,
    }))
}

async fn serve(mock: Arc<Mock>) -> String {
    serve_with(mock, messages).await
}

async fn serve_with<H, T>(mock: Arc<Mock>, handler: H) -> String
where
    H: axum::handler::Handler<T, Arc<Mock>>,
    T: 'static,
{
    let app = axum::Router::new()
        .route("/v1/messages", axum::routing::post(handler))
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
            "Space.grad_add (f g : Space → ℝ) (hf : Differentiable ℝ f) (hg : Differentiable ℝ g) : \
             Space.grad (f + g) = Space.grad f + Space.grad g",
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
    let client = Llm::Anthropic(Client::new(&serve(mock.clone()).await, "test-key".into()));
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
    // Its hypotheses `(hf : Differentiable ℝ f)` and `(hg : …)` are judged:
    // none left out.
    assert_eq!(ok.checks.missing_hypotheses, Some(Vec::new()));
    assert_eq!((ok.judge_calls, ok.redescribed), (1, false));
    assert!(ok.cost.unwrap() > 0.0);

    let requests = mock.requests.lock().unwrap().clone();
    assert!(requests.iter().all(|(key, _)| key == "test-key"));
    let body = &requests[0].1;
    assert_eq!(body["model"], "claude-haiku-5-5");
    assert_eq!(body["output_config"]["effort"], "low");
    assert_eq!(body["output_config"]["format"]["type"], "json_schema");
    assert_eq!(body["system"][1]["cache_control"]["type"], "ephemeral");
    // The overloaded first call was retried with the same body, and the one
    // declaration with hypotheses was judged.
    let judged = requests.iter().filter(|(_, b)| judging(b)).count();
    assert_eq!((requests.len() - judged, judged), (6, 1));
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
    let client = Llm::Anthropic(Client::new(&serve(mock.clone()).await, "bad-key".into()));
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

/// The OpenAI Responses API, answering like `messages` does.
async fn responses(
    State(mock): State<Arc<Mock>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    mock.requests.lock().unwrap().push((auth, body.clone()));
    let prompt = body["input"].as_str().unwrap_or_default();
    if judging(&body) {
        let out = json!({"missing": []}).to_string();
        return Json(json!({
            "model": body["model"],
            "status": "completed",
            "output": [{"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": out}]}],
            // None, so that the description's own are what it costs.
            "usage": {"input_tokens": 0, "output_tokens": 0},
        }))
        .into_response();
    }
    let name = prompt
        .lines()
        .find_map(|l| l.strip_prefix("name: "))
        .unwrap_or_default()
        .to_string();
    let usage = json!({
        "input_tokens": 1100, "input_tokens_details": {"cached_tokens": 1000},
        "output_tokens": 50, "output_tokens_details": {"reasoning_tokens": 10},
    });
    let content = if name == "Danger.thing" {
        json!([{"type": "refusal", "refusal": "I can't help with that."}])
    } else {
        let statement = if name == "Danger.blank" {
            String::new()
        } else {
            format!("What `{name}` says.")
        };
        let out = json!({
            "informal_name": format!("Description of {name}"),
            "statement_nl": statement,
            "statement_latex": "",
            "aliases": [],
            "synthetic_queries": ["a", "b", "c"],
        });
        json!([{"type": "output_text", "text": out.to_string()}])
    };
    Json(json!({
        "model": body["model"],
        "status": "completed",
        "output": [
            {"type": "reasoning", "summary": []},
            {"type": "message", "role": "assistant", "content": content},
        ],
        "usage": usage,
    }))
    .into_response()
}

#[tokio::test]
async fn describes_with_openai() {
    let mock = Arc::new(Mock::default());
    let app = axum::Router::new()
        .route("/v1/responses", axum::routing::post(responses))
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = Llm::OpenAi(openai::Client::new(&url, "test-key".into()));
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().to_str().unwrap()).unwrap();
    let c = corpus();
    let targets: Vec<&Decl> = c.decls.iter().collect();
    let cfg = Config {
        model: "gpt-6-luna".into(),
        ..config()
    };

    let outcomes = informalize::run(
        &client,
        &cfg,
        &store,
        &c,
        &targets,
        HashMap::new(),
        "1-test",
    )
    .await
    .unwrap();

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
    assert_eq!(
        (ok.usage.input_tokens, ok.usage.cache_read_input_tokens),
        (100, 1000)
    );
    // The judge asks the same API.
    assert_eq!(ok.judge_calls, 1);
    // 100 × $0.10 + 1000 × $0.01 + 50 × $0.50, per million.
    assert!((ok.cost.unwrap() - 45e-6).abs() < 1e-12, "{:?}", ok.cost);

    let requests = mock.requests.lock().unwrap().clone();
    assert!(requests.iter().all(|(auth, _)| auth == "Bearer test-key"));
    let body = &requests[0].1;
    assert_eq!(body["reasoning"]["effort"], "low");
    assert_eq!(body["store"], false);
    assert_eq!(body["text"]["format"]["type"], "json_schema");
    assert_eq!(body["text"]["format"]["strict"], true);
    assert!(body["instructions"].as_str().unwrap().contains("Glossary:"));

    let stored = store.load_all().await.unwrap();
    assert_eq!(stored.len(), 4);
    assert!(stored.values().any(|e| e.informal.source == "gpt-6-luna"));
}

#[tokio::test]
async fn embeds_through_an_openai_compatible_endpoint() {
    async fn embeddings(Json(body): Json<Value>) -> Json<Value> {
        assert_eq!(body["model"], "qwen3-embedding-8b");
        let n = body["input"].as_array().unwrap().len();
        // Out of order, and longer than the 512 dimensions kept.
        let data: Vec<Value> = (0..n)
            .rev()
            .map(|i| {
                let mut v = vec![0.0f32; 1024];
                v[i] = 2.0;
                json!({"index": i, "embedding": v})
            })
            .collect();
        Json(json!({"data": data}))
    }
    let app = axum::Router::new().route("/v1/embeddings", axum::routing::post(embeddings));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let spec = format!("openai+{url}/v1?model=qwen3-embedding-8b");
    let vectors = tokio::task::spawn_blocking(move || {
        let e = dense::Embedder::parse(&spec).unwrap();
        assert_eq!(e.model().unwrap(), "qwen3-embedding-8b");
        assert!(
            dense::Embedder::parse("openai+http://h/v1").is_err(),
            "no model given"
        );
        e.embed(&["a".to_string(), "b".to_string()]).unwrap()
    })
    .await
    .unwrap();
    assert_eq!(vectors.len(), 2);
    assert_eq!(vectors[0].len(), dense::DIMS);
    assert_eq!((vectors[0][0], vectors[1][1]), (1.0, 1.0));
}

/// A describer that leaves out `1 < b` until told it did, but for
/// `Real.logb_lt_logb`, which it never states; and a judge that finds `1 < b`
/// missing from a description of a signature with `(hb : 1 < b)` that
/// doesn't say it.
async fn logb(State(mock): State<Arc<Mock>>, Json(body): Json<Value>) -> Json<Value> {
    mock.requests
        .lock()
        .unwrap()
        .push((String::new(), body.clone()));
    let prompt = body["messages"][0]["content"].as_str().unwrap_or_default();
    let usage = json!({"input_tokens": 100, "output_tokens": 50});
    if judging(&body) {
        let description = prompt.split("<description>").nth(1).unwrap_or_default();
        let missing: &[&str] = if prompt.contains("(hb : 1 < b)") && !description.contains("1 < b")
        {
            &["1 < b"]
        } else {
            &[]
        };
        return answer(&body, json!({"missing": missing}), usage);
    }
    let told =
        prompt.contains("left out: `1 < b`") && !prompt.contains("name: Real.logb_lt_logb\n");
    let statement = if told {
        "For a base $1 < b$ and $0 < x \\le y$, $\\log_b x \\le \\log_b y$."
    } else {
        "For a positive base $b \\ne 1$ and $0 < x \\le y$, $\\log_b x \\le \\log_b y$."
    };
    let out = json!({
        "informal_name": "Monotonicity of the logarithm",
        "statement_nl": statement,
        "statement_latex": "\\log_b x \\le \\log_b y",
        "aliases": [],
        "synthetic_queries": ["a", "b", "c"],
    });
    answer(&body, out, usage)
}

fn logb_corpus() -> Corpus {
    let theorem = |name: &str, relation: &str, full: &str| Decl {
        library: "Mathlib".into(),
        module: "Mathlib.Analysis.SpecialFunctions.Log.Base".into(),
        type_pp_full: format!(
            "∀ {{b x y : Real}}, LT.lt 1 b → LT.lt 0 x → {full} x y → {full} (Real.logb b x) (Real.logb b y)"
        ),
        ..decl(
            name,
            "theorem",
            &format!(
                "{name} {{b x y : ℝ}} (hb : 1 < b) (h : 0 < x) (hxy : x {relation} y) : \
                 Real.logb b x {relation} Real.logb b y"
            ),
            &["Real.logb"],
        )
    };
    let mut decls = vec![
        Decl {
            type_pp_full: "Real → Real → Real".into(),
            ..decl("Real.logb", "def", "Real.logb (b x : ℝ) : ℝ", &[])
        },
        theorem("Real.logb_le_logb_of_le", "≤", "LE.le"),
        theorem("Real.logb_lt_logb", "<", "LT.lt"),
    ];
    corpus::compute_hashes(&mut decls);
    Corpus {
        manifest: Default::default(),
        decls,
        modules: Vec::new(),
    }
}

fn describe_requests(mock: &Mock) -> Vec<String> {
    let requests = mock.requests.lock().unwrap();
    (requests.iter())
        .filter(|(_, b)| !judging(b))
        .map(|(_, b)| b["messages"][0]["content"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn a_description_that_leaves_out_a_hypothesis_is_described_again() {
    let mock = Arc::new(Mock::default());
    let client = Llm::Anthropic(Client::new(
        &serve_with(mock.clone(), logb).await,
        "k".into(),
    ));
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().to_str().unwrap()).unwrap();
    let c = logb_corpus();
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

    let outcome = |name: &str| outcomes.iter().find(|o| o.name == name).unwrap();
    let fixed = outcome("Real.logb_le_logb_of_le");
    // The first description, then the second, each judged.
    assert_eq!((fixed.judge_calls, fixed.redescribed), (2, true));
    assert_eq!(fixed.checks.missing_hypotheses, Some(Vec::new()));
    // Four calls of 100 input and 50 output tokens at Haiku's prices.
    assert!(
        (fixed.cost.unwrap() - 4.0 * 35e-6).abs() < 1e-12,
        "{:?}",
        fixed.cost
    );
    let stubborn = outcome("Real.logb_lt_logb");
    assert_eq!((stubborn.judge_calls, stubborn.redescribed), (2, true));
    assert_eq!(
        stubborn.checks.missing_hypotheses,
        Some(vec!["1 < b".to_string()])
    );
    // `Real.logb (b x : ℝ)` has only variables: it isn't judged.
    assert_eq!(outcome("Real.logb").judge_calls, 0);
    // The judge reads the type with full names, where premises are arrows.
    let judged: Vec<String> = (mock.requests.lock().unwrap().iter())
        .filter(|(_, b)| judging(b))
        .map(|(_, b)| b["messages"][0]["content"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(judged.len(), 4);
    assert!(
        judged
            .iter()
            .all(|p| p.contains("<type>\n∀ {b x y : Real}, LT.lt 1 b → LT.lt 0 x →")),
        "{judged:#?}"
    );

    // Each was asked again once, told what it left out.
    let asked = describe_requests(&mock);
    assert_eq!(asked.len(), 5, "{asked:#?}");
    let again: Vec<&String> = asked.iter().filter(|p| p.contains("left out")).collect();
    assert_eq!(again.len(), 2);
    assert!(
        again
            .iter()
            .all(|p| p.contains("The previous description left out: `1 < b`.")),
        "{again:#?}"
    );

    let stored = store.load_all().await.unwrap();
    let by_name: HashMap<&str, _> = stored.values().map(|e| (e.name.as_str(), e)).collect();
    let fixed = by_name["Real.logb_le_logb_of_le"];
    assert!(fixed.informal.statement_nl.contains("1 < b"));
    let meta = fixed.meta.as_ref().unwrap();
    assert_eq!(meta["checks"]["missing_hypotheses"], json!([]));
    assert_eq!(meta["redescribed_for"], json!(["1 < b"]));
    // A second description the judge fails too isn't kept: the first is.
    let stubborn = by_name["Real.logb_lt_logb"];
    assert!(stubborn.informal.statement_nl.contains("$b \\ne 1$"));
    let meta = stubborn.meta.as_ref().unwrap();
    assert_eq!(meta["checks"]["missing_hypotheses"], json!(["1 < b"]));
    assert!(meta.get("redescribed_for").is_none());
}

#[tokio::test]
async fn validating_replaces_stored_descriptions_that_leave_out_hypotheses() {
    let mock = Arc::new(Mock::default());
    let client = Llm::Anthropic(Client::new(
        &serve_with(mock.clone(), logb).await,
        "k".into(),
    ));
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().to_str().unwrap()).unwrap();
    let c = logb_corpus();
    let stored = |name: &str, statement: &str| {
        let d = c.decls.iter().find(|d| d.name == name).unwrap();
        Entry {
            content_hash: d.content_hash.clone(),
            name: name.into(),
            informal: Informal {
                informal_name: "Monotonicity of the logarithm".into(),
                statement_nl: statement.into(),
                source: "lsv2-test".into(),
                ..Default::default()
            },
            synthetic_queries: Vec::new(),
            meta: None,
        }
    };
    let positive = "For any positive real number $b \\neq 1$, $\\log_b x \\le \\log_b y$.";
    let entries = [
        stored("Real.logb", "The logarithm of $x$ in base $b$."),
        stored("Real.logb_le_logb_of_le", positive),
        stored("Real.logb_lt_logb", positive),
    ];
    store.write_batch("0000-lsv2-test", &entries).await.unwrap();
    let described: HashMap<String, Informal> = entries
        .iter()
        .map(|e| (e.name.clone(), e.informal.clone()))
        .collect();
    let targets: Vec<&Decl> = c.decls.iter().filter(|d| d.name != "Real.logb").collect();

    let judged = informalize::validate(
        &client,
        &config(),
        &store,
        &c,
        &targets,
        described,
        "2-validate",
    )
    .await
    .unwrap();

    let names: Vec<&str> = judged.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["Real.logb_le_logb_of_le", "Real.logb_lt_logb"]);
    let (_, fixed) = &judged[0];
    assert_eq!(fixed.missing, Ok(vec!["1 < b".to_string()]));
    assert!(fixed.replaced && fixed.failure.is_none(), "{fixed:?}");
    assert_eq!((fixed.judge_calls, fixed.redescribed), (2, true));
    // Two judge calls and the second description: the stored one cost nothing.
    assert!(
        (fixed.cost.unwrap() - 3.0 * 35e-6).abs() < 1e-12,
        "{:?}",
        fixed.cost
    );
    let (_, kept) = &judged[1];
    assert!(!kept.replaced);
    assert_eq!(
        kept.failure.as_deref(),
        Some("the second description also leaves out `1 < b`")
    );
    // Only the stored descriptions' judging and what followed: no first description.
    assert_eq!(describe_requests(&mock).len(), 2);

    // The replacement is a batch of its own, which wins over the old one.
    let batch = std::fs::read(dir.path().join("informal/2-validate-0000.ndjson.zst")).unwrap();
    let lines = String::from_utf8(zstd::decode_all(&batch[..]).unwrap()).unwrap();
    assert_eq!(lines.lines().count(), 1, "{lines}");
    let all = store.load_all().await.unwrap();
    let by_name: HashMap<&str, &Entry> = all.values().map(|e| (e.name.as_str(), e)).collect();
    let fixed = by_name["Real.logb_le_logb_of_le"];
    assert!(fixed.informal.statement_nl.contains("1 < b"));
    assert_eq!(fixed.informal.source, "claude-haiku-5-5");
    let meta = fixed.meta.as_ref().unwrap();
    assert_eq!(meta["checks"]["missing_hypotheses"], json!([]));
    assert_eq!(meta["redescribed_for"], json!(["1 < b"]));
    assert_eq!(by_name["Real.logb_lt_logb"].informal.source, "lsv2-test");
}
