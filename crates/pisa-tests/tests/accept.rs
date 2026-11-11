//! A run as a Lake project for `lake comparator`: the modules its spec
//! snapshots and heads need, and a comparator configuration for each spec
//! snapshot and head it reaches; and its acceptance, which audits it and
//! runs comparator on the pairs the audit compared, and is stored.

mod common;

use std::time::Duration;

use common::Cluster;
use pisa_protocol::api::{AcceptResponse, LeftOut, LeftOutReason, PairErrorKind};
use pisa_protocol::wire::{ExportedRun, Job, JobResult, SnapshotAcceptJob, SnapshotExportJob};
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
    let readme = text(&run, "README.md");
    let row = format!("| `comparator/1.json` | `{}` | `{}` | 1 |", id(&s), id(&p));
    assert!(readme.contains(&row), "{readme}");
    // Each file says which snapshots are on its module.
    assert!(
        readme.contains(&format!("| `{head_file}` | `{}` |", id(&p))),
        "{readme}"
    );
    // No project here: no toolchain, no Mathlib.
    assert!(!readme.contains("lake exe cache get"), "{readme}");
    assert!(run.forks.is_empty());
}

async fn accept(
    c: &Cluster,
    against: &[&str],
    allow: &[&str],
    paranoid: bool,
    timeout_secs: u64,
) -> AcceptResponse {
    let job = Job::SnapshotAccept(SnapshotAcceptJob {
        against: against.iter().map(|s| s.to_string()).collect(),
        allow: allow.iter().map(|s| s.to_string()).collect(),
        paranoid,
        timeout_secs,
    });
    match run(c, job).await {
        JobResult::SnapshotAccepted(Ok(r)) => *r,
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn accept_judges_each_spec_snapshot_against_its_head() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let s = create(
        &c,
        json!({"code": "import Foo\n\ntheorem t : 2 = 2 := sorry"}),
    )
    .await;
    // Nothing replaces it yet: it is its own head, and its sorry is refused.
    let r = accept(&c, &[&id(&s)], &[], true, 60).await;
    assert!(!r.accepted && !r.clean, "{r:?}");
    assert_eq!(r.heads, [id(&s)]);
    let pair = &r.pairs[0];
    assert_eq!(
        (&pair.spec, &pair.head, pair.exit_code),
        (&id(&s), &id(&s), Some(1))
    );
    assert!(pair.output.contains("sorryAx"), "{}", pair.output);
    let error = pair.error.as_ref().unwrap();
    assert_eq!(error.kind, PairErrorKind::Refused);
    assert_eq!(error.message, "Illegal axiom detected: 'sorryAx'");
    assert!(
        pair.output
            .contains("comparator --config comparator/1.json --paranoid"),
        "{}",
        pair.output
    );

    let p = create(
        &c,
        json!({"code": "import Foo\n\ntheorem t : 2 = 2 := rfl", "replaces": {"snapshot_id": id(&s)}}),
    )
    .await;
    let r = accept(&c, &[&id(&s)], &[], false, 60).await;
    assert!(r.accepted && r.clean, "{r:?}");
    assert_eq!((&r.heads, r.audit.clean), (&vec![id(&p)], Some(true)));
    let pair = &r.pairs[0];
    assert_eq!((&pair.head, pair.exit_code), (&id(&p), Some(0)));
    assert!(!pair.output.contains("--paranoid"), "{}", pair.output);
    assert!(pair.output.contains("Built PisaSnap.s"), "{}", pair.output);
    // Judged again, the run isn't built again.
    let again = accept(&c, &[&id(&s)], &[], false, 60).await;
    let output = &again.pairs[0].output;
    assert!(again.accepted, "{again:?}");
    assert!(
        output.contains("Replayed PisaSnap.s") && !output.contains("Built"),
        "{output}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fork_or_a_pair_out_of_time_is_not_accepted() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let s = create(
        &c,
        json!({"code": "import Foo\n\ntheorem t : 2 = 2 := sorry"}),
    )
    .await;
    for proof in ["rfl", "by rfl"] {
        let code = format!("import Foo\n\ntheorem t : 2 = 2 := {proof}");
        create(
            &c,
            json!({"code": code, "replaces": {"snapshot_id": id(&s)}, "fork": true}),
        )
        .await;
    }
    let r = accept(&c, &[&id(&s)], &[], false, 60).await;
    assert_eq!(r.pairs.len(), 2, "{r:?}");
    assert!(r.pairs.iter().all(|p| p.accepted), "{r:?}");
    assert_eq!(r.audit.forks[0].snapshot_id, id(&s));
    assert!(!r.accepted && !r.clean, "a fork isn't accepted");

    let slow = create(
        &c,
        json!({"code": "import Foo\n\n-- SLEEP\ntheorem u : 3 = 3 := rfl"}),
    )
    .await;
    let r = accept(&c, &[&id(&slow)], &[], false, 1).await;
    assert!(!r.accepted, "{r:?}");
    assert_eq!(r.pairs[0].exit_code, None);
    assert_eq!(
        r.pairs[0].error.as_ref().unwrap().kind,
        PairErrorKind::Timeout
    );
    assert!(
        r.pairs[0].output.contains("more than 1s"),
        "{}",
        r.pairs[0].output
    );
}

/// The files of the tar.gz `bytes`, by path.
fn unpack(bytes: &[u8]) -> std::collections::BTreeMap<String, String> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    let mut files = std::collections::BTreeMap::new();
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let path = entry.path().unwrap().to_string_lossy().into_owned();
        let mut text = String::new();
        std::io::Read::read_to_string(&mut entry, &mut text).unwrap();
        files.insert(path, text);
    }
    files
}

