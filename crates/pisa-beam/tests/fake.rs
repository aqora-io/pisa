//! The client against `fake-beam-mcp`, which checks arguments against the real
//! tool schemas and fakes the answers.

use std::path::Path;
use std::time::{Duration, Instant};

use pisa_beam::{Beam, BeamError, Doc, LspPos, Mode};
use tokio::process::Command;

const T: Duration = Duration::from_secs(10);

async fn beam() -> Beam {
    Beam::spawn(Command::new(env!("CARGO_BIN_EXE_fake-beam-mcp")), T)
        .await
        .expect("spawning the fake")
}

fn doc() -> Doc<'static> {
    Doc {
        root: Path::new("/w"),
        path: "Docs/A.lean",
    }
}

const AT: LspPos = LspPos {
    line: 1,
    character: 2,
};

#[tokio::test]
async fn a_session_runs_every_tool_with_arguments_beam_accepts() {
    let beam = beam().await;
    assert!(beam.tools().iter().any(|t| t == "lean_run_at_handle"));
    let version = beam.update(doc(), T).await.unwrap().version;
    let sync = beam.sync(doc(), true, T).await.unwrap();
    assert_eq!(sync.version, version);
    assert_eq!(sync.diagnostics.items[0].severity, "warning");
    let todo = beam.todo(doc(), version, &["sorry"], T).await.unwrap();
    let at = todo.items[0].run_at_position.unwrap();
    assert_eq!(at, AT);
    let goals = beam
        .goals(doc(), version, at, Mode::Before, T)
        .await
        .unwrap();
    assert_eq!(pisa_beam::goal::render(&goals[0]), "⊢ True");

    let probe = beam.run_at(doc(), version, at, "trivial", T).await.unwrap();
    assert!(probe.success && probe.next_handle.is_none());
    let failed = beam.run_at(doc(), version, at, "omega", T).await.unwrap();
    assert!(!failed.success);
    assert_eq!(failed.messages[0].severity, "error");

    let kept = beam
        .run_at_handle(doc(), version, at, "rfl", T)
        .await
        .unwrap();
    let handle = kept.next_handle.expect("a handle");
    let next = beam.run_with(doc(), &handle, "rfl", T).await.unwrap();
    assert!(next.success && next.next_handle.is_some());
    beam.run_with_linear(doc(), &handle, "rfl", T)
        .await
        .unwrap();
    beam.release(doc(), &handle, T).await.unwrap();
    assert_eq!(beam.save(doc(), T).await.unwrap().module, "Docs.A");
    beam.close(doc(), T).await.unwrap();
    assert_eq!(beam.version(T).await.unwrap()["version"], "fake");
    beam.stats(T).await.unwrap();
}

#[tokio::test]
async fn replies_find_their_requests_out_of_order() {
    let beam = beam().await;
    let start = Instant::now();
    let (slow, fast) = tokio::join!(beam.run_at(doc(), 1, AT, "SLEEP 400 rfl", T), async {
        let r = beam.run_at(doc(), 1, AT, "rfl", T).await;
        (r, start.elapsed())
    });
    assert!(slow.unwrap().success);
    let (fast, took) = fast;
    assert!(fast.unwrap().success);
    assert!(took < Duration::from_millis(300), "{took:?}");
}

#[tokio::test]
async fn a_timed_out_probe_is_cancelled_and_the_process_kept() {
    let beam = beam().await;
    let r = beam
        .run_at(doc(), 1, AT, "SLEEP 2000 rfl", Duration::from_millis(100))
        .await;
    assert!(matches!(r, Err(BeamError::Timeout)), "{r:?}");
    assert!(beam.is_alive());
    assert!(beam.run_at(doc(), 1, AT, "rfl", T).await.unwrap().success);
}

#[tokio::test]
async fn busy_stale_and_failing_tools_are_told_apart() {
    let beam = beam().await;
    let busy = beam.run_at(doc(), 1, AT, "BUSY", T).await;
    assert!(matches!(busy, Err(BeamError::Busy(_))), "{busy:?}");
    let stale = beam.run_at(doc(), 1, AT, "STALE", T).await;
    assert!(matches!(stale, Err(BeamError::Stale(_))), "{stale:?}");
    match beam.run_at(doc(), 1, AT, "FAIL", T).await {
        Err(BeamError::Tool { tool, code, .. }) => {
            assert_eq!(
                (tool.as_str(), code.as_str()),
                ("lean_run_at", "invalidParams")
            );
        }
        other => panic!("{other:?}"),
    }
    // An undeclared field is refused, as Beam refuses it.
    match beam
        .call(
            "lean_update",
            serde_json::json!({"workspace": {"root": "/w"}, "path": "A.lean", "extra": 1}),
            T,
        )
        .await
    {
        Err(BeamError::Tool { code, .. }) => assert_eq!(code, "invalidInput"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_crash_says_what_it_printed_and_later_calls_fail_fast() {
    let beam = beam().await;
    match beam.run_at(doc(), 1, AT, "CRASH", T).await {
        Err(BeamError::Exited { stderr }) => {
            assert!(stderr.contains("crashing as asked"), "{stderr}")
        }
        other => panic!("{other:?}"),
    }
    assert!(!beam.is_alive());
    let r = beam.run_at(doc(), 1, AT, "rfl", T).await;
    assert!(matches!(r, Err(BeamError::Dead)), "{r:?}");
}

#[tokio::test]
async fn documents_on_disk_are_read_where_positions_land() {
    let tmp = tempfile::tempdir().unwrap();
    let ws =
        pisa_beam::Workspace::bare(tmp.path(), "leanprover/lean4:v4.36.0-rc2", "Docs").unwrap();
    ws.write("Docs/D.lean", "theorem t : 𝓝 = 𝓝 := by\n  sorry\n")
        .unwrap();
    let doc = ws.doc("Docs/D.lean");
    let beam = beam().await;
    let sync = beam.sync(doc, true, T).await.unwrap();
    assert_eq!(sync.version, 1);
    let warning = &sync.diagnostics.items[0];
    assert_eq!(
        warning.range.unwrap().start,
        LspPos {
            line: 1,
            character: 2
        }
    );
    let todo = beam.todo(doc, sync.version, &["sorry"], T).await.unwrap();
    assert_eq!(todo.items.len(), 1);
    assert_eq!(
        todo.items[0].run_at_position,
        Some(LspPos {
            line: 1,
            character: 2
        })
    );
    // The goal is the rest of the line. The first `𝓝` takes units 12 and
    // 13, so the second starts at 17.
    let goals = beam
        .goals(
            doc,
            sync.version,
            LspPos {
                line: 0,
                character: 17,
            },
            Mode::Before,
            T,
        )
        .await
        .unwrap();
    assert_eq!(goals[0].target, "𝓝 := by");
    let failed = beam
        .run_at(
            doc,
            sync.version,
            LspPos {
                line: 1,
                character: 2,
            },
            "omega",
            T,
        )
        .await
        .unwrap();
    assert_eq!(
        failed.messages[0].range.unwrap().start,
        LspPos {
            line: 1,
            character: 2
        }
    );

    // An edit is a new version; the old one is stale.
    ws.write("Docs/D.lean", "theorem t : 1 = 1 := by\n  rfl\n")
        .unwrap();
    assert_eq!(beam.sync(doc, true, T).await.unwrap().version, 2);
    let stale = beam.goals(doc, 1, LspPos::default(), Mode::Before, T).await;
    assert!(matches!(stale, Err(BeamError::Stale(_))), "{stale:?}");
    assert!(
        beam.todo(doc, 2, &["sorry"], T)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    beam.close(doc, T).await.unwrap();
}
