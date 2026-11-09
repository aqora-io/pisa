//! The Rust client against an in-process cluster.

mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use common::Cluster;
use pisa_client::{
    Allow, CheckOptions, Client, CreateSnapshotOptions, Detail, Error, ErrorKind, HealthStatus,
    Job, JobKind, JobPoll, JobStatus, ListSnapshotsResponse, Pos, ProofStatus, RebuildLink,
    RebuildOptions, Replaces,
};

#[tokio::test(flavor = "multi_thread")]
async fn client_covers_checks_and_sessions() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let client = Client::new(&c.url("/")).unwrap().with_timeout(5);
    assert_eq!(client.health().await.unwrap().status, HealthStatus::Ok);

    let r = client
        .check("import Foo\ntheorem t : 1 = 1 := rfl")
        .await
        .unwrap();
    assert!(r.ok, "{r:?}");

    let results = client
        .check_batch(["#check Nat", "theorem t : False := sorry"])
        .await
        .unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].messages[0].text, "Nat : Type");
    assert!(!results[1].ok);
    assert_eq!(results[1].reasons, ["axiom: t uses sorryAx"]);

    let s = client
        .start_session("theorem t : 1 = 1 := by sorry")
        .await
        .unwrap();
    let step = client.apply_tactic(&s.states[0].id, "simp").await.unwrap();
    assert!(!step.complete);
    assert_eq!(step.state.goals, ["⊢ goal 1 after simp"]);
    let done = client.apply_tactic(&step.state.id, "rfl").await.unwrap();
    assert!(done.complete);
    let err = client
        .apply_tactic(&step.state.id, "fail")
        .await
        .unwrap_err();
    assert_eq!(err.kind(), Some(ErrorKind::Lean));
    let verdict = client
        .check_session(&s.id, "theorem t : 1 = 1 := by rfl")
        .await
        .unwrap();
    assert!(verdict.ok, "{verdict:?}");

    match client.apply_tactic("not-a-key", "rfl").await {
        Err(Error::Server { status, kind, .. }) => {
            assert_eq!((status, kind), (400, ErrorKind::BadRequest))
        }
        other => panic!("{other:?}"),
    }
}

