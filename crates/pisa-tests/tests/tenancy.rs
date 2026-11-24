//! Tenancy by labels: a request's scope, the `Pisa-Scope` header, labels
//! what it creates, and bounds what it names or reaches, which is a 404
//! outside the scope. A request without one is bounded by nothing.

mod common;

use common::Cluster;
use reqwest::Method;
use serde_json::{Value, json};

const A: &str = "user=a";
const B: &str = "user=b";
const SPEC: &str = "import Foo\n\ntheorem t : 1 = 1 := sorry";
const PROOF: &str = "import Foo\n\ntheorem t : 1 = 1 := rfl";

/// `method` on `path` in `scope` (none for ""), with `body` if given.
async fn send(
    c: &Cluster,
    scope: &str,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let mut req = c.client.request(method, c.url(path));
    if !scope.is_empty() {
        req = req.header("pisa-scope", scope);
    }
    if let Some(body) = body {
        req = req.json(&body);
    }
    let r = req.send().await.unwrap();
    let status = r.status().as_u16();
    let text = r.text().await.unwrap();
    let body = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or(Value::String(text))
    };
    (status, body)
}

async fn post(c: &Cluster, scope: &str, path: &str, body: Value) -> (u16, Value) {
    send(c, scope, Method::POST, path, Some(body)).await
}

async fn get(c: &Cluster, scope: &str, path: &str) -> (u16, Value) {
    send(c, scope, Method::GET, path, None).await
}

async fn create(c: &Cluster, scope: &str, body: Value) -> Value {
    let (status, r) = post(c, scope, "/snapshots", body).await;
    assert_eq!(status, 200, "{r}");
    assert!(r["snapshot"].is_object(), "{r}");
    r
}

fn id(r: &Value) -> &str {
    r["snapshot"]["id"].as_str().unwrap()
}

