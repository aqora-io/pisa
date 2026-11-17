//! Open documents on workers with Beam slots, on the fake Beam, which reads
//! the files the worker writes: each `sorry` is a warning and a todo item,
//! the goal at a position is the rest of its line, and `rfl` closes a goal.

mod common;

use std::time::Duration;

use common::{Cluster, beam_worker_config};
use pisa_protocol::api::{self, ErrorKind, GoalsMode, LinePos, ProbeAt};
use pisa_protocol::wire::{
    DocumentEditJob, DocumentGoalsJob, DocumentJob, DocumentOpenJob, DocumentProbeJob, Job,
    JobResult,
};
use pisa_worker::queue::{LIVE, NewJob, Poll};
use pisa_worker::records::Records;
use reqwest::Method;
use serde_json::json;

const CODE: &str = "import Foo\n\ntheorem t : 𝓝 = 𝓝 := by\n  sorry\n";

fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn document_job(c: &Cluster, document: &str, job: Job, max_wait: Duration) -> NewJob {
    NewJob {
        version: Some(c.version("v")),
        priority: 0,
        header_key: None,
        header: None,
        document: Some(document.into()),
        job: serde_json::to_value(job).unwrap(),
        max_wait,
        max_attempts: 1,
    }
}

/// Queues `job` on `document`, as a server would, and waits for its result.
async fn run(c: &Cluster, document: &str, job: Job) -> JobResult {
    let new = document_job(c, document, job, Duration::from_secs(10));
    let id = c.queue.enqueue(&new).await.unwrap();
    loop {
        match c.queue.poll(id).await.unwrap() {
            Poll::Done(result) => return serde_json::from_value(result).unwrap(),
            Poll::Waiting { .. } => tokio::time::sleep(Duration::from_millis(10)).await,
            other => panic!("{other:?}"),
        }
    }
}

async fn open(c: &Cluster, document: &str, code: &str) -> api::Document {
    let job = Job::DocumentOpen(DocumentOpenJob {
        document: document.into(),
        code: code.into(),
        timeout_secs: 10,
        runs: Vec::new(),
    });
    match run(c, document, job).await {
        JobResult::Document(Ok(d)) => d,
        other => panic!("{other:?}"),
    }
}

async fn goals(
    c: &Cluster,
    document: &str,
    revision: Option<u64>,
    at: &[(u64, Option<u64>)],
) -> Result<api::DocumentGoals, api::Error> {
    let job = Job::DocumentGoals(DocumentGoalsJob {
        document: document.into(),
        revision,
        at: at
            .iter()
            .map(|&(line, column)| LinePos { line, column })
            .collect(),
        mode: GoalsMode::Before,
        timeout_secs: 10,
    });
    match run(c, document, job).await {
        JobResult::DocumentGoals(r) => r,
        other => panic!("{other:?}"),
    }
}

async fn probe(
    c: &Cluster,
    document: &str,
    at: ProbeAt,
    candidates: &[&str],
) -> api::DocumentProbes {
    let job = Job::DocumentProbe(DocumentProbeJob {
        document: document.into(),
        revision: None,
        at,
        candidates: candidates.iter().map(|s| s.to_string()).collect(),
        timeout_secs: 10,
        budget_secs: 300,
    });
    match run(c, document, job).await {
        JobResult::DocumentProbes(Ok(p)) => p,
        other => panic!("{other:?}"),
    }
}

/// The documents each live worker has open, as the registry has them.
async fn documents_open(c: &Cluster) -> Vec<(String, u32)> {
    let peers = c.queue.registry(&c.version("v"), LIVE).await.unwrap();
    let mut open: Vec<_> = peers
        .into_iter()
        .map(|p| (p.name, p.documents_open))
        .collect();
    open.sort();
    open
}

async fn holder(c: &Cluster, document: &str) -> Option<String> {
    let records = Records::new(c.db.clone(), c.version("v"));
    records.document(document).await.unwrap()?.holder
}

fn pos(line: u64, column: u64) -> api::Pos {
    api::Pos { line, column }
}

