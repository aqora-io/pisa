//! Whether Beam works inside the worker's sandbox at all, and how: each
//! check records its outcome (or error) instead of stopping the run.

use std::time::{Duration, Instant};

use anyhow::Result;
use futures::StreamExt;
use pisa_beam::pos::anchored;
use pisa_beam::{BeamError, Mode, Packages, Workspace};
use pisa_worker::policy::AXIOM_REPORT;
use serde_json::{Value, json};

use crate::env::{Env, LIB};
use crate::lean::{self, T_BODY, T_STEP};
use crate::stats::ms;

/// The smallest bench modules: their headers are the cheapest to import.
const SMALL: &[&str] = &[
    "Physlib.SpaceAndTime.Space.Basic",
    "Physlib.QuantumMechanics.Operators.Momentum",
    "Physlib.Relativity.Tensors.RealTensor.Vector.Basic",
];

fn outcome<T: serde::Serialize>(r: Result<T>) -> Value {
    match r {
        Ok(v) => json!({"ok": true, "result": v}),
        Err(e) => json!({"ok": false, "error": format!("{e:#}")}),
    }
}

pub async fn run(env: &Env) -> Result<Value> {
    let mut out = serde_json::Map::new();
    let version = {
        let slot = env.slot();
        match env.beam(slot).await {
            Ok(beam) => beam.version(T_STEP).await.unwrap_or(Value::Null),
            Err(e) => json!({"error": format!("{e:#}")}),
        }
    };
    out.insert("beam_version".into(), version);
    out.insert("bare".into(), outcome(bare(env).await));
    let header = lean::load_module(&env.project, SMALL[0])?.header;
    out.insert(
        "repl_header".into(),
        outcome(repl_header(env, &header).await),
    );
    for packages in [Packages::Symlink, Packages::Absolute] {
        let name = format!("{packages:?}").to_lowercase();
        out.insert(
            format!("project_{name}"),
            outcome(project(env, &header, packages).await),
        );
    }
    out.insert("snapshot_import".into(), outcome(snapshot(env).await));
    out.insert("report_parity".into(), outcome(report_parity(env).await));
    out.insert("documents".into(), outcome(documents(env, &header).await));
    out.insert(
        "busy_and_cancel".into(),
        outcome(busy_and_cancel(env, &header).await),
    );
    Ok(Value::Object(out))
}

/// Check 1: A workspace with no dependencies in the slot directory.
async fn bare(env: &Env) -> Result<Value> {
    let slot = env.slot();
    let ws = Workspace::bare(&env.slot_dir(slot).join("bare"), &env.toolchain, "Docs")?;
    ws.write(
        "Docs/A.lean",
        "theorem t (a b : Nat) : a + b = b + a := by\n  sorry\n",
    )?;
    let doc = ws.doc("Docs/A.lean");
    let beam = env.beam(slot).await?;
    let start = Instant::now();
    let sync = beam.sync(doc, true, T_BODY).await?;
    let todo = beam.todo(doc, sync.version, &["sorry"], T_STEP).await?;
    let at = todo.items[0].run_at_position.unwrap_or_default();
    let goals = beam
        .goals(doc, sync.version, at, Mode::Before, T_STEP)
        .await?;
    let probe = beam.run_at(doc, sync.version, at, "omega", T_STEP).await?;
    Ok(
        json!({"ms": ms(start.elapsed()), "goal": goals.first().map(pisa_beam::goal::render),
              "omega_closes": probe.success, "usage": env.usage(slot)}),
    )
}

/// The REPL's import of the same header, for comparison.
async fn repl_header(env: &Env, header: &str) -> Result<Value> {
    let slot = env.slot();
    let mut repl = env.repl(slot)?;
    let start = Instant::now();
    lean::repl_header(&mut repl, header).await?;
    Ok(json!({"ms": ms(start.elapsed()), "usage": env.usage(slot)}))
}

