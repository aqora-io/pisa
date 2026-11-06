//! Listing snapshots by label, page by page, and editing their labels.

mod common;

use common::Cluster;
use serde_json::{Value, json};

const ROOT: &str = "import Foo\n\ntheorem base : 1 = 1 := rfl";

/// Creates a snapshot of `ROOT` with `labels` and returns the response.
async fn create(c: &Cluster, labels: Value) -> Value {
    let (status, r) = c
        .post("/snapshots", json!({"code": ROOT, "labels": labels}))
        .await;
    assert_eq!(status, 200, "{r}");
    assert!(r["snapshot"]["id"].is_string(), "{r}");
    r
}

/// `GET /snapshots` with `query`, encoded by reqwest's `Url`.
async fn list(c: &Cluster, query: &[(&str, &str)]) -> (u16, Value) {
    let mut url = reqwest::Url::parse(&c.url("/snapshots")).unwrap();
    url.query_pairs_mut().extend_pairs(query);
    let r = c.client.get(url).send().await.unwrap();
    (r.status().as_u16(), r.json().await.unwrap())
}

/// The ids of the snapshots a successful listing returns.
async fn listed(c: &Cluster, query: &[(&str, &str)]) -> Vec<String> {
    let (status, r) = list(c, query).await;
    assert_eq!(status, 200, "{query:?}: {r}");
    ids(&r)
}

fn ids(list: &Value) -> Vec<String> {
    let snapshots = list["snapshots"].as_array().unwrap();
    snapshots
        .iter()
        .map(|s| s["id"].as_str().unwrap().to_string())
        .collect()
}

fn entry(r: &Value) -> &str {
    r["snapshot"]["id"].as_str().unwrap()
}

/// `GET /snapshots/{id}`.
async fn info(c: &Cluster, entry: &str) -> Value {
    let r = c
        .client
        .get(c.url(&format!("/snapshots/{entry}")))
        .send()
        .await;
    r.unwrap().json().await.unwrap()
}

async fn patch(c: &Cluster, entry: &str, body: Value) -> (u16, Value) {
    let path = format!("/snapshots/{entry}");
    let r = c
        .client
        .patch(c.url(&path))
        .json(&body)
        .send()
        .await
        .unwrap();
    (r.status().as_u16(), r.json().await.unwrap())
}

async fn delete(c: &Cluster, snapshot: &Value) {
    let path = format!("/snapshots/{}", entry(snapshot));
    let r = c.client.delete(c.url(&path)).send().await.unwrap();
    assert_eq!(r.status(), 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn lists_entries_newest_first_with_pages() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    for i in 0..5 {
        create(&c, json!({"n": i.to_string()})).await;
    }
    let n = |list: &Value| -> Vec<String> {
        let snapshots = list["snapshots"].as_array().unwrap();
        snapshots
            .iter()
            .map(|e| e["labels"]["n"].as_str().unwrap().to_string())
            .collect()
    };

    let (status, page) = list(&c, &[("limit", "2")]).await;
    assert_eq!(status, 200, "{page}");
    assert_eq!(n(&page), ["4", "3"]);
    let (_, page) = list(
        &c,
        &[("limit", "2"), ("after", page["next"].as_str().unwrap())],
    )
    .await;
    assert_eq!(n(&page), ["2", "1"]);
    let (_, page) = list(
        &c,
        &[("limit", "2"), ("after", page["next"].as_str().unwrap())],
    )
    .await;
    assert_eq!(n(&page), ["0"]);
    assert!(page.get("next").is_none(), "{page}");

    let (_, all) = list(&c, &[]).await;
    assert_eq!(n(&all), ["4", "3", "2", "1", "0"]);
    assert!(all.get("next").is_none(), "{all}");

    // A snapshot as everywhere, without the code, the check or the exports.
    let listed = &all["snapshots"][0];
    let mut info = info(&c, &ids(&all)[0]).await;
    for field in ["code", "check", "exports"] {
        assert!(listed.get(field).is_none(), "{field}: {listed}");
        info.as_object_mut().unwrap().remove(field);
    }
    assert_eq!(*listed, info);
}

#[tokio::test(flavor = "multi_thread")]
async fn filters_are_anded() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let first = create(&c, json!({"p": "x", "k": "a"})).await;
    let second = create(&c, json!({"p": "x", "k": "b"})).await;
    create(&c, json!({"p": "y", "k": "a"})).await;

    let both = listed(&c, &[("label", "p:x"), ("label", "k:a")]).await;
    assert_eq!(both, [entry(&first)]);
    let p = listed(&c, &[("label", "p:x")]).await;
    assert_eq!(p, [entry(&second), entry(&first)]);

    // The first label's newest entry fails the second filter: the scan goes on
    // to the next page of its range for a match.
    let query = [("label", "k:a"), ("label", "p:x"), ("limit", "1")];
    let (_, page) = list(&c, &query).await;
    assert_eq!(ids(&page), [entry(&first)]);
    let next = page["next"].as_str().unwrap();
    let (_, page) = list(&c, &[query[0], query[1], query[2], ("after", next)]).await;
    assert_eq!(ids(&page), Vec::<String>::new());
    assert!(page.get("next").is_none(), "{page}");
}