/// A 503 that isn't a draining server's health, such as a proxy's, is an
/// error.
#[tokio::test(flavor = "multi_thread")]
async fn health_fails_on_a_503_that_is_not_a_servers() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let request = socket.read(&mut [0; 1024]).await.unwrap();
        assert!(request > 0);
        let reply = "HTTP/1.1 503 Service Unavailable\r\ncontent-length: 11\r\nconnection: close\r\n\r\nno upstream";
        socket.write_all(reply.as_bytes()).await.unwrap();
    });
    match Client::new(&url).unwrap().health().await {
        Err(Error::Server {
            status, message, ..
        }) => assert_eq!((status, message.as_str()), (503, "no upstream")),
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn client_asks_about_positions() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let client = Client::new(&c.url("/")).unwrap().with_timeout(5);
    let code = "theorem t : True := by\n  exact foo";
    let options = CheckOptions {
        detail: vec![Detail::Terms],
        at: vec![Pos { line: 2, column: 9 }],
    };
    let r = client.check_with(code, &options).await.unwrap();
    let at = &r.at[0];
    assert_eq!(at.tactic.as_ref().unwrap().goals_before, ["⊢ goal 2"]);
    let term = at.term.as_ref().unwrap();
    assert_eq!(
        (term.term.as_str(), term.expected_type.as_str()),
        ("foo", "goal 2")
    );

    let job = client.submit_check_with(code, &options).await.unwrap();
    assert_eq!(client.wait_job(&job).await.unwrap().at, r.at);
    let results = client.check_batch_with([code], &options).await.unwrap();
    assert_eq!(results[0].at, r.at);

    let terms_alone = CheckOptions {
        detail: vec![Detail::Terms],
        at: Vec::new(),
    };
    match client.check_with(code, &terms_alone).await {
        Err(Error::Server { status, kind, .. }) => {
            assert_eq!((status, kind), (400, ErrorKind::BadRequest));
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
    assert!(base.check.ok, "{:?}", base.check);
    assert!(base.replacement.is_none());
    let base = base.snapshot.unwrap();
    let (id, name) = (base.id.clone(), base.module.clone());
    assert_eq!(base.version, c.version("v"));

    let r = client
        .check(&format!("{}\nneeds base", base.header))
        .await
        .unwrap();
    assert!(r.ok, "{r:?}");
    let detail = client.get_snapshot(&id).await.unwrap();
    assert_eq!(detail.snapshot, base);
    assert_eq!(detail.snapshot.status, ProofStatus::Proved);
    assert!(!detail.snapshot.verified);
    assert_eq!(detail.exports[0].name, "base");

    let verified = client.verify_snapshot(&id).await.unwrap();
    assert!(verified.snapshot.verified);
    assert_eq!(verified.snapshot.id, id);
    assert_eq!(verified.checked, std::slice::from_ref(&name));
    assert!(verified.failure.is_none());
    assert!(client.get_snapshot(&id).await.unwrap().snapshot.verified);
    let verify = |verify| CreateSnapshotOptions {
        verify: Some(verify),
        ..Default::default()
    };
    let forged = client
        .create_snapshot_with("import Foo\ntheorem forge : 1 = 1 := rfl", &verify(true))
        .await
        .unwrap();
    assert!(forged.snapshot.is_none(), "{forged:?}");
    let opted_out = client
        .create_snapshot_with("import Foo\nmacro \"m\" : term => `(1)", &verify(false))
        .await
        .unwrap();
    assert!(!opted_out.snapshot.unwrap().verified);

    let deleted = client.delete_snapshot(&id).await.unwrap();
    assert_eq!(deleted.removed, std::slice::from_ref(&name));
    let err = client.get_snapshot(&id).await.unwrap_err();
    assert_eq!(err.kind(), Some(ErrorKind::NotFound));
}

#[tokio::test(flavor = "multi_thread")]
async fn client_passes_labels() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let client = Client::new(&c.url("/")).unwrap().with_version("v");
    let opts = CreateSnapshotOptions {
        labels: BTreeMap::from([("k".to_string(), "v".to_string())]),
        key: Some("run-1".to_string()),
        ..Default::default()
    };
    let code = "import Foo\ntheorem base : 1 = 1 := rfl";
    let created = client.create_snapshot_with(code, &opts).await.unwrap();
    let created = created.snapshot.unwrap();
    assert_eq!(created.labels, opts.labels);
    let detail = client.get_snapshot(&created.id).await.unwrap();
    assert_eq!(detail.snapshot.labels, opts.labels);

    let again = client.create_snapshot_with(code, &opts).await.unwrap();
    assert_eq!(again.snapshot, Some(created));
}

#[tokio::test(flavor = "multi_thread")]
async fn client_replaces_snapshots() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let client = Client::new(&c.url("/")).unwrap().with_version("v");
    let skeleton = client
        .create_snapshot("import Foo\ntheorem t : 1 = 1 := sorry")
        .await
        .unwrap();
    let replaces = Replaces {
        snapshot_id: skeleton.snapshot.unwrap().id,
        allow: None,
    };
    let opts = CreateSnapshotOptions {
        replaces: Some(replaces.clone()),
        ..Default::default()
    };
    let proof = client
        .create_snapshot_with("import Foo\ntheorem t : 1 = 1 := rfl", &opts)
        .await
        .unwrap();
    assert_eq!(proof.replacement.unwrap().filled, ["t"]);
    let proof = proof.snapshot.unwrap();
    assert_eq!(proof.replaces.as_ref(), Some(&replaces));
    let detail = client.get_snapshot(&proof.id).await.unwrap();
    assert_eq!(detail.snapshot.replaces, Some(replaces));

    let weakened = client
        .create_snapshot_with("import Foo\ntheorem t : True := trivial", &opts)
        .await
        .unwrap();
    assert!(weakened.snapshot.is_none() && !weakened.check.ok);
    assert_eq!(
        weakened.check.reasons,
        ["statement: t has a different type"]
    );

    // `allow` names the definitions whose value may change.
    let skeleton = client
        .create_snapshot("import Foo\ndef f : Nat := 1\ntheorem u : f = f := sorry")
        .await
        .unwrap();
    let replaces = Replaces {
        snapshot_id: skeleton.snapshot.unwrap().id,
        allow: Some(Allow {
            definitions: vec!["f".into()],
        }),
    };
    let opts = CreateSnapshotOptions {
        replaces: Some(replaces.clone()),
        ..Default::default()
    };
    let fixed = client
        .create_snapshot_with(
            "import Foo\ndef f : Nat := 2\ntheorem u : f = f := rfl",
            &opts,
        )
        .await
        .unwrap();
    assert_eq!(fixed.replacement.unwrap().changed_definitions, ["f"]);
    assert_eq!(fixed.snapshot.unwrap().replaces, Some(replaces));
}

