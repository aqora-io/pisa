//! A run as a Lake project for `lake comparator`: the modules its spec
//! snapshots and heads need, and a comparator configuration for each spec
//! snapshot and head it reaches; and its acceptance, which audits it and
//! runs comparator on the pairs the audit compared, and is stored.

mod common;

use std::time::Duration;

use common::Cluster;
use pisa_protocol::api::{AcceptResponse, KernelOutcome, LeftOut, LeftOutReason, PairErrorKind};
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

/// A pair of a named run is named by the run: its head `PisaRun.<name>`,
/// its spec `PisaSpec.<name>`. What the pairs import keeps its hash name,
/// even when it is a named run's, and a fork's heads keep theirs.
#[tokio::test(flavor = "multi_thread")]
async fn a_named_run_exports_its_files_by_name() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let base = create(
        &c,
        json!({"code": "import Foo\n\ndef g : Nat := 1", "name": "base"}),
    )
    .await;
    let spec = "import PisaRun.base\n\ndef cost : Nat := sorry\ntheorem t : cost = cost := sorry";
    let s = create(&c, json!({"code": spec, "holes": ["cost"], "name": "main"})).await;
    let filled = "import PisaRun.base\n\ndef cost : Nat := g\ntheorem t : cost = cost := rfl";
    let replaces = json!({"snapshot_id": id(&s)});
    let p = create(&c, json!({"code": filled, "replaces": replaces})).await;

    let run = export(&c, &[&id(&s)], &[]).await;
    let mut paths: Vec<&str> = run
        .files
        .iter()
        .map(|f| f.path.as_str())
        .filter(|p| p.ends_with(".lean"))
        .collect();
    paths.sort();
    let base_file = format!("{}.lean", module(&base).replace('.', "/"));
    assert_eq!(
        paths,
        ["PisaRun/main.lean", &base_file, "PisaSpec/main.lean"]
    );
    let by_hash = |code: &str| code.replace("PisaRun.base", &module(&base));
    assert_eq!(text(&run, "PisaSpec/main.lean"), by_hash(spec));
    assert_eq!(text(&run, "PisaRun/main.lean"), by_hash(filled));
    let config: Value = serde_json::from_str(text(&run, &run.pairs[0].config)).unwrap();
    assert_eq!(
        (&config["challenge_module"], &config["solution_module"]),
        (&json!("PisaSpec.main"), &json!("PisaRun.main"))
    );
    let lakefile = text(&run, "lakefile.toml");
    assert!(
        lakefile.contains("defaultTargets = [\"PisaRun\", \"PisaSnap\", \"PisaSpec\"]"),
        "{lakefile}"
    );
    let readme = text(&run, "README.md");
    let row = format!("| `PisaRun/main.lean` | `{}` |", id(&p));
    assert!(readme.contains(&row), "{readme}");
    assert!(
        readme.contains(&format!("| `{base_file}` | `{}` |", id(&base))),
        "{readme}"
    );
    assert!(readme.contains("a renamed build"), "{readme}");
    // Acceptance runs comparator on these files, and says what it judged.
    let verdict = accept(&c, &[&id(&s)], &[], true, 60).await;
    let pair = &verdict.pairs[0];
    assert!(pair.accepted, "{verdict:?}");
    assert_eq!(
        (
            pair.challenge_module.as_str(),
            pair.solution_module.as_str()
        ),
        ("PisaSpec.main", "PisaRun.main")
    );
    assert_eq!(pair.spec_module, module(&s));

    // Forked, the run's heads keep their hash names; its spec keeps its.
    let forked = format!("{filled}\n-- forked");
    let f = json!({"code": forked, "replaces": replaces, "fork": true});
    let f = create(&c, f).await;
    let run = export(&c, &[&id(&s)], &[]).await;
    let mut solutions: Vec<&str> = (run.pairs.iter())
        .map(|p| p.solution_module.as_str())
        .collect();
    solutions.sort();
    let mut heads = [module(&p), module(&f)];
    heads.sort();
    assert_eq!(solutions, heads);
    assert!(
        run.pairs
            .iter()
            .all(|p| p.challenge_module == "PisaSpec.main")
    );
}

async fn accept(
    c: &Cluster,
    against: &[&str],
    allow: &[&str],
    paranoid: bool,
    timeout_secs: u64,
) -> AcceptResponse {
    accept_with(c, against, allow, paranoid, timeout_secs, 0).await
}

