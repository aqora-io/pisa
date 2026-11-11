//! The API and search in one server: search that can't serve, or isn't
//! configured, leaves the rest of the API alone.

use pisa_db::db::Db;
use pisa_router::RouterConfig;
use pisa_search::server::SearchArgs;
use pisa_server::{ServerConfig, start_on};
use serde_json::{Value, json};

fn config(search: SearchArgs) -> ServerConfig {
    ServerConfig {
        router: RouterConfig {
            http: "127.0.0.1:0".parse().unwrap(),
            max_wait_secs: 1,
            max_retries: 0,
            default_version: None,
            alias: Vec::new(),
            queue: "pisa".into(),
            allowed_axioms: Vec::new(),
            cors_origins: Vec::new(),
        },
        database_url: String::new(),
        drain_timeout_secs: 1,
        search,
    }
}

async fn get(url: &str) -> (u16, Value) {
    let r = reqwest::get(url).await.unwrap();
    (r.status().as_u16(), r.json().await.unwrap())
}

async fn post(url: &str, body: Value) -> (u16, Value) {
    let r = reqwest::Client::new()
        .post(url)
        .json(&body)
        .send()
        .await
        .unwrap();
    (r.status().as_u16(), r.json().await.unwrap())
}

#[tokio::test]
async fn search_that_isnt_ready_leaves_the_api_up() {
    // Nothing listens on the discard port.
    let search = SearchArgs {
        qdrant: Some("http://127.0.0.1:9".into()),
        env_id: Some("e".into()),
        ..Default::default()
    };
    let server = start_on(config(search), Db::memory().await).await.unwrap();
    let url = |path: &str| format!("http://{}{path}", server.running.http_addr);

    let (status, health) = get(&url("/health")).await;
    assert_eq!(status, 200, "{health}");
    assert_eq!(health["search"]["ready"], false, "{health}");
    assert_eq!(health["search"]["qdrant"], false, "{health}");

    let (status, body) = post(&url("/search"), json!({"queries": [{"text": "x"}]})).await;
    assert_eq!(status, 503, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("isn't ready"),
        "{body}"
    );

    // Checks still go to the queue: with no worker, they wait their second.
    let (status, body) = post(&url("/check"), json!({"code": "#check Nat"})).await;
    assert_eq!((status, &body["error"]["kind"]), (429, &json!("capacity")));
}

#[tokio::test]
async fn without_qdrant_there_is_no_search() {
    let server = start_on(config(SearchArgs::default()), Db::memory().await)
        .await
        .unwrap();
    let url = |path: &str| format!("http://{}{path}", server.running.http_addr);
    let (status, health) = get(&url("/health")).await;
    assert_eq!(status, 200);
    assert_eq!(
        health,
        json!({
            "status": "ok",
            "workers": 0,
            "unlimited_workers": [],
            "sweeps_refused": [],
            "search": null
        })
    );
    let searches = [
        post(&url("/search"), json!({"queries": []})).await,
        post(&url("/resolve"), json!({"names": []})).await,
        get(&url("/declarations/Nat.add_comm")).await,
    ];
    for (status, body) in searches {
        assert_eq!(status, 404, "{body}");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("doesn't search"),
            "{body}"
        );
    }
    let (status, _) = get(&url("/nope")).await;
    assert_eq!(status, 404);
    // The spec covers search all the same.
    let (status, spec) = get(&url("/openapi.json")).await;
    assert_eq!((status, spec), (200, pisa_server::openapi()));
}

/// A preflight for `POST /check` from `origin`, and the origin it allows.
async fn preflight(url: &str, origin: &str) -> Option<String> {
    let r = reqwest::Client::new()
        .request(reqwest::Method::OPTIONS, url)
        .header("origin", origin)
        .header("access-control-request-method", "POST")
        .header("access-control-request-headers", "content-type")
        .send()
        .await
        .unwrap();
    r.headers()
        .get("access-control-allow-origin")
        .map(|v| v.to_str().unwrap().to_string())
}

#[tokio::test]
async fn browsers_may_call_only_from_the_configured_origins() {
    let mut cfg = config(SearchArgs::default());
    cfg.router.cors_origins = vec!["http://app.example".into(), " ".into()];
    let server = start_on(cfg, Db::memory().await).await.unwrap();
    let url = |path: &str| format!("http://{}{path}", server.running.http_addr);
    assert_eq!(
        preflight(&url("/check"), "http://app.example")
            .await
            .as_deref(),
        Some("http://app.example")
    );
    assert_eq!(
        preflight(&url("/check"), "http://other.example").await,
        None
    );
    // Every route has it, search's and /health too.
    let r = reqwest::Client::new()
        .get(url("/health"))
        .header("origin", "http://app.example")
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.headers()["access-control-allow-origin"],
        "http://app.example"
    );

    let server = start_on(config(SearchArgs::default()), Db::memory().await)
        .await
        .unwrap();
    let url = format!("http://{}/check", server.running.http_addr);
    assert_eq!(preflight(&url, "http://app.example").await, None);
}
