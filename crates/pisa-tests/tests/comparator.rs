//! A run as a Lake project for `lake comparator`: the modules its spec
//! snapshots and heads need, and a comparator configuration for each spec
//! snapshot and head it reaches.

mod common;

use std::time::Duration;

use common::Cluster;
use pisa_protocol::wire::{ExportedRun, Job, JobResult, SnapshotExportJob};
use pisa_worker::queue::{NewJob, Poll};
use serde_json::{Value, json};

async fn create(c: &Cluster, body: Value) -> Value {
    let (status, r) = c.post("/snapshots", body).await;
    assert_eq!(status, 200, "{r}");
    assert!(r["snapshot"]["id"].is_string(), "{r}");
    r
}

fn id(r: &Value) -> String {
    r["snapshot"]["id"].as_str().unwrap().to_string()
}

fn module(r: &Value) -> String {
    r["snapshot"]["module"].as_str().unwrap().to_string()
}

/// `header`, a blank line, then `body`.
fn on(snapshot: &Value, body: &str) -> String {
    format!(
        "{}\n\n{body}",
        snapshot["snapshot"]["header"].as_str().unwrap()
    )
}

/// Queues `job`, as a server would, and waits for its result.
async fn run(c: &Cluster, job: Job) -> JobResult {
    let new = NewJob {
        version: Some(c.version("v")),
        priority: 0,
        header_key: None,
        header: None,
        state: None,
        document: None,
        job: serde_json::to_value(job).unwrap(),
        max_wait: Duration::from_secs(10),
        max_attempts: 1,
    };
    let id = c.queue.enqueue(&new).await.unwrap();
    loop {
        match c.queue.poll(id).await.unwrap() {
            Poll::Done(result) => return serde_json::from_value(result).unwrap(),
            Poll::Waiting { .. } => tokio::time::sleep(Duration::from_millis(10)).await,
            other => panic!("{other:?}"),
        }
    }
}

async fn export(c: &Cluster, against: &[&str], allow: &[&str]) -> ExportedRun {
    let job = Job::SnapshotExport(SnapshotExportJob {
        against: against.iter().map(|s| s.to_string()).collect(),
        allow: allow.iter().map(|s| s.to_string()).collect(),
    });
    match run(c, job).await {
        JobResult::SnapshotExport(Ok(run)) => run,
        other => panic!("{other:?}"),
    }
}

fn text<'a>(run: &'a ExportedRun, path: &str) -> &'a str {
    let file = run.files.iter().find(|f| f.path == path);
    &file.unwrap_or_else(|| panic!("no {path}")).text
}

/// A spec with a hole and an assumption on a helper snapshot, and a head
/// that fills them on another helper.
#[tokio::test(flavor = "multi_thread")]
async fn a_run_exports_its_modules_and_a_configuration_per_pair() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let base = create(&c, json!({"code": "import Foo\n\ndef g : Nat := 1"})).await;
    let spec = on(
        &base,
        "def cost : Nat := sorry\naxiom lit : g = g\ntheorem t : cost = cost := sorry",
    );
    let s = create(&c, json!({"code": spec, "holes": ["cost"]})).await;
    let helper = create(
        &c,
        json!({"code": on(&base, "theorem help : 1 = 1 := rfl")}),
    )
    .await;
    let filled = on(
        &helper,
        "def cost : Nat := 3\naxiom lit : g = g\ntheorem t : cost = cost := rfl",
    );
    let p = create(
        &c,
        json!({"code": filled, "replaces": {"snapshot_id": id(&s)}}),
    )
    .await;

    let run = export(&c, &[&id(&s)], &["g"]).await;
    assert_eq!(run.pairs.len(), 1, "{:?}", run.pairs);
    let pair = &run.pairs[0];
    assert_eq!((&pair.spec, &pair.head), (&id(&s), &id(&p)));
    assert_eq!(pair.config, "comparator/1.json");
    let config: Value = serde_json::from_str(text(&run, &pair.config)).unwrap();
    assert_eq!(
        config,
        json!({
            "challenge_module": module(&s),
            "solution_module": module(&p),
            "theorem_names": ["t"],
            "definition_names": ["cost", "g"],
            "permitted_axioms": ["propext", "Quot.sound", "Classical.choice", "lit"],
        })
    );
    // Each module the spec and the head need, as its code was stored.
    let mut modules: Vec<&str> = run
        .files
        .iter()
        .filter_map(|f| f.path.strip_prefix("PisaSnap/"))
        .collect();
    modules.sort();
    let mut expected: Vec<String> = [&base, &s, &helper, &p]
        .iter()
        .map(|r| format!("{}.lean", module(r).trim_start_matches("PisaSnap.")))
        .collect();
    expected.sort();
    assert_eq!(modules, expected);
    let head_file = format!(
        "PisaSnap/{}.lean",
        module(&p).trim_start_matches("PisaSnap.")
    );
    assert_eq!(text(&run, &head_file), filled);
    // No project here: a lakefile that requires nothing, and a manifest.
    assert!(text(&run, "lakefile.toml").contains("autoImplicit = false"));
    assert!(text(&run, "lake-manifest.json").contains("\"pisa_run\""));
    assert!(text(&run, "README.md").contains(&format!("| `comparator/1.json` | `{}` |", id(&s))));
    assert!(run.forks.is_empty());
}
