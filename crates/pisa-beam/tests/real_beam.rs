//! The client against real Lean Beam, on a small dependency-free workspace.
//! Each test needs elan on `PATH` and runs on the `Pisa` library's toolchain
//! (v4.36 and later ship `lean-beam-mcp`), as the image does, so they are
//! ignored by default:
//!
//! ```text
//! cargo test -p pisa-beam --test real_beam -- --ignored
//! ```
//!
//! The report test also needs the `Pisa` library built
//! (`cd docker/pisa-lean && lake build`).

use std::path::{Path, PathBuf};
use std::time::Duration;

use pisa_beam::{Beam, LspPos, Mode, Workspace};
use tokio::process::Command;

const T: Duration = Duration::from_secs(120);
/// The `Pisa` library's toolchain, which the worker image installs.
fn toolchain() -> String {
    let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docker/pisa-lean/lean-toolchain");
    std::fs::read_to_string(file).unwrap().trim().to_string()
}

/// That toolchain's `lean-beam-mcp`, whatever elan's default.
fn beam_bin() -> PathBuf {
    let out = std::process::Command::new("lean")
        .arg("--print-prefix")
        .env("ELAN_TOOLCHAIN", toolchain())
        .output()
        .expect("running lean (is elan on PATH?)");
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
    let ws = Workspace::bare(tmp.path(), &toolchain(), "Docs").unwrap();
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
    let ws = Workspace::bare(tmp.path(), &toolchain(), "Docs").unwrap();
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

/// Beam is beta: if a new toolchain's tools change, the saved schemas (which
/// the fake and the argument test use) must change with them.
#[tokio::test]
#[ignore = "needs elan and the Pisa library's toolchain"]
async fn the_live_tools_list_is_the_saved_one() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = Workspace::bare(tmp.path(), &toolchain(), "Docs").unwrap();
    let beam = beam(&ws, None).await;
    let saved: serde_json::Value =
        serde_json::from_str(include_str!("../testdata/tools-list.json")).unwrap();
    let schemas = |list: &serde_json::Value| -> Vec<(String, serde_json::Value)> {
        let mut tools: Vec<_> = list["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| {
                (
                    t["name"].as_str().unwrap().to_string(),
                    t["inputSchema"].clone(),
                )
            })
            .collect();
        tools.sort_by(|a, b| a.0.cmp(&b.0));
        tools
    };
    assert_eq!(schemas(beam.tools_list()), schemas(&saved));
}

/// Positions in a body below a header of other lines, as the worker writes
/// documents, with a name outside the Basic Multilingual Plane.
#[tokio::test]
#[ignore = "needs elan and the Pisa library's toolchain"]
async fn sorries_and_goals_below_a_header() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = Workspace::bare(tmp.path(), &toolchain(), "Docs").unwrap();
    let header = "set_option autoImplicit false\nset_option linter.unusedVariables false";
    let body = "def 𝓝 : Nat := 2\n\ntheorem t : 𝓝 + 0 = 𝓝 := by\n  sorry\n";
    let text = format!("{header}\n{body}");
    ws.write("Docs/H.lean", &text).unwrap();
    let doc = ws.doc("Docs/H.lean");
    let beam = beam(&ws, None).await;
    let sync = beam.sync(doc, true, T).await.unwrap();
    let map = pisa_beam::pos::BodyMap::new(&text, 2);
    let todo = beam.todo(doc, sync.version, &["sorry"], T).await.unwrap();
    let at = todo.items[0].run_at_position.unwrap();
    assert_eq!(at, map.to_lsp(4, 2));
    assert_eq!(map.to_body(at), Some((4, 2)));
    let goals = beam
        .goals(doc, sync.version, at, Mode::Before, T)
        .await
        .unwrap();
    assert_eq!(pisa_beam::goal::render(&goals[0]), "⊢ 𝓝 + 0 = 𝓝");
    let probe = beam.run_at(doc, sync.version, at, "rfl", T).await.unwrap();
    assert!(probe.success, "{:?}", probe.messages);
}