/// [`accept`] with `build_timeout_secs` more for the first pair, which
/// builds the run's modules.
async fn accept_with(
    c: &Cluster,
    against: &[&str],
    allow: &[&str],
    paranoid: bool,
    timeout_secs: u64,
    build_timeout_secs: u64,
) -> AcceptResponse {
    let job = Job::SnapshotAccept(SnapshotAcceptJob {
        against: against.iter().map(|s| s.to_string()).collect(),
        allow: allow.iter().map(|s| s.to_string()).collect(),
        paranoid,
        timeout_secs,
        build_timeout_secs,
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
    let output = &r.pairs[0].output;
    assert!(output.contains("more than 1s"), "{output}");
    // What Lake printed before then is kept.
    assert!(output.contains("Resolving dependencies"), "{output}");
}

/// The first pair, which builds the run's modules, has the build's time on
/// top of its own; a slow build without it runs out.
#[tokio::test(flavor = "multi_thread")]
async fn the_first_pair_has_time_to_build_the_runs_modules() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let s = create(
        &c,
        json!({"code": "import Foo

theorem t : 2 = 2 := sorry"}),
    )
    .await;
    let slow = "import Foo

-- SLOW_BUILD
theorem t : 2 = 2 := rfl";
    create(
        &c,
        json!({"code": slow, "replaces": {"snapshot_id": id(&s)}}),
    )
    .await;
    let r = accept_with(&c, &[&id(&s)], &[], false, 2, 0).await;
    let error = r.pairs[0].error.as_ref().map(|e| e.kind);
    assert_eq!(error, Some(PairErrorKind::Timeout), "{r:?}");
    let r = accept_with(&c, &[&id(&s)], &[], false, 2, 10).await;
    assert!(r.accepted, "{r:?}");
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
    let status = r.status().as_u16();
    let (status, verdict) = common::job_answer(status, r.json().await.unwrap());
    assert_eq!(status, 200, "{verdict}");
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

/// A paranoid pair of a spec that permits a non-standard axiom runs the
/// kernels that support one, not `--paranoid`, which fails when con-leche and
/// con-ron decline it. Comparator's exit says the verdict, and the pair says
/// which kernels it ran and which it left out. Without a spec axiom it runs
/// `--paranoid`, and a decline is `unsupported`.
#[tokio::test(flavor = "multi_thread")]
async fn a_spec_axiom_runs_the_kernels_that_support_it() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let judge = async |axiom: &str, marker: &str| {
        let spec = create(
            &c,
            json!({"code": format!("import Foo\n\n{axiom}theorem t : 2 = 2 := sorry")}),
        )
        .await;
        let code = format!("import Foo\n\n-- {marker}\n{axiom}theorem t : 2 = 2 := rfl");
        create(
            &c,
            json!({"code": code, "replaces": {"snapshot_id": id(&spec)}}),
        )
        .await;
        accept(&c, &[&id(&spec)], &[], true, 60).await
    };
    let verdicts = |r: &AcceptResponse| -> Vec<(String, KernelOutcome)> {
        let kernels = r.pairs[0].kernels.iter();
        kernels.map(|k| (k.name.clone(), k.verdict)).collect()
    };
    use KernelOutcome::{Accepted, Rejected, Unsupported};
    let named = |v: &[(&str, KernelOutcome)]| -> Vec<(String, KernelOutcome)> {
        v.iter().map(|(n, o)| (n.to_string(), *o)).collect()
    };

    let r = judge("axiom lit : 1 = 1\n", "KERNEL_DECLINE").await;
    let pair = &r.pairs[0];
    assert!(r.accepted && pair.accepted && pair.error.is_none(), "{r:?}");
    assert_eq!(pair.exit_code, Some(0));
    assert!(!pair.output.contains("--paranoid"), "{}", pair.output);
    assert!(!pair.output.contains("con-leche kernel"), "{}", pair.output);
    assert!(
        pair.output.contains("Running nanoda kernel"),
        "{}",
        pair.output
    );
    assert_eq!(
        verdicts(&r),
        named(&[
            ("Lean paranoid", Accepted),
            ("lean4lean", Accepted),
            ("nanoda", Accepted),
            ("con-leche", Unsupported),
            ("con-ron", Unsupported),
            ("Lean default", Accepted),
        ])
    );
    let reason = pair.kernels[3].reason.as_deref().unwrap();
    assert!(reason.contains("non-standard axioms"), "{reason}");

    // A kernel that rejects fails comparator, and the pair is refused.
    let r = judge("axiom lit : 1 = 1\n", "KERNEL_REJECT").await;
    let pair = &r.pairs[0];
    let error = pair.error.as_ref().unwrap();
    assert!(!r.accepted && !pair.accepted, "{r:?}");
    assert_eq!(
        (error.kind, pair.exit_code),
        (PairErrorKind::Refused, Some(1))
    );
    assert_eq!(error.message, "nanoda exited with 1");
    assert!(verdicts(&r).contains(&("nanoda".to_string(), Rejected)));

    // No spec axiom: `--paranoid` runs con-leche and con-ron, which decline.
    let r = judge("", "KERNEL_DECLINE").await;
    let pair = &r.pairs[0];
    let error = pair.error.as_ref().unwrap();
    assert!(!r.accepted, "{r:?}");
    assert_eq!(error.kind, PairErrorKind::Unsupported);
    assert!(pair.output.contains("--paranoid"), "{}", pair.output);
    assert!(verdicts(&r).contains(&("con-ron".to_string(), Rejected)));
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

/// A spec axiom the head no longer builds on is left out of the axioms
/// comparator permits, and listed: comparator needs each on both sides.
#[tokio::test(flavor = "multi_thread")]
async fn an_axiom_the_head_dropped_is_left_out() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let base = create(&c, json!({"code": "import Foo\n\naxiom lit : 1 = 1"})).await;
    let s = create(&c, json!({"code": on(&base, "theorem t : 2 = 2 := sorry")})).await;
    let filled = "import Foo\n\ntheorem t : 2 = 2 := rfl";
    create(
        &c,
        json!({"code": filled, "replaces": {"snapshot_id": id(&s)}}),
    )
    .await;
    let r = accept(&c, &[&id(&s)], &[], false, 60).await;
    let dropped = LeftOut {
        name: "lit".into(),
        reason: LeftOutReason::AxiomNotInHead,
    };
    assert_eq!(r.pairs[0].left_out, [dropped], "{r:?}");
}

async fn get(c: &Cluster, path: &str) -> Value {
    let r = c.client.get(c.url(path)).send().await.unwrap();
    assert_eq!(r.status(), 200, "{path}");
    r.json().await.unwrap()
}

/// A run whose heads hold only through links is refused at once, naming the
/// settle to send: comparator would judge records that read `sorry`.
#[tokio::test(flavor = "multi_thread")]
async fn an_unsettled_run_is_refused_at_once() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let spec_l = create(
        &c,
        json!({"code": "import Foo\n\ntheorem lemma_l : 1 = 1 := sorry"}),
    )
    .await;
    let spec_t = create(
        &c,
        json!({"code": on(&spec_l, "theorem top : 2 = 2 := sorry")}),
    )
    .await;
    let code = on(&spec_l, "theorem top : 2 = 2 := by have := lemma_l; rfl");
    create(
        &c,
        json!({"code": code, "replaces": {"snapshot_id": id(&spec_t)}}),
    )
    .await;
    let lemma = "import Foo\n\ntheorem lemma_l : 1 = 1 := rfl";
    create(
        &c,
        json!({"code": lemma, "replaces": {"snapshot_id": id(&spec_l)}}),
    )
    .await;
    let job = Job::SnapshotAccept(SnapshotAcceptJob {
        against: vec![id(&spec_l), id(&spec_t)],
        allow: Vec::new(),
        paranoid: false,
        timeout_secs: 60,
        build_timeout_secs: 0,
    });
    let JobResult::SnapshotAccepted(Err(e)) = run(&c, job).await else {
        panic!("an unsettled run was judged")
    };
    assert_eq!(e.kind, pisa_protocol::api::ErrorKind::Conflict, "{e:?}");
    assert!(e.message.contains("POST /snapshots/settle"), "{e:?}");
    let listed = get(&c, &format!("/snapshots/accept?spec={}", id(&spec_t))).await;
    assert_eq!(listed["acceptances"], json!([]), "{listed}");
}

/// An acceptance is stale once its spec is amended, which supersedes it, or
/// a target is refuted that its audit didn't list.
#[tokio::test(flavor = "multi_thread")]
async fn an_acceptance_is_stale_once_its_spec_is_amended_or_a_target_refuted() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    for change in ["amend", "refute"] {
        let code = format!(
            "import Foo\n\ntheorem t : 1 = 2 := sorry\ntheorem e : 1 = 1 := sorry -- {change}"
        );
        let s = create(&c, json!({"code": code})).await;
        let head = code.replace("sorry", "rfl");
        create(
            &c,
            json!({"code": head, "replaces": {"snapshot_id": id(&s)}}),
        )
        .await;
        let accepted = accept(&c, &[&id(&s)], &[], false, 60).await;
        let path = format!("/snapshots/accept/{}", accepted.id);
        assert!(get(&c, &path).await.get("stale").is_none(), "{change}");
        let body = match change {
            "amend" => json!({"code": code.replace("1 = 1", "2 = 2"),
                              "amends": {"snapshot_id": id(&s), "reason": "e was wrong"}}),
            _ => json!({"code": on(&s, "theorem r : ¬ (1 = 2) := by decide"),
                        "refutes": {"snapshot_id": id(&s), "target": "t"}}),
        };
        create(&c, body).await;
        assert_eq!(get(&c, &path).await["stale"], true, "{change}");
    }
}

/// A head deleted while comparator runs leaves the acceptance whole: its
/// review list is read with its audit, before comparator.
#[tokio::test(flavor = "multi_thread")]
async fn a_head_deleted_during_comparator_doesnt_lose_the_acceptance() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let s = create(
        &c,
        json!({"code": "import Foo\n\ntheorem u : 3 = 3 := sorry"}),
    )
    .await;
    let slow = "import Foo\n\n-- SLEEP\ntheorem u : 3 = 3 := rfl";
    let h = create(
        &c,
        json!({"code": slow, "replaces": {"snapshot_id": id(&s)}}),
    )
    .await;
    let delete = async {
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        let r = c.client.delete(c.url(&format!("/snapshots/{}", id(&h))));
        assert_eq!(r.send().await.unwrap().status(), 200);
    };
    let spec = id(&s);
    let against = [spec.as_str()];
    let (r, ()) = tokio::join!(accept(&c, &against, &[], false, 60), delete);
    assert_eq!(r.audit.review[0].head, id(&h), "{r:?}");
    get(&c, &format!("/snapshots/accept/{}", r.id)).await;
}

