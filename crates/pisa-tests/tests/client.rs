//! The Rust client against an in-process cluster.

mod common;

use std::collections::BTreeMap;

use common::Cluster;
use pisa_client::{Client, Error, ErrorKind, SnapshotList, SnapshotOptions};

#[tokio::test(flavor = "multi_thread")]
async fn client_covers_checks_and_sessions() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let client = Client::new(&c.url("/")).unwrap().with_timeout(5);
    client.health().await.unwrap();

    let r = client
        .check("import Foo\ntheorem t : 1 = 1 := rfl")
        .await
        .unwrap();
    assert!(r.ok, "{r:?}");

    let results = client
        .check_batch([("a", "#check Nat"), ("b", "theorem t : False := sorry")])
        .await
        .unwrap();
    assert_eq!(results[0].id.as_deref(), Some("a"));
    assert!(!results[1].ok);
    assert_eq!(results[1].reasons, ["axiom: t uses sorryAx"]);

    let s = client
        .start_session("theorem t : 1 = 1 := by sorry")
        .await
        .unwrap();
    let step = client.tactic(&s.states[0].state, "simp").await.unwrap();
    assert!(!step.complete);
    let done = client.tactic(&step.state, "rfl").await.unwrap();
    assert!(done.complete);
    let err = client.tactic(&step.state, "fail").await.unwrap_err();
    assert_eq!(err.kind(), Some(ErrorKind::Lean));
    let verdict = client
        .check_session(&s.session, "theorem t : 1 = 1 := by rfl")
        .await
        .unwrap();
    assert!(verdict.ok, "{verdict:?}");

    match client.tactic("not-a-key", "rfl").await {
        Err(Error::Server { status, kind, .. }) => {
            assert_eq!((status, kind), (400, ErrorKind::BadRequest))
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn client_covers_snapshots() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let client = Client::new(&c.url("/")).unwrap().with_version("v");
    let base = client
        .create_snapshot("import Foo\ntheorem base : 1 = 1 := rfl")
        .await
        .unwrap();
    let name = base.snapshot.unwrap();
    let entry = base.entry.unwrap();
    let header = base.header.unwrap();
    assert!(base.result.ok, "{:?}", base.result);

    let r = client
        .check(&format!("{header}\nneeds base"))
        .await
        .unwrap();
    assert!(r.ok, "{r:?}");
    let info = client.snapshot(&entry).await.unwrap();
    assert_eq!(
        (info.entry.as_str(), info.snapshot.as_str()),
        (entry.as_str(), name.as_str())
    );
    assert_eq!(info.header, header);
    assert!(!info.dirty);
    assert!(!info.verified);

    let verified = client.verify_snapshot(&entry).await.unwrap();
    assert!(verified.verified);
    assert_eq!(verified.entry, entry);
    assert_eq!(verified.checked, std::slice::from_ref(&name));
    assert!(verified.failure.is_none());
    assert!(client.snapshot(&entry).await.unwrap().verified);
    let forged = client
        .create_snapshot_with_verify("import Foo\ntheorem forge : 1 = 1 := rfl", true)
        .await
        .unwrap();
    assert!(forged.snapshot.is_none(), "{forged:?}");
    let opted_out = client
        .create_snapshot_with_verify("import Foo\nmacro \"m\" : term => `(1)", false)
        .await
        .unwrap();
    assert!(!opted_out.verified, "{opted_out:?}");

    let removed = client.delete_snapshot(&entry).await.unwrap();
    assert_eq!(removed, std::slice::from_ref(&name));
    let err = client.snapshot(&entry).await.unwrap_err();
    assert_eq!(err.kind(), Some(ErrorKind::NotFound));
}

#[tokio::test(flavor = "multi_thread")]
async fn client_passes_labels() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let client = Client::new(&c.url("/")).unwrap().with_version("v");
    let opts = SnapshotOptions {
        labels: BTreeMap::from([("k".to_string(), "v".to_string())]),
        key: Some("run-1".to_string()),
        ..Default::default()
    };
    let code = "import Foo\ntheorem base : 1 = 1 := rfl";
    let created = client.create_snapshot_with(code, &opts).await.unwrap();
    assert_eq!(created.labels, opts.labels);
    let entry = created.entry.unwrap();
    let info = client.snapshot(&entry).await.unwrap();
    assert_eq!(info.labels, opts.labels);

    let again = client.create_snapshot_with(code, &opts).await.unwrap();
    assert_eq!(again.entry, Some(entry));
}

#[tokio::test(flavor = "multi_thread")]
async fn client_lists_and_labels_snapshots() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let client = Client::new(&c.url("/")).unwrap().with_version("v");
    let code = "import Foo\ntheorem base : 1 = 1 := rfl";
    let labels = |value: &str| BTreeMap::from([("k".to_string(), value.to_string())]);
    let mut created = Vec::new();
    for value in ["v", "w"] {
        let opts = SnapshotOptions {
            labels: labels(value),
            ..Default::default()
        };
        let snapshot = client.create_snapshot_with(code, &opts).await.unwrap();
        created.push(snapshot.entry.unwrap());
    }
    let entries = |list: &SnapshotList| -> Vec<String> {
        list.entries.iter().map(|s| s.entry.clone()).collect()
    };

    let found = client
        .list_snapshots(&[("k", "v")], None, None)
        .await
        .unwrap();
    assert_eq!(entries(&found), [created[0].clone()]);
    assert!(found.next.is_none());
    let page = client.list_snapshots(&[], Some(1), None).await.unwrap();
    assert_eq!(entries(&page), [created[1].clone()]);
    let rest = client
        .list_snapshots(&[], Some(1), page.next.as_deref())
        .await
        .unwrap();
    assert_eq!(entries(&rest), [created[0].clone()]);

    let summary = client
        .set_snapshot_labels(&created[0], &labels("u"), &[])
        .await
        .unwrap();
    assert_eq!(
        (&summary.entry, &summary.labels),
        (&created[0], &labels("u"))
    );
    let gone = client
        .list_snapshots(&[("k", "v")], None, None)
        .await
        .unwrap();
    assert!(gone.entries.is_empty());
    let moved = client
        .list_snapshots(&[("k", "u")], None, None)
        .await
        .unwrap();
    assert_eq!(entries(&moved), [created[0].clone()]);
    let removed = client
        .set_snapshot_labels(&created[0], &BTreeMap::new(), &["k".to_string()])
        .await
        .unwrap();
    assert!(removed.labels.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn verify_snapshot_uses_the_clients_version() {
    let c = Cluster::start(&[("w0", "a", 1), ("w1", "b", 1)]).await;
    let client = |version: &str| Client::new(&c.url("/")).unwrap().with_version(version);
    let code = "import Foo\ntheorem base : 1 = 1 := rfl";
    let entry = client("b")
        .create_snapshot(code)
        .await
        .unwrap()
        .entry
        .unwrap();

    let err = client("a").verify_snapshot(&entry).await.unwrap_err();
    assert_eq!(err.kind(), Some(ErrorKind::NotFound));
    assert!(client("b").verify_snapshot(&entry).await.unwrap().verified);
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_version_is_out_of_capacity() {
    let mut cfg = common::router_config();
    cfg.max_wait_secs = 1;
    let c = Cluster::start_with(cfg, &[("w0", "v", 1)], |w| w).await;
    let client = Client::new(&c.url("/")).unwrap().with_version("nope");
    match client.check("#check Nat").await {
        Err(Error::Server { status, kind, .. }) => {
            assert_eq!((status, kind), (429, ErrorKind::Capacity))
        }
        other => panic!("{other:?}"),
    }
}
