//! The Rust client against an in-process cluster.

mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use common::Cluster;
use pisa_client::{
    AcceptOptions, Allow, AmendOptions, Amends, AuditOptions, CheckOptions, Client,
    CreateSnapshotOptions, DeleteSnapshotOptions, Detail, Error, ErrorKind, ExportOptions,
    GoalsOptions, HealthStatus, Job, JobKind, JobPoll, JobStatus, LinePos, ListSnapshotsResponse,
    Pos, ProbeAt, ProofOptions, ProofStatus, RebuildOptions, Refusal, RefuteOptions, Refutes,
    Replaces, ResubmitOptions, RetryPolicy, SettleOptions, SpecOptions,
};
use pisa_protocol::api::DependencyState;

#[tokio::test(flavor = "multi_thread")]
async fn client_covers_checks_and_sessions() {
    let c = Cluster::start_beam(&[("w0", "v", 2)]).await;
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
    assert_eq!(step.state.goals, ["⊢ True after simp"]);
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

/// Health names the workers that run Lean without memory or process limits,
/// as a cluster's do (no cgroups), and a version whose sweep of the store's
/// files was refused, as a store another database's worker marked is.
#[tokio::test(flavor = "multi_thread")]
async fn health_names_workers_without_limits_and_a_refused_sweep() {
    use object_store::ObjectStoreExt as _;
    let mut c = Cluster::start(&[]).await;
    let client = Client::new(&c.url("/")).unwrap();
    let marker = object_store::path::Path::from(format!("catalogs/{}", c.version("v")));
    let other = object_store::PutPayload::from_static(b"another-catalog");
    c.store.put(&marker, other).await.unwrap();
    c.add_worker(common::worker_config("w0", "v", 1));
    c.wait_for_workers(1).await;
    // A worker sweeps as it starts.
    let health = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let health = client.health().await.unwrap();
            if health
                .sweeps_refused
                .as_ref()
                .is_some_and(|r| !r.is_empty())
            {
                return health;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("health shows the refused sweep");
    assert_eq!(health.unlimited_workers, Some(vec!["w0".to_string()]));
    assert_eq!(health.revision, pisa_protocol::revision::REVISION);
    assert_eq!(health.worker_revisions, Some(vec!["unknown".to_string()]));
    assert_eq!(health.dependencies.database, DependencyState::Ok);
    // The worker probes the store as it starts.
    tokio::time::timeout(Duration::from_secs(10), async {
        while client.health().await.unwrap().dependencies.store != DependencyState::Ok {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("health shows the store reachable");
    let refused = &health.sweeps_refused.unwrap()[0];
    assert_eq!(refused.version, c.version("v"));
    assert!(refused.reason.contains("another-catalog"), "{refused:?}");
    assert!(refused.since > 0);
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
    let verify = |verify| CreateSnapshotOptions {
        verify: Some(verify),
        ..Default::default()
    };
    let base = client
        .create_snapshot_with("import Foo\ntheorem base : 1 = 1 := rfl", &verify(false))
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
    assert_eq!(deleted.deleted, std::slice::from_ref(&id));
    let err = client.get_snapshot(&id).await.unwrap_err();
    assert_eq!(err.kind(), Some(ErrorKind::NotFound));
}

#[tokio::test(flavor = "multi_thread")]
async fn client_drops_a_branch() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let client = Client::new(&c.url("/")).unwrap().with_version("v");
    let skeleton = client
        .create_snapshot("import Foo\ntheorem t : 1 = 1 := sorry")
        .await
        .unwrap();
    let skeleton = skeleton.snapshot.unwrap().id;
    let replacing = |fork| CreateSnapshotOptions {
        replaces: Some(Replaces {
            snapshot_id: skeleton.clone(),
            allow: None,
        }),
        fork,
        ..Default::default()
    };
    let a1 = client
        .create_snapshot_with("import Foo\ntheorem t : 1 = 1 := rfl", &replacing(false))
        .await
        .unwrap();
    let a1 = a1.snapshot.unwrap().id;
    let b1 = client
        .create_snapshot_with("import Foo\ntheorem t : 1 = 1 := by simp", &replacing(true))
        .await
        .unwrap();
    let b1 = b1.snapshot.unwrap().id;
    let a2 = CreateSnapshotOptions {
        replaces: Some(Replaces {
            snapshot_id: a1.clone(),
            allow: None,
        }),
        ..Default::default()
    };
    let a2 = client
        .create_snapshot_with(
            "import Foo\ntheorem t : 1 = 1 := rfl\ntheorem u : 1 = 1 := rfl",
            &a2,
        )
        .await
        .unwrap();
    let a2 = a2.snapshot.unwrap().id;

    let opts = DeleteSnapshotOptions {
        with_replacers: true,
    };
    let dropped = client.delete_snapshot_with(&a1, &opts).await.unwrap();
    assert_eq!(dropped.deleted, [a1.clone(), a2.clone()]);
    for gone in [&a1, &a2] {
        let err = client.get_snapshot(gone).await.unwrap_err();
        assert_eq!(err.kind(), Some(ErrorKind::NotFound));
    }
    let audit = client
        .audit_snapshots_against([&skeleton], &[])
        .await
        .unwrap();
    assert_eq!(audit.heads, [b1]);
    assert!(audit.forks.is_empty());
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

    // The skeleton is replaced now: replacing it again needs `fork`.
    let weakened = "import Foo\ntheorem t : True := trivial";
    let refused = client.create_snapshot_with(weakened, &opts).await.unwrap();
    assert!(refused.snapshot.is_none());
    assert_eq!(refused.check.error.unwrap().kind, ErrorKind::Conflict);
    assert_eq!(refused.heads, std::slice::from_ref(&proof.id));
    let forking = CreateSnapshotOptions { fork: true, ..opts };
    let weakened = client
        .create_snapshot_with(weakened, &forking)
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
    let link = Replaces {
        snapshot_id: above.id.clone(),
        allow: None,
    };
    assert_eq!(item.replaces, Some(link));
    let opts = CreateSnapshotOptions {
        labels: item.labels.clone(),
        key: Some(item.key.clone()),
        replaces: item.replaces.clone(),
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
    assert_eq!(run.created[0].new_snapshot_id.as_ref(), Some(&rebuilt.id));
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
async fn client_audits_a_run_allowing_definitions() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let client = Client::new(&c.url("/")).unwrap().with_version("v");
    let verified = CreateSnapshotOptions {
        verify: Some(true),
        ..Default::default()
    };
    let code = "import Foo\ndef f : Nat := 1\ntheorem t : f = f := sorry";
    let spec = client.create_snapshot_with(code, &verified).await.unwrap();
    let spec = spec.snapshot.unwrap();
    let opts = CreateSnapshotOptions {
        replaces: Some(Replaces {
            snapshot_id: spec.id.clone(),
            allow: Some(Allow {
                definitions: vec!["f".into()],
            }),
        }),
        ..verified
    };
    let code = "import Foo\ndef f : Nat := 2\ntheorem t : f = f := rfl";
    client.create_snapshot_with(code, &opts).await.unwrap();
    let audit = client
        .audit_snapshots_against([&spec.id], &[])
        .await
        .unwrap();
    assert_eq!(audit.clean, Some(false), "{audit:?}");
    let audit = client
        .audit_snapshots_against([&spec.id], &["f"])
        .await
        .unwrap();
    assert_eq!(audit.clean, Some(true), "{audit:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn client_creates_a_spec_with_holes() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let client = Client::new(&c.url("/")).unwrap().with_version("v");
    let opts = CreateSnapshotOptions {
        verify: Some(true),
        holes: vec!["f".into()],
        ..Default::default()
    };
    let code = "import Foo\ndef f : Nat := 1\ntheorem t : f = f := sorry";
    let spec = client.create_snapshot_with(code, &opts).await.unwrap();
    let spec = spec.snapshot.unwrap();
    assert_eq!(spec.holes, ["f"]);
    let opts = CreateSnapshotOptions {
        verify: Some(true),
        replaces: Some(Replaces {
            snapshot_id: spec.id.clone(),
            allow: None,
        }),
        ..Default::default()
    };
    let code = "import Foo\ndef f : Nat := 2\ntheorem t : f = f := rfl";
    let filled = client.create_snapshot_with(code, &opts).await.unwrap();
    assert_eq!(filled.snapshot.unwrap().holes, ["f"]);
    let audit = client
        .audit_snapshots_against([&spec.id], &[])
        .await
        .unwrap();
    assert_eq!(audit.clean, Some(true), "{audit:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn client_audits_a_run_against_its_spec() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let client = Client::new(&c.url("/")).unwrap().with_version("v");
    // The kernel re-checks the spec too: the audit judges from its records.
    let verified = CreateSnapshotOptions {
        verify: Some(true),
        ..Default::default()
    };
    let spec = client
        .create_snapshot_with("import Foo\ntheorem t : 1 = 1 := sorry", &verified)
        .await
        .unwrap()
        .snapshot
        .unwrap();
    let audit = client
        .audit_snapshots_against([&spec.id], &[])
        .await
        .unwrap();
    assert_eq!(audit.clean, Some(false));
    assert_eq!(audit.targets[0].status, ProofStatus::Sorry);
    // No ids would be a label audit: it isn't sent.
    let none: [&str; 0] = [];
    let refused = client.audit_snapshots_against(none, &[]).await;
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
    let audit = client
        .audit_snapshots_against([&spec.id], &[])
        .await
        .unwrap();
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
        .list_snapshots(&labels("v"), None, None, false)
        .await
        .unwrap();
    assert_eq!(ids(&found), [created[0].clone()]);
    assert!(found.next.is_none());
    let all = BTreeMap::new();
    let page = client
        .list_snapshots(&all, Some(1), None, false)
        .await
        .unwrap();
    assert_eq!(ids(&page), [created[1].clone()]);
    let rest = client
        .list_snapshots(&all, Some(1), page.next.as_deref(), false)
        .await
        .unwrap();
    assert_eq!(ids(&rest), [created[0].clone()]);
    // Nothing replaces either, so both are heads.
    let heads = client.list_snapshots(&all, None, None, true).await.unwrap();
    assert_eq!(ids(&heads), [created[1].clone(), created[0].clone()]);

    let patch = |value: Option<&str>| BTreeMap::from([("k".to_string(), value.map(String::from))]);
    let updated = client
        .update_snapshot(&created[0], &patch(Some("u")))
        .await
        .unwrap();
    assert_eq!((&updated.id, &updated.labels), (&created[0], &labels("u")));
    let gone = client
        .list_snapshots(&labels("v"), None, None, false)
        .await
        .unwrap();
    assert!(gone.snapshots.is_empty());
    let moved = client
        .list_snapshots(&labels("u"), None, None, false)
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
    // A replacement runs on the replaced snapshot's version, not the client's.
    let opts = CreateSnapshotOptions {
        replaces: Some(Replaces {
            snapshot_id: id.clone(),
            allow: None,
        }),
        fork: true,
        ..Default::default()
    };
    let replacement = other.create_snapshot_with(code, &opts).await.unwrap();
    let snapshot = replacement.snapshot.expect("kept");
    assert!(
        snapshot.id.starts_with(&id[..id.find('~').unwrap() + 1]),
        "{snapshot:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_version_is_out_of_capacity() {
    let mut cfg = common::router_config();
    cfg.max_wait_secs = 1;
    let c = Cluster::start_with(cfg, &[("w0", "v", 1)], |w| w).await;
    let client = Client::new(&c.url("/"))
        .unwrap()
        .with_version("nope")
        .with_retry(RetryPolicy::none());
    match client.check("#check Nat").await {
        Err(Error::Server { status, kind, .. }) => {
            assert_eq!((status, kind), (429, ErrorKind::Capacity))
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn client_submits_and_waits_for_jobs() {
    let c = Cluster::start_beam(&[("w0", "v", 1)]).await;
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

#[tokio::test(flavor = "multi_thread")]
async fn client_opens_asks_edits_and_closes_a_document() {
    let c = Cluster::start_beam(&[("w0", "v", 1)]).await;
    let client = Client::new(&c.url("/"))
        .unwrap()
        .with_version("v")
        .with_timeout(10);
    let code = "import Foo\n\ntheorem t : 𝓝 = 𝓝 := by\n  sorry\n";
    let d = client.open_document(code).await.unwrap();
    assert_eq!((d.revision, d.sorries.len()), (1, 1), "{d:?}");
    let line = |line| LinePos { line, column: None };
    let goals = client
        .document_goals(&d.id, &[line(4)], &GoalsOptions::default())
        .await
        .unwrap();
    assert_eq!(goals.at[0].goals, ["⊢ sorry"]);
    let probes = client
        .probe_document(&d.id, ProbeAt::Sorry { sorry: 0 }, &["rfl", "omega"], None)
        .await
        .unwrap();
    assert!(
        probes.results[0].closed && !probes.results[1].ok,
        "{probes:?}"
    );

    let edited = client
        .edit_document(&d.id, 1, &code.replace("sorry", "rfl"))
        .await
        .unwrap();
    assert_eq!((edited.revision, edited.sorries.len()), (2, 0));
    // A stale revision is a conflict.
    let stale = GoalsOptions {
        revision: Some(1),
        ..Default::default()
    };
    let e = client
        .document_goals(&d.id, &[line(4)], &stale)
        .await
        .unwrap_err();
    assert_eq!(e.kind(), Some(ErrorKind::Conflict));

    client.close_document(&d.id).await.unwrap();
    let e = client
        .document_goals(&d.id, &[line(4)], &GoalsOptions::default())
        .await
        .unwrap_err();
    assert_eq!(e.kind(), Some(ErrorKind::NotFound));
}

#[tokio::test(flavor = "multi_thread")]
async fn client_reviews_exports_and_accepts_a_run() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let client = Client::new(&c.url("/")).unwrap().with_version("v");
    let opts = CreateSnapshotOptions {
        holes: vec!["f".into()],
        ..Default::default()
    };
    let code = "import Foo\ndef f : Nat := sorry\ntheorem t : f = f := sorry";
    let spec = client.create_snapshot_with(code, &opts).await.unwrap();
    let spec = spec.snapshot.unwrap();
    let opts = CreateSnapshotOptions {
        replaces: Some(Replaces {
            snapshot_id: spec.id.clone(),
            allow: None,
        }),
        ..Default::default()
    };
    let code = "import Foo\ndef f : Nat := 2\ntheorem t : f = f := rfl";
    let head = client.create_snapshot_with(code, &opts).await.unwrap();
    let head = head.snapshot.unwrap();

    let review = AuditOptions {
        review: true,
        ..Default::default()
    };
    let audit = client
        .audit_snapshots_against_with([&spec.id], &review)
        .await
        .unwrap();
    let f = audit.review[0]
        .declarations
        .iter()
        .find(|d| d.name == "f")
        .unwrap();
    assert_eq!((f.hole, f.value.as_deref()), (true, Some("2")));
    let capped = AuditOptions {
        review: true,
        review_limit: Some(1),
        ..Default::default()
    };
    let audit = client
        .audit_snapshots_against_with([&spec.id], &capped)
        .await
        .unwrap();
    let listed: usize = audit.review.iter().map(|e| e.declarations.len()).sum();
    assert_eq!(listed, 1, "{audit:?}");
    assert!(audit.review_left_out > 0, "{audit:?}");

    let tarball = client.export_run([&spec.id], &[]).await.unwrap();
    assert_eq!(&tarball[..2], [0x1f, 0x8b], "a gzip");
    // As a Palomar package, whose challenge imports only Mathlib, Init and
    // Lean: this spec's `Foo` is refused.
    let palomar = ExportOptions {
        profile: Some("palomar".into()),
        pair: Some(1),
        ..Default::default()
    };
    let refused = client.export_run_with([&spec.id], &palomar).await;
    match refused {
        Err(Error::Server { kind, message, .. }) => {
            assert_eq!(kind, ErrorKind::BadRequest);
            assert!(message.contains("imports Foo"), "{message}");
        }
        other => panic!("{other:?}"),
    }

    let accepted = client
        .accept_run([&spec.id], &AcceptOptions::default())
        .await
        .unwrap();
    assert!(accepted.accepted && accepted.clean, "{accepted:?}");
    assert_eq!(accepted.heads, std::slice::from_ref(&head.id));
    let stored = client.get_acceptance(&accepted.id).await.unwrap();
    assert_eq!(stored.pairs, accepted.pairs);
    let job = client
        .submit_accept_run([&spec.id], &AcceptOptions::default())
        .await
        .unwrap();
    assert_eq!(job.kind, JobKind::AcceptRun);
    let later = client.wait_job(&job).await.unwrap();
    assert!(later.accepted);
    let listed = client.list_acceptances(&spec.id).await.unwrap();
    let ids: Vec<&str> = listed.acceptances.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(ids, [later.id.as_str(), accepted.id.as_str()]);
    let none: [&str; 0] = [];
    let e = client.accept_run(none, &AcceptOptions::default()).await;
    assert!(matches!(e, Err(Error::Invalid(_))), "{e:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn client_amends_a_spec_resubmits_its_run_and_refutes_a_target() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let client = Client::new(&c.url("/")).unwrap().with_version("v");
    let opts = CreateSnapshotOptions {
        holes: vec!["cost".into()],
        ..Default::default()
    };
    let spec = "import Foo\ndef cost : Nat := sorry\ntheorem t : cost = cost := sorry\ntheorem w : 1 = 2 := sorry";
    let spec = client.create_snapshot_with(spec, &opts).await.unwrap();
    let spec = spec.snapshot.unwrap();
    let replaces = Replaces {
        snapshot_id: spec.id.clone(),
        allow: None,
    };
    let opts = CreateSnapshotOptions {
        replaces: Some(replaces),
        ..Default::default()
    };
    let head = "import Foo\ndef cost : Nat := 1\ntheorem t : cost = cost := rfl\ntheorem w : 1 = 2 := sorry";
    let head = client.create_snapshot_with(head, &opts).await.unwrap();
    let head = head.snapshot.unwrap();

    // `w` is false: refute it, then amend the spec without it.
    let refuter = format!("{}\ntheorem r : ¬ (1 = 2) := by decide", spec.header);
    let opts = CreateSnapshotOptions {
        refutes: Some(Refutes {
            snapshot_id: spec.id.clone(),
            target: "w".into(),
        }),
        ..Default::default()
    };
    let refuted = client.create_snapshot_with(&refuter, &opts).await.unwrap();
    assert_eq!(refuted.refutation.map(|r| r.theorem).as_deref(), Some("r"));
    let opts = CreateSnapshotOptions {
        holes: vec!["cost".into()],
        amends: Some(Amends {
            snapshot_id: spec.id.clone(),
            reason: "w is false".into(),
        }),
        ..Default::default()
    };
    let amended = "import Foo\ndef cost : Nat := sorry\ntheorem t : cost = cost := sorry";
    let amended = client.create_snapshot_with(amended, &opts).await.unwrap();
    let amendment = amended.amendment.unwrap();
    assert_eq!(amendment.changes[0].name, "w");
    let to = amended.snapshot.unwrap().id;

    let dry = ResubmitOptions {
        dry_run: true,
        ..Default::default()
    };
    let plan = client
        .resubmit_snapshots(&spec.id, &to, &dry)
        .await
        .unwrap();
    assert!(plan.ok && plan.items[0].passes, "{plan:?}");
    assert_eq!(plan.items[0].snapshot_id, head.id);
    let job = client
        .submit_resubmit_snapshots(&spec.id, &to, &ResubmitOptions::default())
        .await
        .unwrap();
    assert_eq!(job.kind, JobKind::ResubmitSnapshots);
    let done = client.wait_job(&job).await.unwrap();
    assert!(
        done.ok && done.items[0].new_snapshot_id.is_some(),
        "{done:?}"
    );
    let again = client
        .resubmit_snapshots(&spec.id, &to, &ResubmitOptions::default())
        .await
        .unwrap();
    assert_eq!(
        again.items[0].new_snapshot_id,
        done.items[0].new_snapshot_id
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn client_reads_a_run_through_links_and_settles_it() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let client = Client::new(&c.url("/")).unwrap().with_version("v");
    let made = |r: pisa_client::CreateSnapshotResponse| r.snapshot.expect("kept");
    let replacing = |s: &pisa_client::Snapshot| CreateSnapshotOptions {
        replaces: Some(Replaces {
            snapshot_id: s.id.clone(),
            allow: None,
        }),
        ..Default::default()
    };
    let spec_l = made(
        client
            .create_snapshot("import Foo\n\ntheorem rs_l : 1 = 1 := sorry")
            .await
            .unwrap(),
    );
    let code = format!("{}\n\ntheorem rs_t : 2 = 2 := sorry", spec_l.header);
    let spec_t = made(client.create_snapshot(&code).await.unwrap());
    let code = format!(
        "{}\n\ntheorem rs_t : 2 = 2 := by have := rs_l; rfl",
        spec_l.header
    );
    client
        .create_snapshot_with(&code, &replacing(&spec_t))
        .await
        .unwrap();
    let code = "import Foo\n\ntheorem rs_l : 1 = 1 := rfl";
    client
        .create_snapshot_with(code, &replacing(&spec_l))
        .await
        .unwrap();
    let specs = [&spec_l.id, &spec_t.id];
    let audit = client
        .audit_snapshots_against_with(specs, &AuditOptions::default())
        .await
        .unwrap();
    let t = audit.targets.iter().find(|t| t.name == "rs_t").unwrap();
    assert_eq!(t.linked_status, Some(ProofStatus::Proved), "{audit:?}");
    assert_eq!(t.through[0].name, "rs_l");
    assert_eq!(audit.unsettled.len(), 1);

    let dry = SettleOptions {
        dry_run: true,
        ..Default::default()
    };
    let plan = client.settle_run(specs, &dry).await.unwrap();
    assert!(plan.ok && plan.plan.len() == 1, "{plan:?}");
    let job = client
        .submit_settle_run(specs, &SettleOptions::default())
        .await
        .unwrap();
    assert!(client.wait_job(&job).await.unwrap().ok);
    let audit = client
        .audit_snapshots_against_with(specs, &AuditOptions::default())
        .await
        .unwrap();
    assert_eq!(audit.clean, Some(true), "{audit:?}");

    let again = client.verify_snapshot_again(&spec_t.id).await.unwrap();
    assert!(!again.checked.is_empty(), "{again:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn client_names_a_run_and_imports_it_by_name() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let client = Client::new(&c.url("/")).unwrap().with_version("v");
    let opts = CreateSnapshotOptions {
        name: Some("cl.lemma".into()),
        ..Default::default()
    };
    let spec = "import Foo\ntheorem cl_lemma : 1 = 1 := sorry";
    let spec = client.create_snapshot_with(spec, &opts).await.unwrap();
    let spec = spec.snapshot.unwrap();
    assert_eq!(spec.run.as_deref(), Some("cl.lemma"));

    let code = "import PisaRun.cl.lemma\n\nneeds cl_lemma\ntheorem cl_t : 1 = 1 := cl_lemma";
    let checked = client.check(code).await.unwrap();
    // The lemma is sorried, so the check isn't ok; the proof itself is done.
    assert_eq!(
        checked.declarations[0].own_status,
        Some(ProofStatus::Proved),
        "{checked:?}"
    );
    assert_eq!(checked.runs[0].module, spec.module);
    assert_eq!(checked.runs[0].snapshot_id, spec.id);
    let made = client.create_snapshot(code).await.unwrap();
    assert_eq!(made.runs, checked.runs);
    let detail = client
        .get_snapshot(&made.snapshot.unwrap().id)
        .await
        .unwrap();
    assert_eq!(detail.source.as_deref(), Some(code));
    assert!(detail.code.contains(&spec.module), "{}", detail.code);

    // The run by its name, and named again.
    let run = client.get_run("cl.lemma").await.unwrap();
    assert_eq!(
        (run.root.as_str(), run.snapshot_id.as_deref()),
        (spec.id.as_str(), Some(spec.id.as_str()))
    );
    let renamed = client.name_run("cl.renamed", &spec.id).await.unwrap();
    assert_eq!(renamed.heads, std::slice::from_ref(&spec.id));
    let err = client.get_run("cl.lemma").await.unwrap_err();
    assert_eq!(err.kind(), Some(ErrorKind::NotFound));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_keeps_to_its_scope() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let scoped = |user: &str| {
        let scope = BTreeMap::from([("user".to_string(), user.to_string())]);
        Client::new(&c.url("/"))
            .unwrap()
            .with_version("v")
            .with_scope(&scope)
            .unwrap()
    };
    let made = scoped("a")
        .create_snapshot("import Foo\ntheorem sc : 1 = 1 := rfl")
        .await
        .unwrap();
    let made = made.snapshot.unwrap();
    assert_eq!(made.labels["user"], "a");
    assert!(scoped("a").get_snapshot(&made.id).await.is_ok());
    let err = scoped("b").get_snapshot(&made.id).await.unwrap_err();
    assert_eq!(err.kind(), Some(ErrorKind::NotFound));
    let bad = BTreeMap::from([("user".to_string(), "a,b".to_string())]);
    assert!(Client::new(&c.url("/")).unwrap().with_scope(&bad).is_err());
}

/// A server that answers each request with the next of `replies`, raw HTTP
/// responses, one connection each, and records the requests as method,
/// path and JSON body.
async fn scripted(
    replies: Vec<String>,
) -> (String, std::sync::Arc<std::sync::Mutex<Vec<Recorded>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = seen.clone();
    tokio::spawn(async move {
        for reply in replies {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buf = [0; 4096];
            // The head, then as much body as it says.
            let (head, body) = loop {
                let n = socket.read(&mut buf).await.unwrap();
                request.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&request).to_string();
                let Some(end) = text.find("\r\n\r\n") else {
                    continue;
                };
                let length = (text[..end].lines())
                    .find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse().ok())?
                    })
                    .unwrap_or(0usize);
                if request.len() >= end + 4 + length {
                    break (
                        text[..end].to_string(),
                        text[end + 4..end + 4 + length].to_string(),
                    );
                }
            };
            let mut line = head.lines().next().unwrap().split(' ');
            let (method, path) = (
                line.next().unwrap().to_string(),
                line.next().unwrap().to_string(),
            );
            let body = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
            recorded
                .lock()
                .unwrap()
                .push(Recorded { method, path, body });
            socket.write_all(reply.as_bytes()).await.unwrap();
        }
    });
    (url, seen)
}

#[derive(Debug, Clone)]
struct Recorded {
    method: String,
    path: String,
    body: serde_json::Value,
}

/// A raw HTTP response with `status`, `headers` and a JSON or text `body`.
fn reply(status: &str, headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\n{headers}content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

const CHECKED: &str =
    r#"{"ok": true, "time": 0, "messages": [], "sorries": [], "reasons": [], "declarations": []}"#;

/// A client of `url` that retries at once.
fn eager(url: &str) -> Client {
    Client::new(url).unwrap().with_retry(RetryPolicy {
        base_delay: Duration::from_millis(1),
        ..RetryPolicy::default()
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_sends_again_what_is_safe_to_and_waits_as_told() {
    // A 503 that says to wait a second, then the answer.
    let lost = r#"{"error": {"kind": "unavailable", "message": "the worker was lost"}}"#;
    let (url, seen) = scripted(vec![
        reply("503 Service Unavailable", "retry-after: 1\r\n", lost),
        reply("200 OK", "", CHECKED),
    ])
    .await;
    let started = std::time::Instant::now();
    let checked = eager(&url).check("#check Nat").await.unwrap();
    assert!(checked.ok);
    assert!(started.elapsed() >= Duration::from_secs(1));
    assert_eq!(seen.lock().unwrap().len(), 2);

    // A create is sent again with the same key, its own.
    let created = r#"{"check": {"ok": true, "time": 0, "declarations": []}}"#;
    let (url, seen) = scripted(vec![
        reply("502 Bad Gateway", "", "<html>bad gateway</html>"),
        reply("200 OK", "", created),
    ])
    .await;
    eager(&url).create_snapshot("def x := 1").await.unwrap();
    let seen = seen.lock().unwrap().clone();
    let keys: Vec<&serde_json::Value> = seen.iter().map(|r| &r.body["key"]).collect();
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0], keys[1]);
    assert!(keys[0].as_str().unwrap().starts_with("client:"), "{keys:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_sends_again_what_may_have_run_only_when_nothing_did() {
    let named = r#"{"name": "r", "root": "v~a", "heads": ["v~a"]}"#;
    // Naming a run again after a 502 might name it twice: the error is
    // returned, a gateway's, which may answer later.
    let (url, seen) = scripted(vec![reply("502 Bad Gateway", "", "bad gateway")]).await;
    match eager(&url).name_run("r", "v~a").await {
        Err(Error::Server { status, kind, .. }) => {
            assert_eq!((status, kind), (502, ErrorKind::Unavailable))
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(seen.lock().unwrap().len(), 1);
    // A 429 says nothing ran: sent again.
    let busy = r#"{"error": {"kind": "capacity", "message": "no worker took it"}}"#;
    let (url, seen) = scripted(vec![
        reply("429 Too Many Requests", "retry-after: 0\r\n", busy),
        reply("200 OK", "", named),
    ])
    .await;
    let run = eager(&url).name_run("r", "v~a").await.unwrap();
    assert_eq!(run.heads, ["v~a"]);
    assert_eq!(seen.lock().unwrap().len(), 2);
    // Turned off, nothing is sent again.
    let (url, seen) = scripted(vec![reply("429 Too Many Requests", "", busy)]).await;
    let off = Client::new(&url).unwrap().with_retry(RetryPolicy::none());
    assert_eq!(
        off.check("#check Nat").await.unwrap_err().kind(),
        Some(ErrorKind::Capacity)
    );
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_batch_sends_again_only_the_items_no_worker_took() {
    let item = |error: Option<&str>| match error {
        Some(kind) => format!(
            r#"{{"ok": false, "time": 0, "error": {{"kind": "{kind}", "message": ""}}, "declarations": []}}"#
        ),
        None => CHECKED.to_string(),
    };
    let first = format!(
        r#"{{"results": [{}, {}]}}"#,
        item(None),
        item(Some("capacity"))
    );
    let second = format!(r#"{{"results": [{}]}}"#, item(None));
    let (url, seen) = scripted(vec![
        reply("200 OK", "", &first),
        reply("200 OK", "", &second),
    ])
    .await;
    let results = eager(&url).check_batch(["a", "b"]).await.unwrap();
    assert!(results.iter().all(|r| r.ok), "{results:?}");
    let seen = seen.lock().unwrap().clone();
    let items: Vec<&serde_json::Value> = seen.iter().map(|r| &r.body["items"]).collect();
    assert_eq!(
        items,
        [
            &serde_json::json!([{"code": "a"}, {"code": "b"}]),
            &serde_json::json!([{"code": "b"}]),
        ]
    );
    assert!(
        seen.iter()
            .all(|r| r.method == "POST" && r.path == "/check/batch")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn client_names_each_kind_of_create() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let client = Client::new(&c.url("/")).unwrap().with_version("v");
    let opts = SpecOptions {
        holes: vec!["cost".into()],
        name: Some("named.spec".into()),
        ..Default::default()
    };
    let spec = "import Foo\ndef cost : Nat := sorry\ntheorem t : cost = cost := sorry\ntheorem w : 1 = 2 := sorry";
    let spec = client
        .create_spec(spec, &opts)
        .await
        .unwrap()
        .snapshot
        .unwrap();
    assert_eq!(spec.holes, ["cost"]);
    assert_eq!(spec.run.as_deref(), Some("named.spec"));

    // A proof onto the spec, then another sent onto the spec as well, which
    // lands on the first's head.
    let proof = |n: u32| {
        format!(
            "import Foo\ndef cost : Nat := {n}\ntheorem t : cost = cost := rfl\ntheorem w : 1 = 2 := sorry"
        )
    };
    let none = ProofOptions::default();
    let first = client
        .create_proof(&spec.id, &proof(1), &none)
        .await
        .unwrap();
    let first = first.snapshot.unwrap();
    let second = client
        .create_proof(&spec.id, &proof(2), &none)
        .await
        .unwrap();
    let second = second.snapshot.expect("onto the head");
    assert_eq!(second.replaces.unwrap().snapshot_id, first.id);
    // A branch, and then two heads: the caller chooses.
    let fork = ProofOptions {
        fork: true,
        ..Default::default()
    };
    client
        .create_proof(&first.id, &proof(3), &fork)
        .await
        .unwrap()
        .snapshot
        .unwrap();
    let refused = client
        .create_proof(&spec.id, &proof(4), &none)
        .await
        .unwrap();
    assert_eq!(refused.refused, Some(Refusal::Conflict), "{refused:?}");
    assert_eq!(refused.heads.len(), 2, "{refused:?}");

    // `w` is false: refute it, then amend the spec without it.
    let refuter = format!("{}\ntheorem r : ¬ (1 = 2) := by decide", spec.header);
    let none = RefuteOptions::default();
    let refuted = client.refute(&spec.id, "w", &refuter, &none).await.unwrap();
    assert_eq!(refuted.refutation.map(|r| r.theorem).as_deref(), Some("r"));
    let amended = "import Foo\ndef cost : Nat := sorry\ntheorem t : cost = cost := sorry";
    let none = AmendOptions::default();
    let amended = client
        .amend_spec(&spec.id, "w is false", amended, &none)
        .await
        .unwrap();
    assert_eq!(amended.amendment.unwrap().changes[0].name, "w");
    let amended = amended.snapshot.unwrap();
    // It keeps the amended spec's holes, and takes its run's name.
    assert_eq!(amended.holes, ["cost"]);
    assert_eq!(amended.run.as_deref(), Some("named.spec"));
}