#[tokio::test(flavor = "multi_thread")]
async fn patch_sets_and_removes_labels() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let created = create(&c, json!({"k": "a", "draft": "1"})).await;
    let e = entry(&created);

    let body = json!({"labels": {"stage": "done", "draft": null}});
    let (status, r) = patch(&c, e, body).await;
    assert_eq!(status, 200, "{r}");
    assert_eq!(r["id"], e);
    assert_eq!(r["module"], created["snapshot"]["module"]);
    assert_eq!(r["labels"], json!({"k": "a", "stage": "done"}));
    assert!(listed(&c, &[("label", "draft:1")]).await.is_empty());
    assert_eq!(listed(&c, &[("label", "stage:done")]).await, [e]);
    assert_eq!(listed(&c, &[("label", "k:a")]).await, [e]);
    assert_eq!(
        info(&c, e).await["labels"],
        json!({"k": "a", "stage": "done"})
    );

    let (status, r) = patch(&c, e, json!({"labels": {"bad key": "x"}})).await;
    assert_eq!(
        (status, &r["error"]["kind"]),
        (400, &json!("bad_request")),
        "{r}"
    );
    assert_eq!(
        listed(&c, &[("label", "stage:done")]).await,
        [e],
        "unchanged"
    );
    let (status, r) = patch(&c, &"0".repeat(32), json!({"labels": {"k": "c"}})).await;
    assert_eq!(
        (status, &r["error"]["kind"]),
        (404, &json!("not_found")),
        "{r}"
    );
}

/// Review Focus 4: a patch that removes a label the snapshot doesn't have,
/// or names none, changes nothing, and one that would break the label rules
/// is refused and changes nothing either.
#[tokio::test(flavor = "multi_thread")]
async fn a_patch_of_absent_or_no_labels_changes_nothing() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let created = create(&c, json!({"k": "a"})).await;
    let e = entry(&created);
    let labels = json!({"k": "a"});

    for body in [json!({"labels": {"absent": null}}), json!({"labels": {}})] {
        let (status, r) = patch(&c, e, body.clone()).await;
        assert_eq!(status, 200, "{body}: {r}");
        assert_eq!(r["labels"], labels, "{body}: {r}");
        assert_eq!(info(&c, e).await["labels"], labels, "{body}");
    }

    let long = "k".repeat(129);
    let (status, r) = patch(&c, e, json!({"labels": {long: "x"}})).await;
    assert_eq!(
        (status, &r["error"]["kind"]),
        (400, &json!("bad_request")),
        "{r}"
    );
    assert_eq!(info(&c, e).await["labels"], labels, "unchanged");
}

#[tokio::test(flavor = "multi_thread")]
async fn listings_are_per_version() {
    let c = Cluster::start(&[("w0", "a", 1), ("w1", "b", 1)]).await;
    let mut created = Vec::new();
    for version in ["a", "b"] {
        let body = json!({"code": ROOT, "version": version});
        let (status, r) = c.post("/snapshots", body).await;
        assert_eq!(status, 200, "{r}");
        created.push(r);
    }
    assert_eq!(listed(&c, &[("version", "a")]).await, [entry(&created[0])]);
    assert_eq!(listed(&c, &[("version", "b")]).await, [entry(&created[1])]);
}

