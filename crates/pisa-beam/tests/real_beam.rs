//! The client against real Lean Beam, on a small dependency-free workspace.
//! Each test needs elan's Lean 4.36.0-rc2 (whose toolchain ships
//! `lean-beam-mcp`), so they are ignored by default:
//!
//! ```text
//! ELAN_TOOLCHAIN=leanprover/lean4:v4.36.0-rc2 cargo test -p pisa-beam --test real_beam -- --ignored
//! ```
//!
//! The report test also needs the `Pisa` library built
//! (`cd docker/pisa-lean && lake build`).

use std::path::{Path, PathBuf};
use std::time::Duration;

use pisa_beam::{Beam, LspPos, Mode, Workspace};
use tokio::process::Command;

const T: Duration = Duration::from_secs(120);
const TOOLCHAIN: &str = "leanprover/lean4:v4.36.0-rc2";

/// The toolchain's `lean-beam-mcp`, next to the `lean` elan resolves.
fn beam_bin() -> PathBuf {
    let out = std::process::Command::new("lean")
        .arg("--print-prefix")
        .output()
        .expect("running lean (is elan's Lean 4.36.0-rc2 on PATH?)");
    let prefix = String::from_utf8(out.stdout).unwrap();
    Path::new(prefix.trim()).join("bin/lean-beam-mcp")
}

async fn beam(ws: &Workspace, lean_path: Option<&Path>) -> Beam {
    let mut cmd = Command::new(beam_bin());
    cmd.current_dir(ws.root());
    if let Some(path) = lean_path {
        cmd.env("LEAN_PATH", path);
    }
    Beam::spawn(cmd, T).await.expect("starting lean-beam-mcp")
}

const DOC: &str = "\
def double (n : Nat) : Nat := 2 * n

theorem double_add (a b : Nat) : double (a + b) = double a + double b := by
  sorry
";

#[tokio::test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
async fn tactics_chain_from_a_sorry_and_the_file_saves() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = Workspace::bare(tmp.path(), TOOLCHAIN, "Docs").unwrap();
    ws.write("Docs/A.lean", DOC).unwrap();
    let doc = ws.doc("Docs/A.lean");
    let beam = beam(&ws, None).await;
    let sync = beam.sync(doc, true, T).await.unwrap();
    assert!(
        sync.diagnostics
            .items
            .iter()
            .any(|d| d.message.contains("sorry"))
    );
    let todo = beam.todo(doc, sync.version, &["sorry"], T).await.unwrap();
    assert_eq!(todo.items.len(), 1);
    let at = todo.items[0].run_at_position.unwrap();
    assert_eq!(
        at,
        LspPos {
            line: 3,
            character: 2
        }
    );

    let goals = beam
        .goals(doc, sync.version, at, Mode::Before, T)
        .await
        .unwrap();
    assert_eq!(
        pisa_beam::goal::render(&goals[0]),
        "a b : Nat\n⊢ double (a + b) = double a + double b"
    );
    // Independent probes from the same position.
    let omega = beam
        .run_at(doc, sync.version, at, "omega", T)
        .await
        .unwrap();
    assert!(!omega.success);
    let simp = beam
        .run_at(doc, sync.version, at, "simp [double]", T)
        .await
        .unwrap();
    assert!(simp.success);

    // A chain: unfold, then branch twice from the same state.
    let step = beam
        .run_at_handle(doc, sync.version, at, "unfold double", T)
        .await
        .unwrap();
    let target = &step.proof_state.unwrap().goals[0].target;
    assert_eq!(target, "2 * (a + b) = 2 * a + 2 * b");
    let handle = step.next_handle.unwrap();
    let done = beam.run_with(doc, &handle, "omega", T).await.unwrap();
    assert!(done.success && done.proof_state.unwrap().goals.is_empty());
    let other = beam.run_with_linear(doc, &handle, "simp", T).await.unwrap();
    assert!(!other.success);
    // run_with_linear consumed it.
    assert!(beam.release(doc, &handle, T).await.is_err());

    let saved = beam.save(doc, T).await.unwrap();
    assert_eq!(saved.module, "Docs.A");
    assert!(Path::new(&saved.olean).is_file());
    beam.close(doc, T).await.unwrap();
}

#[tokio::test]
#[ignore = "needs elan Lean 4.36.0-rc2 and the Pisa library built"]
async fn the_axiom_report_runs_as_a_command_probe() {
    let pisa =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docker/pisa-lean/.lake/build/lib/lean");
    assert!(
        pisa.join("Pisa.olean").is_file(),
        "build docker/pisa-lean first"
    );
    let tmp = tempfile::tempdir().unwrap();
    let ws = Workspace::bare(tmp.path(), TOOLCHAIN, "Docs").unwrap();
    let (text, end) = pisa_beam::pos::anchored(&format!("import Pisa\n{DOC}"));
    ws.write("Docs/R.lean", &text).unwrap();
    let doc = ws.doc("Docs/R.lean");
    // Lean's own library plus Pisa's, as the worker's LEAN_PATH has them.
    let beam = beam(&ws, Some(&std::fs::canonicalize(&pisa).unwrap())).await;
    let sync = beam.sync(doc, false, T).await.unwrap();
    assert!(
        sync.diagnostics.items.iter().all(|d| d.severity != "error"),
        "{:?}",
        sync.diagnostics.items
    );
    let report = beam
        .run_at(doc, sync.version, end, "run_cmd _root_.Pisa.reportNew", T)
        .await
        .unwrap();
    assert!(report.success, "{:?}", report.messages);
    let decls = report
        .messages
        .iter()
        .find(|m| m.text.starts_with("PISA_DECLS "))
        .unwrap_or_else(|| panic!("no report in {:?}", report.messages));
    assert!(decls.text.contains("double_add"), "{}", decls.text);
}
