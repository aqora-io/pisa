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
use pisa_worker::queue::{NewJob, Poll};
use pisa_worker::records::Records;

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
        state: None,
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
    });
    match run(c, document, job).await {
        JobResult::DocumentProbes(Ok(p)) => p,
        other => panic!("{other:?}"),
    }
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
    // header has no goals.
    let g = goals(&c, &id, Some(1), &[(3, Some(16)), (4, None), (1, None)])
        .await
        .unwrap();
    let at: Vec<_> = g.at.iter().map(|a| (a.pos, a.goals.clone())).collect();
    assert_eq!(
        at,
        [
            (pos(3, 16), vec!["⊢ 𝓝 := by".to_string()]),
            (pos(4, 2), vec!["⊢ sorry".to_string()]),
            (pos(1, 0), vec![]),
        ]
    );
    let past = goals(&c, &id, None, &[(9, None)]).await.unwrap_err();
    assert_eq!(past.kind, ErrorKind::BadRequest);

    let p = probe(
        &c,
        &id,
        ProbeAt::Sorry { sorry: 0 },
        &["rfl", "omega", "#eval 1", "rfl -- sorry"],
    )
    .await;
    assert_eq!(p.pos, pos(4, 2));
    let r = &p.results;
    assert!(r[0].ok && r[0].closed && r[0].error.is_none(), "{:?}", r[0]);
    assert!(!r[1].ok && !r[1].closed);
    assert_eq!(r[1].messages[0].pos, pos(4, 2));
    assert_eq!(r[2].error.as_ref().unwrap().kind, ErrorKind::Policy);
    assert!(r[3].ok && !r[3].closed, "a sorry closes nothing");
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
