//! The Rust client against an in-process cluster.

mod common;

use common::Cluster;
use pisa_client::{Client, Error, ErrorKind};

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
    let header = base.header.unwrap();
    assert!(base.result.ok, "{:?}", base.result);

    let r = client
        .check(&format!("{header}\nneeds base"))
        .await
        .unwrap();
    assert!(r.ok, "{r:?}");
    let info = client.snapshot(&name).await.unwrap();
    assert_eq!(info.header, header);
    assert!(!info.dirty);

    assert_eq!(client.delete_snapshot(&name).await.unwrap(), [name.clone()]);
    let err = client.snapshot(&name).await.unwrap_err();
    assert_eq!(err.kind(), Some(ErrorKind::NotFound));
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