/// Checks 2, 3 and 5: A workspace over the project: does a document with the worker's
/// header (Physlib and `Pisa`) open without Lake trying to build, and save?
async fn project(env: &Env, header: &str, packages: Packages) -> Result<Value> {
    let slot = env.slot();
    let ws = Workspace::over(&env.slot_dir(slot).join("ws"), &env.project, LIB, packages)?;
    let (text, _) = lean::document(header, "theorem t : (2 : ℕ) + 2 = 4 := by\n  sorry\n");
    let path = format!("{LIB}/P.lean");
    ws.write(&path, &text)?;
    let beam = env.beam(slot).await?;
    let start = Instant::now();
    let sync = beam.sync(ws.doc(&path), true, T_BODY).await?;
    let sync_ms = ms(start.elapsed());
    let errors: Vec<&str> = sync
        .diagnostics
        .items
        .iter()
        .filter(|d| d.severity == "error")
        .map(|d| d.message.as_str())
        .collect();
    let save = beam
        .save(ws.doc(&path), T_BODY)
        .await
        .map_err(|e| e.to_string());
    Ok(
        json!({"sync_ms": sync_ms, "errors": errors, "save": save.map(|s| s.module),
              "stderr": beam.stderr_tail(), "usage": env.usage(slot)}),
    )
}

/// Check 3: A snapshot module, compiled as the worker compiles one and placed in the
/// snapshot cache: does a Beam document import it through LEAN_PATH?
async fn snapshot(env: &Env) -> Result<Value> {
    let slot = env.slot();
    let dir = env.slot_dir(slot);
    let src = dir.join("snap-src");
    let file = src.join("PisaSnap/sProbe.lean");
    std::fs::create_dir_all(file.parent().unwrap())?;
    std::fs::write(&file, "def probeConst : Nat := 42\n")?;
    let olean = dir.join("snap-out/PisaSnap/sProbe.olean");
    std::fs::create_dir_all(olean.parent().unwrap())?;
    let compiled = env
        .launcher
        .compile(slot, &src, &file, &olean, Duration::from_secs(300))
        .await?;
    anyhow::ensure!(olean.is_file(), "compile failed: {}", compiled.stderr);
    let cache = env.snapshot_root().join("PisaSnap");
    std::fs::create_dir_all(&cache)?;
    for ext in ["olean", "ilean"] {
        let from = olean.with_extension(ext);
        if from.is_file() {
            std::fs::copy(&from, cache.join(format!("sProbe.{ext}")))?;
        }
    }
    let ws = env.workspace(slot, "ws")?;
    let path = format!("{LIB}/S.lean");
    ws.write(
        &path,
        "import PisaSnap.sProbe\n\nexample : probeConst = 42 := rfl\n",
    )?;
    let beam = env.beam(slot).await?;
    let sync = beam.sync(ws.doc(&path), true, T_BODY).await?;
    let errors: Vec<&str> = sync
        .diagnostics
        .items
        .iter()
        .filter(|d| d.severity == "error")
        .map(|d| d.message.as_str())
        .collect();
    Ok(json!({"imports": errors.is_empty(), "errors": errors}))
}

