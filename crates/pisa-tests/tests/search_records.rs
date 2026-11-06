//! What workers record for the search service: each snapshot module's
//! declarations, and a log of what changed.

mod common;

use common::Cluster;
use pisa_protocol::api::SnapshotId;
use pisa_worker::catalog::{Catalog, Change};
use serde_json::{Value, json};

const CODE: &str = "import Foo\n\n/-- One is one. -/\ntheorem one : 1 = 1 := rfl\n\ndef two := 2";

async fn create(c: &Cluster, code: &str, labels: Value) -> Value {
    let (status, r) = c
        .post("/snapshots", json!({"code": code, "labels": labels}))
        .await;
    assert_eq!(status, 200, "{r}");
    assert!(r["snapshot"]["id"].is_string(), "{r}");
    r
}

/// The module id of a create's snapshot.
fn module(r: &Value) -> String {
    let name = r["snapshot"]["module"].as_str().unwrap();
    name.strip_prefix("PisaSnap.s").unwrap().to_string()
}

/// The entry of a create's snapshot, as the catalog names it.
fn entry(r: &Value) -> String {
    let id = r["snapshot"]["id"].as_str().unwrap();
    SnapshotId::parse(id).unwrap().entry
}

/// Every change logged so far, oldest first.
async fn changes(catalog: &Catalog) -> Vec<Change> {
    let page = catalog.changes(0, None, 1000).await.unwrap();
    page.into_iter().map(|logged| logged.change).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_create_records_declarations_and_a_change() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let catalog = Catalog::new(c.db.clone(), c.version("v"));
    let r = create(&c, CODE, json!({"run": "1"})).await;
    let m = module(&r);

    let decls = catalog.decls(&m).await.unwrap().expect("declarations");
    assert_eq!(decls.header, r["snapshot"]["header"].as_str().unwrap());
    assert!(!decls.dirty && !decls.verified);
    let names: Vec<&str> = decls.decls.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, ["one", "two"]);
    assert_eq!(decls.decls[0].kind, "theorem");
    assert_eq!(decls.decls[0].signature, "one : 1 = 1");
    assert_eq!(decls.decls[0].docstring.as_deref(), Some("One is one."));
    assert_eq!(decls.decls[1].docstring, None);

    let logged = changes(&catalog).await;
    assert_eq!(
        logged,
        [Change {
            entry: Some(entry(&r)),
            module: Some(m.clone()),
        }]
    );
}

/// `GET /snapshots/{id}`.
async fn info(c: &Cluster, entry: &str) -> Value {
    let r = c.client.get(c.url(&format!("/snapshots/{entry}"))).send();
    r.await.unwrap().json().await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_snapshot_lists_its_exports_with_their_descriptions() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let catalog = Catalog::new(c.db.clone(), c.version("v"));
    let r = create(&c, CODE, json!({})).await;
    // As search's leader records a description.
    let informal = json!({
        "informal_name": "One is one",
        "statement_nl": "One equals one.",
        "source": "test",
    });
    let record = json!({"informal": informal, "view": "v"});
    catalog
        .put_described(&module(&r), &[("one".into(), record)])
        .await
        .unwrap();

    let exports = &info(&c, r["snapshot"]["id"].as_str().unwrap()).await["exports"];
    assert_eq!(
        exports[0],
        json!({
            "name": "one",
            "kind": "theorem",
            "signature": "one : 1 = 1",
            "docstring": "One is one.",
            "description": {
                "title": "One is one",
                "text": "One equals one.",
                "aliases": [],
                "source": "test",
            },
        })
    );
    assert_eq!(exports[1]["name"], "two");
    assert!(exports[1].get("description").is_none(), "{exports}");
}

#[tokio::test(flavor = "multi_thread")]
async fn sorry_is_flagged() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let catalog = Catalog::new(c.db.clone(), c.version("v"));
    let r = create(&c, "import Foo\n\ntheorem t : 1 = 2 := sorry", json!({})).await;
    let decls = catalog.decls(&module(&r)).await.unwrap().unwrap();
    assert!(decls.dirty);
    assert!(decls.decls[0].uses_sorry);
}

#[tokio::test(flavor = "multi_thread")]
async fn relabels_and_deletes_are_logged_and_deletes_clean_up() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let catalog = Catalog::new(c.db.clone(), c.version("v"));
    let r = create(&c, CODE, json!({})).await;
    let m = module(&r);
    assert!(
        catalog
            .put_described(&m, &[("one".into(), json!({}))])
            .await
            .unwrap()
    );
    assert_eq!(catalog.described(&m).await.unwrap().len(), 1);

    let path = format!("/snapshots/{}", r["snapshot"]["id"].as_str().unwrap());
    let patched = c
        .client
        .patch(c.url(&path))
        .json(&json!({"labels": {"stage": "done"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(patched.status(), 200);
    let deleted = c.client.delete(c.url(&path)).send().await.unwrap();
    assert_eq!(deleted.status(), 200);

    let touched = Change {
        entry: Some(entry(&r)),
        module: Some(m.clone()),
    };
    assert_eq!(
        changes(&catalog).await,
        [touched.clone(), touched.clone(), touched]
    );
    assert!(catalog.decls(&m).await.unwrap().is_none());
    assert!(catalog.described(&m).await.unwrap().is_empty());
    // The module is gone, so nothing more can be described for it.
    assert!(
        !catalog
            .put_described(&m, &[("one".into(), json!({}))])
            .await
            .unwrap()
    );
}