#[tokio::test(flavor = "multi_thread")]
async fn goals_and_probes_land_where_the_code_says() {
    let c = Cluster::start_with(common::router_config(), &[("w0", "v", 1)], |cfg| {
        beam_worker_config(cfg.worker_id.as_deref().unwrap(), "v", 1)
    })
    .await;
    let id = new_id();
    let d = open(&c, &id, CODE).await;
    assert_eq!(d.id, format!("{}~{id}", c.version("v")));
    assert_eq!(d.revision, 1);
    // The `sorry` on line 4, below the header the worker adds.
    assert_eq!(d.messages.len(), 1, "{:?}", d.messages);
    assert_eq!(d.messages[0].pos, pos(4, 2));
    assert_eq!(d.sorries.len(), 1);
    assert_eq!(d.sorries[0].pos, Some(pos(4, 2)));
    assert_eq!(d.sorries[0].end_pos, Some(pos(4, 7)));

    // The second `𝓝` is column 16; a line alone is its first tactic; the
    // header, and a line outside any proof, have no goals.
    let g = goals(
        &c,
        &id,
        Some(1),
        &[(3, Some(16)), (4, None), (1, None), (2, None)],
    )
    .await
    .unwrap();
    let at: Vec<_> = g.at.iter().map(|a| (a.pos, a.goals.clone())).collect();
    assert_eq!(
        at,
        [
            (pos(3, 16), vec!["⊢ 𝓝 := by".to_string()]),
            (pos(4, 2), vec!["⊢ sorry".to_string()]),
            (pos(1, 0), vec![]),
            (pos(2, 0), vec![]),
        ]
    );
    // Each says the line it was asked at, and whether it is in a proof.
    let lines: Vec<_> = g.at.iter().map(|a| (a.line.as_str(), a.in_proof)).collect();
    assert_eq!(
        lines,
        [
            ("theorem t : 𝓝 = 𝓝 := by", true),
            ("  sorry", true),
            ("import Foo", false),
            ("", false)
        ]
    );
    // A finished proof has no goals, but is a proof.
    let done = new_id();
    open(
        &c,
        &done,
        "import Foo\n\ntheorem u : 1 = 1 := by\n  rfl -- done\n",
    )
    .await;
    let g = goals(&c, &done, None, &[(4, Some(6))]).await.unwrap();
    assert_eq!(
        (g.at[0].in_proof, g.at[0].goals.len(), g.at[0].line.as_str()),
        (true, 0, "  rfl -- done")
    );
    let past = goals(&c, &id, None, &[(9, None)]).await.unwrap_err();
    assert_eq!(past.kind, ErrorKind::BadRequest);

    let p = probe(
        &c,
        &id,
        ProbeAt::Sorry { sorry: 0 },
        &["rfl", "omega", "#eval 1", "rfl -- sorry", "FAIL"],
    )
    .await;
    assert_eq!(p.pos, pos(4, 2));
    let r = &p.results;
    assert!(r[0].ok && r[0].closed && r[0].error.is_none(), "{:?}", r[0]);
    assert!(!r[1].ok && !r[1].closed);
    assert_eq!(r[1].messages[0].pos, pos(4, 2));
    assert_eq!(r[2].error.as_ref().unwrap().kind, ErrorKind::Policy);
    assert!(r[3].ok && !r[3].closed, "a sorry closes nothing");
    // Beam's own failure, which isn't Lean's answer.
    assert_eq!(r[4].error.as_ref().unwrap().kind, ErrorKind::Unavailable);
    let at_line = probe(
        &c,
        &id,
        ProbeAt::Pos(LinePos {
            line: 4,
            column: None,
        }),
        &["rfl"],
    )
    .await;
    assert_eq!(at_line.pos, pos(4, 2));
}

/// A candidate that reaches one of Lean's limits has its own kind, and the
/// candidates after it still run.
#[tokio::test(flavor = "multi_thread")]
async fn lean_s_limits_are_errors_of_their_own_not_an_outage() {
    let c = Cluster::start_with(common::router_config(), &[("w0", "v", 1)], |cfg| {
        beam_worker_config(cfg.worker_id.as_deref().unwrap(), "v", 1)
    })
    .await;
    let id = new_id();
    open(&c, &id, CODE).await;
    let p = probe(
        &c,
        &id,
        ProbeAt::Sorry { sorry: 0 },
        &["RECURSION", "HEARTBEAT", "skip"],
    )
    .await;
    let r = &p.results;
    let deep = r[0].error.as_ref().unwrap();
    assert_eq!(deep.kind, ErrorKind::RecursionLimit);
    assert_eq!(
        serde_json::to_value(deep.limit.as_ref().unwrap()).unwrap(),
        json!({"option": "maxRecDepth", "value": 512})
    );
    let slow = r[1].error.as_ref().unwrap();
    assert_eq!(slow.kind, ErrorKind::HeartbeatLimit);
    assert_eq!(
        serde_json::to_value(slow.limit.as_ref().unwrap()).unwrap(),
        json!({"option": "maxHeartbeats", "value": 200000, "phase": "whnf"})
    );
    assert!(r[2].ok && r[2].error.is_none(), "{:?}", r[2]);
}