#[tokio::test(flavor = "multi_thread")]
async fn the_api_exports_a_run_and_judges_it() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let s = create(
        &c,
        json!({"code": "import Foo\n\ntheorem t : 2 = 2 := sorry"}),
    )
    .await;
    let p = create(
        &c,
        json!({"code": "import Foo\n\ntheorem t : 2 = 2 := rfl", "replaces": {"snapshot_id": id(&s)}}),
    )
    .await;

    let r = c
        .client
        .get(c.url(&format!("/snapshots/export?against={}", id(&s))))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "application/gzip");
    assert!(
        r.headers()["content-disposition"]
            .to_str()
            .unwrap()
            .contains("pisa-run.tar.gz")
    );
    let files = unpack(&r.bytes().await.unwrap());
    let config: Value = serde_json::from_str(&files["pisa-run/comparator/1.json"]).unwrap();
    assert_eq!(config["solution_module"], module(&p));
    for path in [
        "pisa-run/lakefile.toml",
        "pisa-run/lake-manifest.json",
        "pisa-run/README.md",
    ] {
        assert!(files.contains_key(path), "{:?}", files.keys());
    }

    // As a job to read back later.
    let r = c
        .client
        .post(c.url("/snapshots/accept"))
        .header("prefer", "respond-async")
        .json(&json!({"against": [id(&s)], "paranoid": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 202);
    let job: Value = r.json().await.unwrap();
    assert_eq!(job["kind"], "accept_run");
    let r = c
        .client
        .get(c.url(&format!("/jobs/{}?wait=30", job["id"].as_str().unwrap())))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let verdict: Value = r.json().await.unwrap();
    assert_eq!(verdict["accepted"], true, "{verdict}");
    assert_eq!(verdict["pairs"][0]["head"], json!(id(&p)), "{verdict}");
    // It is kept.
    let id = verdict["id"].as_str().unwrap();
    assert!(id.starts_with(&format!("{}~", c.version("v"))), "{id}");
    let r = c
        .client
        .get(c.url(&format!("/snapshots/accept/{id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let stored: Value = r.json().await.unwrap();
    assert_eq!(stored, verdict);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_is_named_by_spec_snapshots_of_one_version() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let get = async |query: &str| {
        let r = c
            .client
            .get(c.url(&format!("/snapshots/export{query}")))
            .send()
            .await
            .unwrap();
        (r.status().as_u16(), r.json::<Value>().await.unwrap())
    };
    let hex = "0123456789abcdef0123456789abcdef";
    let v = c.version("v");
    for (query, status) in [
        ("".to_string(), 400),
        (format!("?against={v}~{hex}&against=w~{hex}"), 400),
        ("?against=nope".to_string(), 400),
        (format!("?against={v}~{hex}"), 404),
    ] {
        let (got, e) = get(&query).await;
        assert_eq!(got, status, "{query}: {e}");
    }
    for body in [
        json!({"against": []}),
        json!({"against": [format!("{v}~{hex}")], "allow": [""]}),
    ] {
        let (status, e) = c.post("/snapshots/accept", body.clone()).await;
        assert_eq!(
            (status, &e["error"]["kind"]),
            (400, &json!("bad_request")),
            "{body}: {e}"
        );
    }
    // A stored acceptance is named like a snapshot, and comparator alone
    // has no path any more.
    for (path, status) in [
        ("/snapshots/accept/nope".to_string(), 400),
        (format!("/snapshots/accept/{v}~{hex}"), 404),
    ] {
        let r = c.client.get(c.url(&path)).send().await.unwrap();
        assert_eq!(r.status(), status, "{path}");
    }
    let old = c
        .client
        .post(c.url("/snapshots/comparator"))
        .json(&json!({"against": [format!("{v}~{hex}")]}))
        .send()
        .await
        .unwrap();
    // `/snapshots/{id}` takes no POST.
    assert_eq!(old.status(), 405);
}

/// A pair not accepted says why: what comparator refused, a module that
/// didn't build, comparator that couldn't run, or a spec with nothing to
/// judge, for which it doesn't run.
#[tokio::test(flavor = "multi_thread")]
async fn a_pair_not_accepted_says_why() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let kind = |r: &AcceptResponse| r.pairs[0].error.as_ref().map(|e| e.kind);
    let spec = create(
        &c,
        json!({"code": "import Foo\n\ntheorem t : 2 = 2 := sorry"}),
    )
    .await;
    for (marker, expected) in [
        ("BUILD_FAIL", PairErrorKind::BuildFailed),
        ("CANNOT_RUN", PairErrorKind::CannotRun),
    ] {
        let code = format!("import Foo\n\n-- {marker}\ntheorem t : 2 = 2 := rfl");
        let head = create(
            &c,
            json!({"code": code, "replaces": {"snapshot_id": id(&spec)}}),
        )
        .await;
        let r = accept(&c, &[&id(&spec)], &[], false, 60).await;
        // The audit is clean: only comparator didn't accept.
        assert!(r.clean && !r.accepted, "{r:?}");
        assert_eq!(kind(&r), Some(expected), "{r:?}");
        let deleted = c
            .client
            .delete(c.url(&format!("/snapshots/{}", id(&head))))
            .send()
            .await
            .unwrap();
        assert_eq!(deleted.status(), 200);
    }

    // A spec of definitions only names no theorem.
    let defs = create(&c, json!({"code": "import Foo\n\ndef d : Nat := 1"})).await;
    let r = accept(&c, &[&id(&defs)], &[], false, 60).await;
    assert!(!r.accepted, "{r:?}");
    assert_eq!(kind(&r), Some(PairErrorKind::NothingToJudge));
    assert_eq!((r.pairs[0].theorems, r.pairs[0].exit_code), (0, None));
}

/// A hole that isn't a plain definition, and an `allow` name the head
/// doesn't have, are left out of what comparator compares, and listed: the
/// pair is judged on the rest.
#[tokio::test(flavor = "multi_thread")]
async fn what_comparator_cant_take_as_a_hole_is_left_out() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let base = create(&c, json!({"code": "import Foo\n\ndef g : Nat := 1"})).await;
    let spec = on(
        &base,
        "structure S where\n  x : Nat\ndef cost : Nat := sorry\ntheorem t : cost = cost := sorry",
    );
    let s = create(&c, json!({"code": spec, "holes": ["cost", "S"]})).await;
    // The head no longer builds on `g`'s snapshot.
    let filled = "import Foo\n\nstructure S where\n  x : Nat\ndef cost : Nat := 3\n\
                  theorem t : cost = cost := rfl";
    create(
        &c,
        json!({"code": filled, "replaces": {"snapshot_id": id(&s)}}),
    )
    .await;
    let r = accept(&c, &[&id(&s)], &["g"], false, 60).await;
    let pair = &r.pairs[0];
    assert!(pair.accepted, "{r:?}");
    let left_out = |name: &str, reason| LeftOut {
        name: name.into(),
        reason,
    };
    assert_eq!(
        pair.left_out,
        [
            left_out("S", LeftOutReason::NotADefinitionInSpec),
            left_out("g", LeftOutReason::NotADefinitionInHead),
        ]
    );
    assert_eq!(pair.theorems, 1);
}