fn listed(r: &Value) -> Vec<&str> {
    let snapshots = r["snapshots"].as_array().unwrap();
    snapshots
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_scope_labels_what_it_creates_and_bounds_what_it_reads() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let mine = create(&c, A, json!({"code": SPEC, "labels": {"run": "1"}})).await;
    assert_eq!(mine["snapshot"]["labels"], json!({"run": "1", "user": "a"}));
    let path = format!("/snapshots/{}", id(&mine));

    assert_eq!(listed(&get(&c, A, "/snapshots").await.1), [id(&mine)]);
    assert!(listed(&get(&c, B, "/snapshots").await.1).is_empty());
    assert_eq!(
        get(&c, "", "/snapshots").await.1["snapshots"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // Everything that names it from elsewhere finds nothing.
    assert_eq!(get(&c, B, &path).await.0, 404);
    let relabel = json!({"labels": {"run": "2"}});
    assert_eq!(
        send(&c, B, Method::PATCH, &path, Some(relabel.clone()))
            .await
            .0,
        404
    );
    assert_eq!(
        post(&c, B, &format!("{path}/verify"), json!({})).await.0,
        404
    );
    assert_eq!(get(&c, B, &format!("{path}/dependents")).await.0, 404);
    let replacing = json!({"code": PROOF, "replaces": {"snapshot_id": id(&mine)}});
    let (_, r) = post(&c, B, "/snapshots", replacing.clone()).await;
    assert_eq!(r["check"]["error"]["kind"], "not_found", "{r}");
    assert_eq!(send(&c, B, Method::DELETE, &path, None).await.0, 404);
    // Its scope's label can't change; the rest can.
    let (status, r) = send(
        &c,
        A,
        Method::PATCH,
        &path,
        Some(json!({"labels": {"user": null}})),
    )
    .await;
    assert_eq!(status, 400, "{r}");
    assert_eq!(
        send(&c, A, Method::PATCH, &path, Some(relabel)).await.0,
        200
    );
    // A create can't label itself out of its scope.
    let (status, r) = post(
        &c,
        A,
        "/snapshots",
        json!({"code": PROOF, "labels": {"user": "b"}}),
    )
    .await;
    assert!(
        status == 400 || r["check"]["error"]["kind"] == "bad_request",
        "{r}"
    );
    // A replacement in the scope is stamped too.
    let proof = create(&c, A, replacing).await;
    assert_eq!(proof["snapshot"]["labels"]["user"], "a");
    // A header that isn't a scope is a 400.
    assert_eq!(get(&c, "user", "/snapshots").await.0, 400);
}

#[tokio::test(flavor = "multi_thread")]
async fn keys_names_and_imports_keep_to_their_scope() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let lemma = "import Foo\n\ntheorem lemma1 : 1 = 1 := rfl";
    let a = create(&c, A, json!({"code": lemma, "key": "k", "name": "lemma"})).await;
    // The same key and name are another tenant's to take.
    let b = create(
        &c,
        B,
        json!({"code": format!("{lemma}\n-- b"), "key": "k", "name": "lemma"}),
    )
    .await;
    assert_ne!(id(&a), id(&b));
    let again = create(&c, A, json!({"code": lemma, "key": "k"})).await;
    assert_eq!(id(&again), id(&a));

    // A name resolves in the scope it is asked in.
    let uses = "import PisaRun.lemma\n\nneeds lemma1\ntheorem u : 1 = 1 := lemma1";
    let (_, r) = post(&c, A, "/check", json!({"code": uses})).await;
    assert_eq!(r["runs"][0]["snapshot_id"], id(&a), "{r}");
    let (_, r) = post(&c, B, "/check", json!({"code": uses})).await;
    assert_eq!(r["runs"][0]["snapshot_id"], id(&b), "{r}");
    let (_, r) = post(&c, "user=c", "/check", json!({"code": uses})).await;
    assert_eq!(r["error"]["kind"], "not_found", "{r}");
    // So does a run read by its name, and one is named in its scope only.
    assert_eq!(get(&c, A, "/runs/lemma").await.1["snapshot_id"], id(&a));
    assert_eq!(get(&c, B, "/runs/lemma").await.1["snapshot_id"], id(&b));
    assert_eq!(get(&c, "user=c", "/runs/lemma").await.0, 404);
    let named = json!({"snapshot_id": id(&a)});
    let (status, _) = send(&c, B, Method::PUT, "/runs/mine", Some(named.clone())).await;
    assert_eq!(status, 404);
    assert_eq!(
        send(&c, A, Method::PUT, "/runs/mine", Some(named)).await.0,
        200
    );
    assert_eq!(get(&c, A, "/runs/lemma").await.0, 404);
    assert_eq!(get(&c, B, "/runs/lemma").await.0, 200);
    assert_eq!(get(&c, B, "/runs/mine").await.0, 404);

    // A module by its hash, only from a scope with a snapshot of it.
    let by_hash = format!(
        "{}\n\nneeds lemma1",
        a["snapshot"]["header"].as_str().unwrap()
    );
    let (_, r) = post(&c, A, "/check", json!({"code": by_hash})).await;
    assert_eq!(r["ok"], true, "{r}");
    let (_, r) = post(&c, B, "/check", json!({"code": by_hash})).await;
    assert_eq!(r["error"]["kind"], "not_found", "{r}");
    let (_, r) = post(&c, B, "/snapshots", json!({"code": by_hash})).await;
    assert_eq!(r["check"]["error"]["kind"], "not_found", "{r}");
    let (_, r) = post(&c, "", "/check", json!({"code": by_hash})).await;
    assert_eq!(r["ok"], true, "without a scope: {r}");
    // An open document's module, never.
    let doc = "import PisaDocs.D0123\n\ntheorem v : True := trivial";
    let (_, r) = post(&c, "", "/check", json!({"code": doc})).await;
    assert_eq!(r["error"]["kind"], "bad_request", "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn documents_sessions_states_and_jobs_keep_to_their_scope() {
    let c = Cluster::start_beam(&[("w0", "v", 1)]).await;
    let code = "theorem t : 1 = 1 := by\n  sorry";
    let (status, doc) = post(&c, A, "/documents", json!({"code": code})).await;
    assert_eq!(status, 200, "{doc}");
    let path = format!("/documents/{}", doc["id"].as_str().unwrap());
    let goals = json!({"at": [{"line": 2}]});
    assert_eq!(
        post(&c, B, &format!("{path}/goals"), goals.clone()).await.0,
        404
    );
    assert_eq!(post(&c, A, &format!("{path}/goals"), goals).await.0, 200);
    let edit = json!({"code": code, "revision": 1});
    assert_eq!(send(&c, B, Method::PATCH, &path, Some(edit)).await.0, 404);
    assert_eq!(send(&c, B, Method::DELETE, &path, None).await.0, 404);
    assert_eq!(send(&c, A, Method::DELETE, &path, None).await.0, 204);

    let (status, s) = post(&c, A, "/sessions", json!({"code": code})).await;
    assert_eq!(status, 200, "{s}");
    let state = s["states"][0]["id"].as_str().unwrap();
    let tactic = format!("/states/{state}/tactic");
    assert_eq!(post(&c, B, &tactic, json!({"tactic": "rfl"})).await.0, 404);
    assert_eq!(post(&c, A, &tactic, json!({"tactic": "rfl"})).await.0, 200);
    let check = format!("/sessions/{}/check", s["id"].as_str().unwrap());
    let done = json!({"code": "theorem t : 1 = 1 := by\n  rfl"});
    assert_eq!(post(&c, B, &check, done.clone()).await.0, 404);
    let (status, r) = post(&c, A, &check, done).await;
    assert_eq!((status, &r["ok"]), (200, &json!(true)), "{r}");

    // A job's token, read and cancelled in its scope only.
    let r = c
        .client
        .post(c.url("/check"))
        .header("prefer", "respond-async")
        .header("pisa-scope", A)
        .json(&json!({"code": "SLEEP 300\n#check Nat"}))
        .send()
        .await
        .unwrap();
    let job: Value = r.json().await.unwrap();
    let job = format!("/jobs/{}", job["id"].as_str().unwrap());
    assert_eq!(get(&c, B, &job).await.0, 404);
    assert_eq!(send(&c, B, Method::DELETE, &job, None).await.0, 404);
    let (status, r) = get(&c, A, &format!("{job}?wait=30")).await;
    let (status, r) = common::job_answer(status, r);
    assert_eq!((status, &r["ok"]), (200, &json!(true)), "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn runs_keep_to_their_scope() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let spec = create(&c, A, json!({"code": SPEC})).await;
    let replaces = json!({"snapshot_id": id(&spec)});
    create(&c, A, json!({"code": PROOF, "replaces": replaces})).await;
    let against = format!("against={}", id(&spec));

    let (status, r) = get(&c, A, &format!("/snapshots/audit?{against}")).await;
    assert_eq!(status, 200, "{r}");
    assert_eq!(
        get(&c, B, &format!("/snapshots/audit?{against}")).await.0,
        404
    );
    assert_eq!(
        get(&c, B, &format!("/snapshots/export?{against}")).await.0,
        404
    );
    let (status, r) = post(&c, B, "/snapshots/settle", json!({"against": [id(&spec)]})).await;
    assert_eq!(status, 404, "{r}");
    let (status, r) = post(&c, B, "/snapshots/accept", json!({"against": [id(&spec)]})).await;
    assert_eq!(status, 404, "{r}");

    // An acceptance is read in its scope only.
    let (status, verdict) = post(&c, A, "/snapshots/accept", json!({"against": [id(&spec)]})).await;
    assert_eq!(status, 200, "{verdict}");
    let stored = format!("/snapshots/accept/{}", verdict["id"].as_str().unwrap());
    assert_eq!(get(&c, A, &stored).await.0, 200);
    assert_eq!(get(&c, B, &stored).await.0, 404);
    let by_spec = format!("/snapshots/accept?against={}", id(&spec));
    assert_eq!(
        get(&c, A, &by_spec).await.1["acceptances"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let (_, r) = get(&c, B, &by_spec).await;
    assert!(
        r["acceptances"].as_array().is_none_or(|a| a.is_empty()),
        "{r}"
    );
}