#[tokio::test(flavor = "multi_thread")]
async fn client_lists_dependents_and_plans_rebuilds() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let client = Client::new(&c.url("/")).unwrap().with_version("v");
    let b = client
        .create_snapshot("import Foo\ntheorem rb : 1 = 1 := sorry")
        .await
        .unwrap()
        .snapshot
        .unwrap();
    let labels = BTreeMap::from([("k".to_string(), "v".to_string())]);
    let opts = CreateSnapshotOptions {
        labels: labels.clone(),
        ..Default::default()
    };
    let code = format!("{}\n\ntheorem above : 2 = 2 := rfl", b.header);
    let above = client.create_snapshot_with(&code, &opts).await.unwrap();
    let above = above.snapshot.unwrap();
    let listed = client.list_dependents(&b.id, &labels).await.unwrap();
    assert_eq!(listed.snapshots, std::slice::from_ref(&above));

    let opts = CreateSnapshotOptions {
        replaces: Some(Replaces {
            snapshot_id: b.id.clone(),
            allow: None,
        }),
        ..Default::default()
    };
    let new = client
        .create_snapshot_with("import Foo\ntheorem rb : 1 = 1 := rfl", &opts)
        .await
        .unwrap()
        .snapshot
        .unwrap();
    let dry_run = RebuildOptions {
        dry_run: true,
        ..Default::default()
    };
    let plan = client
        .rebuild_snapshots(&b.id, &new.id, &labels, &dry_run)
        .await
        .unwrap();
    assert!(plan.ok && plan.changes.is_empty(), "{plan:?}");
    assert!(plan.created.is_empty(), "a dry run creates nothing");
    // A dry run isn't a job: the server answers it at once.
    let err = client
        .submit_rebuild_snapshots(&b.id, &new.id, &labels, &dry_run)
        .await
        .unwrap_err();
    match &err {
        Error::Server {
            status: 200,
            kind: ErrorKind::Internal,
            message,
        } => assert!(message.contains("at once"), "{message}"),
        other => panic!("{other:?}"),
    }
    let item = &plan.plan[0];
    assert_eq!(item.snapshot_id.as_ref(), Some(&above.id));
    let link = RebuildLink {
        snapshot_id: Some(above.id.clone()),
        item: None,
        allow: None,
    };
    assert_eq!(item.replaces, Some(link));
    let opts = CreateSnapshotOptions {
        labels: item.labels.clone(),
        key: Some(item.key.clone()),
        replaces: Some(Replaces {
            snapshot_id: above.id.clone(),
            allow: None,
        }),
        ..Default::default()
    };
    let rebuilt = client
        .create_snapshot_with(&item.code, &opts)
        .await
        .unwrap();
    assert!(rebuilt.replacement.is_some(), "{rebuilt:?}");
    let rebuilt = rebuilt.snapshot.unwrap();
    assert_eq!(
        rebuilt.replaces.as_ref().map(|r| &r.snapshot_id),
        Some(&above.id)
    );
    assert_eq!(rebuilt.module, item.new_module);

    // The server runs the same plan: the item above replays, as a job too.
    let opts = RebuildOptions::default();
    let run = client
        .rebuild_snapshots(&b.id, &new.id, &labels, &opts)
        .await
        .unwrap();
    assert!(run.ok, "{run:?}");
    assert_eq!(run.created[0].new_snapshot_id, rebuilt.id);
    let job = client
        .submit_rebuild_snapshots(&b.id, &new.id, &labels, &opts)
        .await
        .unwrap();
    assert_eq!(job.kind, JobKind::RebuildSnapshots);
    let again = client.wait_job(&job).await.unwrap();
    assert_eq!(
        again.created[0].new_snapshot_id,
        run.created[0].new_snapshot_id
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn client_audits_a_project() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let client = Client::new(&c.url("/")).unwrap().with_version("v");
    let a = client
        .create_snapshot("import Foo\naxiom kunneth : 1 = 1")
        .await
        .unwrap()
        .snapshot
        .unwrap();
    let labels = BTreeMap::from([("audit".to_string(), "1".to_string())]);
    let opts = CreateSnapshotOptions {
        labels: labels.clone(),
        ..Default::default()
    };
    let code = format!("{}\n\ntheorem thm3 : 1 = 1 := kunneth", a.header);
    let b = client.create_snapshot_with(&code, &opts).await.unwrap();
    assert_eq!(b.snapshot.unwrap().status, ProofStatus::Conditional);
    let audit = client.audit_snapshots(&labels).await.unwrap();
    assert_eq!(audit.summary.conditional, 1);
    assert_eq!(audit.assumptions[0].name, "kunneth");
    assert_eq!(audit.assumptions[0].snapshot_id.as_ref(), Some(&a.id));
    assert_eq!(audit.assumptions[0].used_by, ["thm3"]);
    assert!(audit.not_proved.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn client_audits_a_run_against_its_spec() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let client = Client::new(&c.url("/")).unwrap().with_version("v");
    let spec = client
        .create_snapshot("import Foo\ntheorem t : 1 = 1 := sorry")
        .await
        .unwrap()
        .snapshot
        .unwrap();
    let audit = client.audit_snapshots_against([&spec.id]).await.unwrap();
    assert_eq!(audit.clean, Some(false));
    assert_eq!(audit.targets[0].status, ProofStatus::Sorry);
    // No ids would be a label audit: it isn't sent.
    let none: [&str; 0] = [];
    let refused = client.audit_snapshots_against(none).await;
    assert!(matches!(refused, Err(Error::Invalid(_))), "{refused:?}");

    let opts = CreateSnapshotOptions {
        replaces: Some(Replaces {
            snapshot_id: spec.id.clone(),
            allow: None,
        }),
        verify: Some(true),
        ..Default::default()
    };
    let proof = client
        .create_snapshot_with("import Foo\ntheorem t : 1 = 1 := rfl", &opts)
        .await
        .unwrap()
        .snapshot
        .unwrap();
    let audit = client.audit_snapshots_against([&spec.id]).await.unwrap();
    assert_eq!(audit.clean, Some(true), "{audit:?}");
    assert_eq!(audit.heads, std::slice::from_ref(&proof.id));
    let target = &audit.targets[0];
    assert_eq!(
        (&target.name, &target.spec, &target.head, target.status),
        (&"t".to_string(), &spec.id, &proof.id, ProofStatus::Proved)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn client_lists_and_updates_snapshots() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let client = Client::new(&c.url("/")).unwrap().with_version("v");
    let code = "import Foo\ntheorem base : 1 = 1 := rfl";
    let labels = |value: &str| BTreeMap::from([("k".to_string(), value.to_string())]);
    let mut created = Vec::new();
    for value in ["v", "w"] {
        let opts = CreateSnapshotOptions {
            labels: labels(value),
            ..Default::default()
        };
        let made = client.create_snapshot_with(code, &opts).await.unwrap();
        created.push(made.snapshot.unwrap().id);
    }
    let ids = |list: &ListSnapshotsResponse| -> Vec<String> {
        list.snapshots.iter().map(|s| s.id.clone()).collect()
    };

    let found = client
        .list_snapshots(&labels("v"), None, None)
        .await
        .unwrap();
    assert_eq!(ids(&found), [created[0].clone()]);
    assert!(found.next.is_none());
    let all = BTreeMap::new();
    let page = client.list_snapshots(&all, Some(1), None).await.unwrap();
    assert_eq!(ids(&page), [created[1].clone()]);
    let rest = client
        .list_snapshots(&all, Some(1), page.next.as_deref())
        .await
        .unwrap();
    assert_eq!(ids(&rest), [created[0].clone()]);

    let patch = |value: Option<&str>| BTreeMap::from([("k".to_string(), value.map(String::from))]);
    let updated = client
        .update_snapshot(&created[0], &patch(Some("u")))
        .await
        .unwrap();
    assert_eq!((&updated.id, &updated.labels), (&created[0], &labels("u")));
    let gone = client
        .list_snapshots(&labels("v"), None, None)
        .await
        .unwrap();
    assert!(gone.snapshots.is_empty());
    let moved = client
        .list_snapshots(&labels("u"), None, None)
        .await
        .unwrap();
    assert_eq!(ids(&moved), [created[0].clone()]);
    let removed = client
        .update_snapshot(&created[0], &patch(None))
        .await
        .unwrap();
    assert!(removed.labels.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn ids_route_whatever_the_clients_version() {
    let c = Cluster::start(&[("w0", "a", 1), ("w1", "b", 1)]).await;
    let client = |version: &str| Client::new(&c.url("/")).unwrap().with_version(version);
    let code = "import Foo\ntheorem base : 1 = 1 := rfl";
    let id = client("b")
        .create_snapshot(code)
        .await
        .unwrap()
        .snapshot
        .unwrap()
        .id;

    let other = client("a");
    assert_eq!(other.get_snapshot(&id).await.unwrap().snapshot.id, id);
    let verified = other.verify_snapshot(&id).await.unwrap();
    assert!(verified.snapshot.verified);
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

#[tokio::test(flavor = "multi_thread")]
async fn client_submits_and_waits_for_jobs() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let client = Client::new(&c.url("/")).unwrap().with_timeout(5);

    let job = client.submit_check("#check Nat").await.unwrap();
    assert_eq!(job.kind, JobKind::Check);
    assert!(client.wait_job(&job).await.unwrap().ok);

    let job = client
        .submit_start_session("theorem t : 1 = 1 := by sorry")
        .await
        .unwrap();
    let s = client.wait_job(&job).await.unwrap();
    let job = client
        .submit_apply_tactic(&s.states[0].id, "rfl")
        .await
        .unwrap();
    assert_eq!(job.kind, JobKind::ApplyTactic);
    assert!(client.wait_job(&job).await.unwrap().complete);
    // An error is the request's own.
    let job = client
        .submit_apply_tactic(&s.states[0].id, "fail")
        .await
        .unwrap();
    let err = client.wait_job(&job).await.unwrap_err();
    assert_eq!(err.kind(), Some(ErrorKind::Lean));
    let job = client
        .submit_check_session(&s.id, "theorem t : 1 = 1 := by rfl")
        .await
        .unwrap();
    assert!(client.wait_job(&job).await.unwrap().ok);

    let job = client
        .submit_create_snapshot("def jobbed := 1")
        .await
        .unwrap();
    assert_eq!(job.kind, JobKind::CreateSnapshot);
    let made = client.wait_job(&job).await.unwrap();
    let id = made.snapshot.expect("kept").id;
    let job = client.submit_verify_snapshot(&id).await.unwrap();
    assert_eq!(job.kind, JobKind::VerifySnapshot);
    assert!(client.wait_job(&job).await.unwrap().snapshot.verified);

    // A job resumed by its id; a slow one is pending, and once a worker
    // holds it, it can't be cancelled.
    let slow = client.submit_check("SLEEP 1000\n#check Nat").await.unwrap();
    let resumed: Job<pisa_client::CheckResult> = Job::new(slow.id.clone(), slow.kind);
    let pending = client.poll_job(&resumed, Duration::ZERO).await.unwrap();
    let JobPoll::Pending(pending) = pending else {
        panic!("{pending:?}");
    };
    assert_eq!(pending.id, slow.id);
    assert!(matches!(
        pending.status,
        JobStatus::Queued | JobStatus::Running
    ));
    let queued = client.submit_check("#check Nat").await.unwrap();
    assert_eq!(
        client.cancel_job(&queued).await.unwrap().status,
        JobStatus::Cancelled
    );
    assert!(client.wait_job(&resumed).await.unwrap().ok);
}