#[tokio::test(flavor = "multi_thread")]
async fn each_probe_candidate_has_its_own_time_limit() {
    let c = Cluster::start_with(common::router_config(), &[("w0", "v", 1)], |cfg| {
        beam_worker_config(cfg.worker_id.as_deref().unwrap(), "v", 1)
    })
    .await;
    let id = new_id();
    open(&c, &id, CODE).await;
    let job = Job::DocumentProbe(DocumentProbeJob {
        document: id.clone(),
        revision: None,
        at: ProbeAt::Sorry { sorry: 0 },
        candidates: vec!["SLEEP 3000".into(), "rfl".into()],
        timeout_secs: 1,
        budget_secs: 300,
    });
    let JobResult::DocumentProbes(Ok(p)) = run(&c, &id, job).await else {
        panic!("not probed");
    };
    let r = &p.results;
    assert_eq!(
        r[0].error.as_ref().map(|e| e.kind),
        Some(ErrorKind::Timeout)
    );
    // The first used up its second; the next still ran.
    assert!(r[1].ok && r[1].closed, "{:?}", r[1]);
}

#[tokio::test(flavor = "multi_thread")]
async fn three_timeouts_in_a_row_restart_lean_beam() {
    let c = Cluster::start_with(common::router_config(), &[("w0", "v", 1)], |cfg| {
        beam_worker_config(cfg.worker_id.as_deref().unwrap(), "v", 1)
    })
    .await;
    let id = new_id();
    open(&c, &id, CODE).await;
    let probe = |candidates: Vec<String>| {
        Job::DocumentProbe(DocumentProbeJob {
            document: id.clone(),
            revision: None,
            at: ProbeAt::Sorry { sorry: 0 },
            candidates,
            timeout_secs: 1,
            budget_secs: 300,
        })
    };
    // Two, then an answer: the count starts again.
    let slow = || "SLEEP 3000".to_string();
    run(&c, &id, probe(vec![slow(), slow(), "rfl".into()])).await;
    let g = goals(&c, &id, None, &[(4, None)]).await.unwrap();
    assert!(!g.reopened);
    // Three in a row: what timed out may still run, so Beam restarts, and
    // the document opens again in the new one.
    run(&c, &id, probe(vec![slow(), slow(), slow()])).await;
    let g = goals(&c, &id, None, &[(4, None)]).await.unwrap();
    assert!(g.reopened);
    assert_eq!(g.at[0].goals, ["⊢ sorry"]);
}