/// A stored acceptance names the modules it judged and keeps the review
/// list; a spec's acceptances are listed newest first, and one whose heads
/// are no longer the run's says it is stale.
#[tokio::test(flavor = "multi_thread")]
async fn an_acceptance_keeps_what_it_judged_and_is_found_by_spec() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let spec = "import Foo\n\ndef cost : Nat := sorry\ntheorem t : cost = cost := sorry";
    let s = create(&c, json!({"code": spec, "holes": ["cost"]})).await;
    let filled =
        |n: u32| format!("import Foo\n\ndef cost : Nat := {n}\ntheorem t : cost = cost := rfl");
    let h = create(
        &c,
        json!({"code": filled(3), "replaces": {"snapshot_id": id(&s)}}),
    )
    .await;
    let first = accept(&c, &[&id(&s)], &[], false, 60).await;
    let pair = &first.pairs[0];
    assert_eq!(
        (pair.spec_module.as_str(), pair.head_module.as_str()),
        (module(&s).as_str(), module(&h).as_str())
    );
    assert_eq!(first.audit.review[0].head, id(&h), "{first:?}");
    let second = accept(&c, &[&id(&s)], &[], false, 60).await;
    let listed = get(&c, &format!("/snapshots/accept?spec={}", id(&s))).await;
    let listed = listed["acceptances"].as_array().unwrap();
    let ids: Vec<&Value> = listed.iter().map(|a| &a["id"]).collect();
    assert_eq!(ids, [&json!(second.id), &json!(first.id)]);
    assert!(
        listed.iter().all(|a| a.get("stale").is_none()),
        "{listed:?}"
    );
    // The run moves on.
    create(
        &c,
        json!({"code": filled(4), "replaces": {"snapshot_id": id(&h)}}),
    )
    .await;
    let read = get(&c, &format!("/snapshots/accept/{}", first.id)).await;
    assert_eq!(read["stale"], true, "{read}");
    // A spec with none lists none.
    let other = create(
        &c,
        json!({"code": "import Foo\n\ntheorem z : 1 = 1 := sorry"}),
    )
    .await;
    let none = get(&c, &format!("/snapshots/accept?spec={}", id(&other))).await;
    assert_eq!(none["acceptances"], json!([]));
}