/// Check 4: The axiom report from a Beam command probe against the REPL's, read by
/// the same parser.
async fn report_parity(env: &Env) -> Result<Value> {
    let mut out = Vec::new();
    for module in SMALL {
        let src = lean::load_module(&env.project, module)?;
        let slot = env.slot();
        let mut repl = env.repl(slot)?;
        let header = lean::repl_header(&mut repl, &src.header).await?;
        let r = lean::repl_body(&mut repl, header, &src.body, None).await?;
        let response = lean::repl_report_response(&mut repl, r.env).await?;
        let repl_report = pisa_worker::policy::parse_report(&response);
        let repl_texts: Vec<&str> = response.messages.iter().map(|m| m.data.as_str()).collect();
        let repl_why = repl_report
            .is_none()
            .then(|| lean::why_no_report(&repl_texts));
        let body_errors = lean::errors(&r);
        drop(repl);

        let slot = env.slot();
        let ws = env.workspace(slot, "ws")?;
        let (text, anchor) = anchored(&lean::document(&src.header, &src.body).0);
        let path = format!("{LIB}/R.lean");
        ws.write(&path, &text)?;
        let beam = env.beam(slot).await?;
        let sync = beam.sync(ws.doc(&path), false, T_BODY).await?;
        let probe = beam
            .run_at(ws.doc(&path), sync.version, anchor, AXIOM_REPORT, T_STEP)
            .await?;
        let beam_report = lean::beam_report(&probe.messages);
        let beam_texts: Vec<&str> = probe.messages.iter().map(|m| m.text.as_str()).collect();
        let beam_why = beam_report
            .is_none()
            .then(|| lean::why_no_report(&beam_texts));
        let doc_errors: Vec<&str> = sync
            .diagnostics
            .items
            .iter()
            .filter(|d| d.severity == "error")
            .map(|d| d.message.as_str())
            .take(3)
            .collect();
        let (repl_n, beam_n) = (
            repl_report.as_ref().map(Vec::len),
            beam_report.as_ref().map(Vec::len),
        );
        let first_difference = match (&repl_report, &beam_report) {
            (Some(a), Some(b)) => a
                .iter()
                .zip(b)
                .find(|(x, y)| x != y)
                .map(|(x, y)| format!("{x:?}\n  vs\n{y:?}")),
            _ => None,
        };
        out.push(json!({"module": module, "same": repl_report == beam_report,
                        "repl_declarations": repl_n, "beam_declarations": beam_n,
                        "first_difference": first_difference, "repl_why": repl_why,
                        "beam_why": beam_why, "repl_body_errors": body_errors,
                        "beam_document_errors": doc_errors}));
    }
    Ok(json!(out))
}

/// Check 6: Memory and process count with one to four documents open in one process.
async fn documents(env: &Env, header: &str) -> Result<Value> {
    let slot = env.slot();
    let ws = env.workspace(slot, "ws")?;
    let beam = env.beam(slot).await?;
    let mut out = Vec::new();
    for n in 1..=4 {
        let path = format!("{LIB}/D{n}.lean");
        let (text, _) = lean::document(header, &format!("theorem t{n} : True := trivial\n"));
        ws.write(&path, &text)?;
        let start = Instant::now();
        beam.sync(ws.doc(&path), false, T_BODY).await?;
        out.push(
            json!({"open": n, "sync_ms": ms(start.elapsed()), "pids": env.pids(slot),
                        "usage": env.usage(slot)}),
        );
    }
    Ok(json!(out))
}

/// Check 7: Forty probes at once, then a probe cancelled by its timeout and the
/// time the next one takes.
async fn busy_and_cancel(env: &Env, header: &str) -> Result<Value> {
    let slot = env.slot();
    let ws = env.workspace(slot, "ws")?;
    let path = format!("{LIB}/B.lean");
    let (text, _) = lean::document(
        header,
        "theorem t (a b : ℕ) : a + b = b + a := by\n  sorry\n",
    );
    ws.write(&path, &text)?;
    let beam = env.beam(slot).await?;
    let doc = ws.doc(&path);
    let sync = beam.sync(doc, false, T_BODY).await?;
    let todo = beam.todo(doc, sync.version, &["sorry"], T_STEP).await?;
    let at = todo.items[0].run_at_position.unwrap_or_default();
    let start = Instant::now();
    let results: Vec<_> = futures::stream::iter(0..40)
        .map(|_| beam.run_at(doc, sync.version, at, "omega", T_STEP))
        .buffer_unordered(40)
        .collect()
        .await;
    let burst_ms = ms(start.elapsed());
    let busy = results
        .iter()
        .filter(|r| matches!(r, Err(BeamError::Busy(_))))
        .count();
    let errors: Vec<String> = results
        .iter()
        .filter_map(|r| r.as_ref().err().map(|e| e.to_string()))
        .take(3)
        .collect();
    let slow = beam
        .run_at(doc, sync.version, at, "sleep 5000", Duration::from_secs(1))
        .await;
    let cancelled = matches!(slow, Err(BeamError::Timeout));
    let start = Instant::now();
    let next = beam.run_at(doc, sync.version, at, "omega", T_STEP).await;
    Ok(
        json!({"burst_ms": burst_ms, "busy": busy, "errors": errors, "timed_out": cancelled,
              "next_ms": ms(start.elapsed()), "next_ok": next.is_ok(), "alive": beam.is_alive()}),
    )
}