/// A probe of `candidates` at the first `sorry` of `document`, each with
/// `timeout` and all of them with `budget` seconds.
fn probes(document: &str, candidates: &[&str], timeout: u64, budget: u64) -> Job {
    Job::DocumentProbe(DocumentProbeJob {
        document: document.into(),
        revision: None,
        at: ProbeAt::Sorry { sorry: 0 },
        candidates: candidates.iter().map(|s| s.to_string()).collect(),
        timeout_secs: timeout,
        budget_secs: budget,
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_probe_request_goes_on_after_lean_beam_restarts() {
    let c = beam_cluster().await;
    let id = new_id();
    open(&c, &id, CODE).await;
    // The third timeout in a row restarts Beam: the document opens again in
    // the new one, and the rest of the candidates run there.
    let slow = "SLEEP 3000";
    let job = probes(&id, &["rfl", slow, slow, slow, "trivial"], 1, 300);
    let JobResult::DocumentProbes(Ok(p)) = run(&c, &id, job).await else {
        panic!("not probed")
    };
    let r = &p.results;
    assert!(r[0].closed, "{:?}", r[0]);
    for timed_out in &r[1..4] {
        let kind = timed_out.error.as_ref().map(|e| e.kind);
        assert_eq!(kind, Some(ErrorKind::Timeout), "{timed_out:?}");
    }
    assert!(r[4].closed, "{:?}", r[4]);
    assert!(p.reopened);
    // A candidate that Beam dies on is answered as a crash, and the others
    // still run.
    let job = probes(&id, &["rfl", "CRASH", "trivial"], 10, 300);
    let JobResult::DocumentProbes(Ok(p)) = run(&c, &id, job).await else {
        panic!("not probed")
    };
    let r = &p.results;
    assert!(r[0].closed && r[2].closed, "{r:?}");
    assert_eq!(r[1].error.as_ref().map(|e| e.kind), Some(ErrorKind::Crash));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_candidate_after_a_reopen_has_its_whole_limit() {
    let c = beam_cluster().await;
    let id = new_id();
    // Each sync of it takes a second and a half; its open had ten.
    open(&c, &id, &CODE.replace("\n\n", "\n-- SLOW_SYNC 1500\n")).await;
    // The third timeout restarts Beam, and the document opens again for the
    // last candidate, taking longer than a candidate's limit, which starts
    // after it.
    let slow = "SLEEP 3000";
    let job = probes(&id, &[slow, slow, slow, "trivial"], 1, 300);
    let JobResult::DocumentProbes(Ok(p)) = run(&c, &id, job).await else {
        panic!("not probed")
    };
    assert!(p.reopened);
    assert!(p.results[3].closed, "{:?}", p.results[3]);
}

/// Beam restarted after three timeouts in a row on one document: a request
/// in flight on another of its documents says so, rather than that Beam
/// crashed, as nothing it sent did.
#[tokio::test(flavor = "multi_thread")]
async fn a_restart_reads_as_one_on_the_slots_other_documents() {
    let c = beam_cluster().await;
    let (a, b) = (new_id(), new_id());
    open(&c, &a, CODE).await;
    open(&c, &b, CODE).await;
    let slow = "SLEEP 3000";
    let on_a = probes(&a, &["SLEEP 4000"], 10, 300);
    let on_b = probes(&b, &[slow, slow, slow], 1, 300);
    let (on_a, _) = tokio::join!(run(&c, &a, on_a), async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        run(&c, &b, on_b).await
    });
    let JobResult::DocumentProbes(Ok(p)) = on_a else {
        panic!("not probed: {on_a:?}")
    };
    let e = p.results[0].error.as_ref().unwrap();
    assert_eq!(e.kind, ErrorKind::Unavailable, "{e:?}");
    assert!(e.message.contains("restarted"), "{e:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_probe_request_has_a_budget_for_all_its_candidates() {
    let c = beam_cluster().await;
    let id = new_id();
    open(&c, &id, CODE).await;
    let job = probes(&id, &["SLEEP 3000", "rfl"], 10, 1);
    let JobResult::DocumentProbes(Ok(p)) = run(&c, &id, job).await else {
        panic!("not probed")
    };
    let r = &p.results;
    assert_eq!(
        r[0].error.as_ref().map(|e| e.kind),
        Some(ErrorKind::Timeout)
    );
    let ran_out = r[1].error.as_ref().unwrap();
    assert_eq!(ran_out.kind, ErrorKind::Timeout);
    assert!(ran_out.message.contains("budget ran out"), "{ran_out:?}");
    // The API takes at most an hour.
    let path = format!("/documents/{}~{id}/probes", c.version("v"));
    let body = json!({"at": {"sorry": 0}, "candidates": ["rfl"], "budget": 3601});
    let (status, e) = c.post(&path, body).await;
    assert_eq!(status, 400, "{e}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_edit_is_to_a_revision_and_a_closed_document_is_gone() {
    let c = Cluster::start_with(common::router_config(), &[("w0", "v", 1)], |cfg| {
        beam_worker_config(cfg.worker_id.as_deref().unwrap(), "v", 1)
    })
    .await;
    let id = new_id();
    open(&c, &id, CODE).await;
    let edit = |revision| {
        Job::DocumentEdit(DocumentEditJob {
            document: id.clone(),
            code: "theorem t : 1 = 1 := by\n  rfl\n".into(),
            revision,
            timeout_secs: 10,
            runs: Vec::new(),
        })
    };
    let JobResult::Document(Ok(d)) = run(&c, &id, edit(1)).await else {
        panic!("edit failed")
    };
    assert_eq!((d.revision, d.sorries.len()), (2, 0));
    let JobResult::Document(Err(stale)) = run(&c, &id, edit(1)).await else {
        panic!("a stale edit went through")
    };
    assert_eq!(stale.kind, ErrorKind::Conflict);
    let stale = goals(&c, &id, Some(1), &[(1, None)]).await.unwrap_err();
    assert_eq!(stale.kind, ErrorKind::Conflict);
    let g = goals(&c, &id, Some(2), &[(2, None)]).await.unwrap();
    assert_eq!(g.at[0].goals, ["⊢ rfl"]);

    let close = || {
        Job::DocumentClose(DocumentJob {
            document: id.clone(),
        })
    };
    assert!(matches!(
        run(&c, &id, close()).await,
        JobResult::DocumentClosed(Ok(()))
    ));
    let JobResult::DocumentClosed(Err(gone)) = run(&c, &id, close()).await else {
        panic!("closed twice")
    };
    assert_eq!(gone.kind, ErrorKind::NotFound);
    let gone = goals(&c, &id, None, &[(1, None)]).await.unwrap_err();
    assert_eq!(gone.kind, ErrorKind::NotFound);
    assert!(c.stored_rows("v").await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_document_reopens_on_another_worker_when_its_holder_is_gone() {
    let mut c = Cluster::start_with(common::router_config(), &[("a", "v", 1)], |cfg| {
        beam_worker_config(cfg.worker_id.as_deref().unwrap(), "v", 1)
    })
    .await;
    let id = new_id();
    open(&c, &id, CODE).await;
    c.add_worker(beam_worker_config("b", "v", 1));
    c.wait_for_workers(2).await;
    // While it is live, the holder runs every job on it.
    for _ in 0..3 {
        goals(&c, &id, None, &[(4, None)]).await.unwrap();
        assert_eq!(holder(&c, &id).await.as_deref(), Some("a"));
    }
    c.kill_worker(0).await;
    let g = goals(&c, &id, Some(1), &[(4, None)]).await.unwrap();
    assert_eq!(g.at[0].goals, ["⊢ sorry"]);
    assert_eq!(holder(&c, &id).await.as_deref(), Some("b"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_document_closed_to_make_room_reopens_where_there_is_room() {
    let one_each = |name: &str| {
        let mut cfg = beam_worker_config(name, "v", 1);
        cfg.beam_documents = 1;
        cfg
    };
    let mut c = Cluster::start_with(common::router_config(), &[("a", "v", 1)], |cfg| {
        one_each(cfg.worker_id.as_deref().unwrap())
    })
    .await;
    let (first, second) = (new_id(), new_id());
    open(&c, &first, CODE).await;
    // `a` has room for one: it closes the first to open the second, and
    // no longer holds the first.
    open(&c, &second, CODE).await;
    assert_eq!(holder(&c, &first).await, None);
    assert_eq!(holder(&c, &second).await.as_deref(), Some("a"));
    // So it opens again on the worker with room, not where it was.
    c.add_worker(one_each("b"));
    c.wait_for_workers(2).await;
    let g = goals(&c, &first, None, &[(4, None)]).await.unwrap();
    assert!(g.reopened);
    assert_eq!(holder(&c, &first).await.as_deref(), Some("b"));
    assert_eq!(holder(&c, &second).await.as_deref(), Some("a"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_error_after_a_reopen_says_the_document_opened_again() {
    let mut c = beam_cluster().await;
    let id = new_id();
    open(&c, &id, &format!("{CODE}  FAIL_GOALS\n")).await;
    let e = goals(&c, &id, None, &[(5, None)]).await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Unavailable);
    assert!(!e.message.contains("opened again"), "{e:?}");
    c.kill_worker(0).await;
    c.add_worker(beam_worker_config("w0", "v", 1));
    c.wait_for_workers(1).await;
    let e = goals(&c, &id, None, &[(5, None)]).await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Unavailable);
    assert!(e.message.contains("opened again"), "{e:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reopen_that_fails_says_so() {
    let mut c = beam_cluster().await;
    let (status, s) = c
        .post("/snapshots", json!({"code": "theorem s : 1 = 1 := rfl"}))
        .await;
    assert_eq!(status, 200, "{s}");
    let module = s["snapshot"]["module"].as_str().unwrap();
    let id = new_id();
    open(
        &c,
        &id,
        &CODE.replace("import Foo", &format!("import {module}")),
    )
    .await;
    let snapshot = s["snapshot"]["id"].as_str().unwrap();
    let path = format!("/snapshots/{snapshot}");
    assert_eq!(c.request(Method::DELETE, &path, None).await.0, 200);
    c.kill_worker(0).await;
    c.add_worker(beam_worker_config("w0", "v", 1));
    c.wait_for_workers(1).await;
    // The code imports a snapshot that is gone, so it can't open again.
    let e = goals(&c, &id, None, &[(4, None)]).await.unwrap_err();
    assert!(e.message.contains("opening the document again"), "{e:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restarted_holder_reopens_its_documents() {
    let mut c = Cluster::start_with(common::router_config(), &[("a", "v", 1)], |cfg| {
        beam_worker_config(cfg.worker_id.as_deref().unwrap(), "v", 1)
    })
    .await;
    let id = new_id();
    open(&c, &id, CODE).await;
    c.kill_worker(0).await;
    c.add_worker(beam_worker_config("a", "v", 1));
    c.wait_for_workers(1).await;
    let g = goals(&c, &id, None, &[(4, None)]).await.unwrap();
    assert_eq!(
        (g.revision, g.at[0].goals.clone()),
        (1, vec!["⊢ sorry".to_string()])
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reopen_takes_the_time_its_open_took_and_says_so() {
    let mut c = Cluster::start_with(common::router_config(), &[("a", "v", 1)], |cfg| {
        beam_worker_config(cfg.worker_id.as_deref().unwrap(), "v", 1)
    })
    .await;
    let id = new_id();
    // Each sync of it takes a second and a half; its open had ten.
    let code = CODE.replace("\n\n", "\n-- SLOW_SYNC 1500\n");
    open(&c, &id, &code).await;
    let quick = |line| {
        Job::DocumentGoals(DocumentGoalsJob {
            document: id.clone(),
            revision: None,
            at: vec![LinePos { line, column: None }],
            mode: GoalsMode::Before,
            timeout_secs: 1,
        })
    };
    let JobResult::DocumentGoals(Ok(g)) = run(&c, &id, quick(4)).await else {
        panic!("no goals")
    };
    assert!(!g.reopened);

    c.kill_worker(0).await;
    c.add_worker(beam_worker_config("a", "v", 1));
    c.wait_for_workers(1).await;
    // The reopen takes longer than the request's second, within the open's.
    let JobResult::DocumentGoals(Ok(g)) = run(&c, &id, quick(4)).await else {
        panic!("the reopen took the request's time")
    };
    assert!(g.reopened);
    assert_eq!(g.at[0].goals, ["⊢ sorry"]);
    let JobResult::DocumentGoals(Ok(g)) = run(&c, &id, quick(4)).await else {
        panic!("no goals")
    };
    assert!(!g.reopened);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_holder_that_shuts_down_hands_its_documents_over() {
    let mut c = Cluster::start(&[]).await;
    let drain = c.add_drainable_worker(beam_worker_config("a", "v", 1));
    c.wait_for_workers(1).await;
    let id = new_id();
    open(&c, &id, CODE).await;
    c.add_worker(beam_worker_config("b", "v", 1));
    c.wait_for_workers(2).await;
    drain.send(()).unwrap();
    c.wait_for_workers(1).await;
    goals(&c, &id, None, &[(4, None)]).await.unwrap();
    assert_eq!(holder(&c, &id).await.as_deref(), Some("b"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_worker_without_beam_takes_no_job_on_a_document() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let id = new_id();
    let job = Job::DocumentClose(DocumentJob {
        document: id.clone(),
    });
    let new = document_job(&c, &id, job, Duration::from_secs(60));
    let job = c.queue.enqueue(&new).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(matches!(
        c.queue.poll(job).await.unwrap(),
        Poll::Waiting {
            give_up_at: Some(_)
        }
    ));
    assert_eq!(c.queue.give_up(job).await.unwrap(), Some(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn opens_go_where_fewest_documents_are_open() {
    let mut c = Cluster::start_with(common::router_config(), &[("a", "v", 1)], |cfg| {
        beam_worker_config(cfg.worker_id.as_deref().unwrap(), "v", 1)
    })
    .await;
    c.add_worker(beam_worker_config("b", "v", 1));
    c.wait_for_workers(2).await;
    let ids: Vec<String> = (0..4).map(|_| new_id()).collect();
    let mut holders = Vec::new();
    for id in &ids {
        open(&c, id, CODE).await;
        holders.push(holder(&c, id).await.unwrap());
    }
    // Each open after the first goes to the worker with fewer open, or to
    // either when they have as many.
    assert_ne!(holders[0], holders[1], "{holders:?}");
    assert_ne!(holders[2], holders[3], "{holders:?}");
    assert_eq!(
        documents_open(&c).await,
        [("a".to_string(), 2), ("b".to_string(), 2)]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn opens_at_once_keep_to_the_slots_cap() {
    let c = Cluster::start_with(common::router_config(), &[("w0", "v", 1)], |cfg| {
        pisa_worker::WorkerConfig {
            beam_documents: 2,
            ..beam_worker_config(cfg.worker_id.as_deref().unwrap(), "v", 1)
        }
    })
    .await;
    open(&c, &new_id(), CODE).await;
    // Two more at once, each slow to open: the second counts the first's
    // place, and closes the idle one.
    let slow = CODE.replace("\n\n", "\n-- SLOW_SYNC 500\n");
    let opens = (0..2).map(|_| {
        let (c, slow, id) = (&c, &slow, new_id());
        async move { open(c, &id, slow).await }
    });
    futures::future::join_all(opens).await;
    assert_eq!(documents_open(&c).await, [("w0".to_string(), 2)]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_full_slot_closes_its_least_recently_used_document() {
    let c = Cluster::start_with(common::router_config(), &[("w0", "v", 1)], |cfg| {
        pisa_worker::WorkerConfig {
            beam_documents: 2,
            ..beam_worker_config(cfg.worker_id.as_deref().unwrap(), "v", 1)
        }
    })
    .await;
    let ids: Vec<String> = (0..3).map(|_| new_id()).collect();
    open(&c, &ids[0], CODE).await;
    open(&c, &ids[1], CODE).await;
    // The first is the least recently used once the second is asked about.
    goals(&c, &ids[0], None, &[(4, None)]).await.unwrap();
    goals(&c, &ids[1], None, &[(4, None)]).await.unwrap();
    let workspace = c.dir.path().join("w0/slot-1/docs/PisaDocs");
    let file = |id: &str| workspace.join(format!("D{id}.lean"));
    open(&c, &ids[2], CODE).await;
    assert!(!file(&ids[0]).exists(), "the least recently used is closed");
    assert!(file(&ids[1]).exists() && file(&ids[2]).exists());
    // It reopens when asked about, and closes the next least recently used.
    let g = goals(&c, &ids[0], None, &[(4, None)]).await.unwrap();
    assert_eq!(g.at[0].goals, ["⊢ sorry"]);
    assert!(file(&ids[0]).exists() && !file(&ids[1]).exists());
}

async fn beam_cluster() -> Cluster {
    Cluster::start_with(common::router_config(), &[("w0", "v", 1)], |cfg| {
        beam_worker_config(cfg.worker_id.as_deref().unwrap(), "v", 1)
    })
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_api_opens_asks_edits_and_closes_a_document() {
    let c = beam_cluster().await;
    let (status, d) = c
        .post("/documents", json!({"code": CODE, "version": "v"}))
        .await;
    assert_eq!(status, 200, "{d}");
    let id = d["id"].as_str().unwrap().to_string();
    assert_eq!(d["revision"], 1);
    assert_eq!(d["sorries"][0]["pos"], json!({"line": 4, "column": 2}));

    let path = |p: &str| format!("/documents/{id}{p}");
    let (status, g) = c.post(&path("/goals"), json!({"at": [{"line": 4}]})).await;
    assert_eq!(status, 200, "{g}");
    assert_eq!(g["at"][0]["pos"], json!({"line": 4, "column": 2}));
    assert_eq!(g["at"][0]["goals"], json!(["⊢ sorry"]));
    let probes = json!({"at": {"sorry": 0}, "candidates": ["rfl", "omega"]});
    let (status, p) = c.post(&path("/probes"), probes).await;
    assert_eq!(status, 200, "{p}");
    assert_eq!(
        (&p["results"][0]["closed"], &p["results"][1]["ok"]),
        (&json!(true), &json!(false))
    );

    let edit = |revision| json!({"code": "theorem t : 1 = 1 := by\n  rfl\n", "revision": revision});
    let (status, d) = c.request(Method::PATCH, &path(""), Some(edit(1))).await;
    assert_eq!((status, &d["revision"]), (200, &json!(2)), "{d}");
    let (status, e) = c.request(Method::PATCH, &path(""), Some(edit(1))).await;
    assert_eq!(
        (status, &e["error"]["kind"]),
        (409, &json!("conflict")),
        "{e}"
    );
    let (status, e) = c
        .post(&path("/goals"), json!({"revision": 1, "at": [{"line": 1}]}))
        .await;
    assert_eq!(status, 409, "{e}");

    assert_eq!(c.request(Method::DELETE, &path(""), None).await.0, 204);
    let (status, e) = c.request(Method::DELETE, &path(""), None).await;
    assert_eq!(
        (status, &e["error"]["kind"]),
        (404, &json!("not_found")),
        "{e}"
    );
    let (status, _) = c.post(&path("/goals"), json!({"at": [{"line": 1}]})).await;
    assert_eq!(status, 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn requests_out_of_bounds_are_bad_requests() {
    let c = beam_cluster().await;
    let (_, d) = c
        .post("/documents", json!({"code": CODE, "version": "v"}))
        .await;
    let id = d["id"].as_str().unwrap();
    let goals = format!("/documents/{id}/goals");
    let probes = format!("/documents/{id}/probes");
    let many: Vec<_> = (0..65).map(|_| json!({"line": 1})).collect();
    let rfl: Vec<_> = (0..65).map(|_| "rfl").collect();
    for (path, body) in [
        (goals.as_str(), json!({"at": []})),
        (&goals, json!({"at": many})),
        (&goals, json!({"at": [{"line": 0}]})),
        (&probes, json!({"at": {"sorry": 0}, "candidates": []})),
        (&probes, json!({"at": {"sorry": 0}, "candidates": rfl})),
        (&probes, json!({"at": {"line": 1}, "candidates": ["rfl"]})),
        (&probes, json!({"at": {"sorry": 3}, "candidates": ["rfl"]})),
        ("/documents/v~nope/goals", json!({"at": [{"line": 1}]})),
    ] {
        let (status, e) = c.post(path, body.clone()).await;
        assert_eq!(
            (status, &e["error"]["kind"]),
            (400, &json!("bad_request")),
            "{body}: {e}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn without_beam_documents_are_unavailable_and_checks_still_run() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let r = c
        .client
        .post(c.url("/documents"))
        .json(&json!({"code": CODE, "version": "v"}))
        .send()
        .await
        .unwrap();
    // It says when to try again.
    assert_eq!(r.status(), 503);
    assert_eq!(r.headers()["retry-after"], "2");
    let e: serde_json::Value = r.json().await.unwrap();
    assert_eq!(e["error"]["kind"], "unavailable", "{e}");
    let id = format!("{}~{}", c.version("v"), new_id());
    let (status, _) = c
        .post(
            &format!("/documents/{id}/goals"),
            json!({"at": [{"line": 1}]}),
        )
        .await;
    assert_eq!(status, 503);
    let (status, r) = c
        .post(
            "/check",
            json!({"code": "theorem t : True := trivial", "version": "v"}),
        )
        .await;
    assert_eq!(status, 200, "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn probes_and_goals_say_where_their_time_went() {
    let mut c = Cluster::start_with(common::router_config(), &[("a", "v", 1)], |cfg| {
        beam_worker_config(cfg.worker_id.as_deref().unwrap(), "v", 1)
    })
    .await;
    let id = new_id();
    let code = CODE.replace("\n\n", "\n-- SLOW_SYNC 700\n");
    open(&c, &id, &code).await;
    let JobResult::DocumentProbes(Ok(p)) =
        run(&c, &id, probes(&id, &["SLEEP 300", "rfl"], 10, 300)).await
    else {
        panic!("not probed")
    };
    assert!(p.timing.execution >= 0.3, "{:?}", p.timing);
    assert!(p.timing.load < 0.2, "{:?}", p.timing);
    assert!(p.timing.total >= p.timing.execution, "{:?}", p.timing);

    let g = goals(&c, &id, None, &[(4, None)]).await.unwrap();
    assert!(!g.reopened);
    assert!(g.timing.load < 0.2, "{:?}", g.timing);
    assert!(g.timing.tracing > 0.0, "{:?}", g.timing);

    c.kill_worker(0).await;
    c.add_worker(beam_worker_config("a", "v", 1));
    c.wait_for_workers(1).await;
    // The reopen syncs the code again, which takes its time.
    let g = goals(&c, &id, None, &[(4, None)]).await.unwrap();
    assert!(g.reopened);
    assert!(g.timing.load >= 0.7, "{:?}", g.timing);
    assert!(g.timing.total >= g.timing.load, "{:?}", g.timing);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_open_says_where_its_time_went() {
    let c = beam_cluster().await;
    let id = new_id();
    let code = CODE.replace("\n\n", "\n-- SLOW_SYNC 1500\n");
    let d = open(&c, &id, &code).await;
    assert!(d.timing.execution >= 1.5, "{:?}", d.timing);
    assert!(d.timing.load > 0.0, "{:?}", d.timing);
    assert!(d.timing.tracing >= 0.0, "{:?}", d.timing);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_open_past_its_budget_says_where_its_time_went() {
    let c = beam_cluster().await;
    let code = CODE.replace("\n\n", "\n-- SLOW_SYNC 3000\n");
    let (status, e) = c
        .post(
            "/documents",
            json!({"code": code, "version": "v", "timeout": 1}),
        )
        .await;
    assert_eq!(status, 422, "{e}");
    assert_eq!(e["error"]["kind"], "timeout", "{e}");
    let timing = &e["error"]["timing"];
    // The sync ran until the budget ran out.
    let execution = timing["execution"].as_f64().unwrap();
    assert!(execution >= 0.5, "{e}");
    assert!(timing["load"].as_f64().unwrap() > 0.0, "{e}");
    assert!(timing["total"].as_f64().unwrap() >= execution, "{e}");
}