#[tokio::test(flavor = "multi_thread")]
async fn deleted_entries_leave_listings() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let kept = create(&c, json!({"k": "a"})).await;
    let gone = create(&c, json!({"k": "a", "p": "x"})).await;
    delete(&c, &gone).await;

    assert_eq!(listed(&c, &[]).await, [entry(&kept)]);
    assert_eq!(listed(&c, &[("label", "k:a")]).await, [entry(&kept)]);
    assert!(listed(&c, &[("label", "p:x")]).await.is_empty());

    delete(&c, &kept).await;
    assert!(listed(&c, &[]).await.is_empty());
    let rows = c.stored_rows("v").await;
    assert!(rows.iter().all(|r| r[0] != "entries"), "{rows:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn limits_are_bounded() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    for query in [
        ("limit", "0"),
        ("limit", "1001"),
        ("limit", "ten"),
        ("label", "nocolon"),
        ("label", "bad key:x"),
        ("after", "xyz"),
    ] {
        let (status, r) = list(&c, &[query]).await;
        assert_eq!(
            (status, &r["error"]["kind"]),
            (400, &json!("bad_request")),
            "{query:?}: {r}"
        );
    }
    assert!(listed(&c, &[("limit", "1000")]).await.is_empty());

    let filters: Vec<String> = (0..65).map(|i| format!("k{i}:v")).collect();
    let query = |n: usize| -> Vec<(&str, &str)> {
        filters[..n].iter().map(|f| ("label", f.as_str())).collect()
    };
    assert!(listed(&c, &query(64)).await.is_empty());
    let (status, r) = list(&c, &query(65)).await;
    assert_eq!(
        (status, &r["error"]["kind"]),
        (400, &json!("bad_request")),
        "{r}"
    );
}

/// Review Focus 1: a value with the characters a query string and a filter
/// treat specially matches exactly.
#[tokio::test(flavor = "multi_thread")]
async fn labels_with_special_characters_round_trip() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let created = create(&c, json!({})).await;
    let (status, r) = patch(&c, entry(&created), json!({"labels": {"k": "a:b c&d=é"}})).await;
    assert_eq!(status, 200, "{r}");
    assert_eq!(r["labels"], json!({"k": "a:b c&d=é"}));

    assert_eq!(
        listed(&c, &[("label", "k:a:b c&d=é")]).await,
        [entry(&created)]
    );
    assert!(listed(&c, &[("label", "k:a:b")]).await.is_empty());
}

/// Review Focus 2: a cursor is a position, not an entry that must still exist.
#[tokio::test(flavor = "multi_thread")]
async fn a_cursor_survives_deleting_its_entry() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let mut created = Vec::new();
    for i in 0..3 {
        created.push(create(&c, json!({"n": i.to_string()})).await);
    }
    let (_, page) = list(&c, &[("limit", "1")]).await;
    assert_eq!(ids(&page), [entry(&created[2])]);
    delete(&c, &created[2]).await;

    let next = page["next"].as_str().unwrap();
    let (_, page) = list(&c, &[("limit", "1"), ("after", next)]).await;
    assert_eq!(ids(&page), [entry(&created[1])]);
}

/// Review Focus 3: an entry has one value per key, so two values of the same
/// key match nothing, and the same filter twice is the filter once.
#[tokio::test(flavor = "multi_thread")]
async fn the_same_key_twice_matches_nothing() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let a = create(&c, json!({"k": "a"})).await;
    create(&c, json!({"k": "b"})).await;

    let (status, r) = list(&c, &[("label", "k:a"), ("label", "k:b")]).await;
    assert_eq!(status, 200, "{r}");
    assert_eq!(r["snapshots"], json!([]));
    let twice = listed(&c, &[("label", "k:a"), ("label", "k:a")]).await;
    assert_eq!(twice, listed(&c, &[("label", "k:a")]).await);
    assert_eq!(twice, [entry(&a)]);
}
