//! The measured phases. Each runs both backends on the same workload, in
//! fresh slots, and returns its raw samples and summaries as JSON.

use std::collections::HashMap;
use std::time::Instant;

use anyhow::{Result, bail};
use futures::StreamExt;
use pisa_beam::pos::{BodyMap, anchored};
use pisa_beam::{Beam, BeamError, Doc, LspPos, Mode, Workspace};
use pisa_protocol::repl::{Pos, at_positions};
use pisa_worker::policy::AXIOM_REPORT;
use serde::Serialize;
use serde_json::{Value, json};

use crate::env::{Env, LIB};
use crate::lean::{self, T_BODY, T_STEP};
use crate::stats::{ms, summarize, timed};
use crate::workload::{Block, ModuleWork, Workload};

/// The API's limit on positions per check.
const MAX_AT: usize = 64;
/// Automation tried at every sorry, before the block's own steps.
const AUTOMATION: &[&str] = &["simp", "omega", "norm_num", "aesop"];

fn doc_path(name: &str) -> String {
    format!("{LIB}/{name}.lean")
}

/// A Beam process with one document written and synced.
struct Opened {
    beam: Beam,
    ws: Workspace,
    path: String,
    version: u64,
    map_offset: u32,
    text: String,
    sync_ms: f64,
}

impl Opened {
    fn doc(&self) -> Doc<'_> {
        self.ws.doc(&self.path)
    }

    fn map(&self) -> BodyMap<'_> {
        BodyMap::new(&self.text, self.map_offset)
    }

    /// Rewrites the document and syncs it; returns the sync's time.
    async fn rewrite(&mut self, text: String) -> Result<f64> {
        self.ws.write(&self.path, &text)?;
        let (sync, took) = timed(self.beam.sync(self.ws.doc(&self.path), false, T_BODY)).await;
        self.version = sync?.version;
        self.text = text;
        Ok(ms(took))
    }
}

async fn open(env: &Env, slot: usize, header: &str, body: &str, anchor: bool) -> Result<Opened> {
    let beam = env.beam(slot).await?;
    let ws = env.workspace(slot, "ws")?;
    let (text, offset) = lean::document(header, body);
    let text = if anchor { anchored(&text).0 } else { text };
    let path = doc_path("Doc");
    ws.write(&path, &text)?;
    let (sync, took) = timed(beam.sync(ws.doc(&path), false, T_BODY)).await;
    let sync = sync?;
    if let Some(e) = sync
        .diagnostics
        .items
        .iter()
        .find(|d| d.severity == "error")
    {
        tracing::warn!(error = %e.message, "the document has errors");
    }
    Ok(Opened {
        beam,
        ws,
        path,
        version: sync.version,
        map_offset: offset,
        text,
        sync_ms: ms(took),
    })
}

/// Goals before each step of the usable blocks, at most [`MAX_AT`] per module.
pub async fn goals(env: &Env, workload: &Workload) -> Result<Value> {
    let mut modules = Vec::new();
    for work in &workload.modules {
        let src = &work.source;
        let positions: Vec<Pos> = work
            .usable()
            .flat_map(|b| {
                b.steps.iter().map(move |s| Pos {
                    line: s.line as u64,
                    column: b.base as u64,
                })
            })
            .take(MAX_AT)
            .collect();
        if positions.is_empty() {
            continue;
        }
        // REPL: elaborate with the tactic tree, read every position from it.
        let slot = env.slot();
        let mut repl = env.repl(slot)?;
        let (header, header_t) = timed(lean::repl_header(&mut repl, &src.header)).await;
        let header = header?;
        let (r, tree_t) = timed(lean::repl_body(
            &mut repl,
            header,
            &src.body,
            Some("original"),
        ))
        .await;
        let r = r?;
        let tree = r.infotree.clone().unwrap_or_else(|| json!([]));
        let (at, read_t) = timed(async { at_positions(&tree, &src.body, &positions, false) }).await;
        let at = at?;
        let repl_usage = env.usage(slot);
        drop(repl);

        // Beam: sync once, then ask each position.
        let slot = env.slot();
        let mut opened = open(env, slot, &src.header, &src.body, false).await?;
        let mut warm = Vec::new();
        let mut same = 0;
        let mut differ: Vec<Value> = Vec::new();
        let mut kinds: HashMap<&str, usize> = HashMap::new();
        for (pos, at) in positions.iter().zip(&at) {
            let lsp = opened.map().to_lsp(pos.line as u32, pos.column as u32);
            let (goals, took) =
                timed(
                    opened
                        .beam
                        .goals(opened.doc(), opened.version, lsp, Mode::Before, T_STEP),
                )
                .await;
            warm.push(ms(took));
            let beam: Vec<String> = goals?.iter().map(pisa_beam::goal::render).collect();
            let repl = at
                .tactic
                .as_ref()
                .map(|t| t.goals_before.clone())
                .unwrap_or_default();
            // The REPL's tree has no tactic at some positions; tell that apart
            // from goals that differ.
            // Lean breaks long goals differently in the two (the REPL indents a
            // continuation by four, the server by two), so layout is told apart.
            let flat = |goals: &[String]| -> Vec<String> {
                goals
                    .iter()
                    .map(|g| g.split_whitespace().collect::<Vec<_>>().join(" "))
                    .collect()
            };
            let kind = match (repl.is_empty(), beam.is_empty()) {
                _ if beam == repl => "same",
                _ if flat(&beam) == flat(&repl) => "layout",
                (true, false) => "repl_none",
                (false, true) => "beam_none",
                _ => "differ",
            };
            *kinds.entry(kind).or_insert(0usize) += 1;
            if kind == "same" {
                same += 1;
            } else if differ.iter().filter(|d: &&Value| d["kind"] == kind).count() < 3 {
                differ.push(json!({"kind": kind, "pos": pos, "repl": repl, "beam": beam}));
            }
        }
        // An edit in the middle (trailing spaces on a step's line), then a
        // position after it.
        let edit = work
            .usable()
            .nth(work.usable().count() / 2)
            .map(|b| &b.steps[0]);
        let mut edited = Value::Null;
        if let Some(step) = edit {
            let line = (opened.map_offset + step.line - 1) as usize;
            let mut lines: Vec<String> = opened.text.split('\n').map(String::from).collect();
            lines[line].push_str("  ");
            let resync = opened.rewrite(lines.join("\n")).await?;
            let last = *positions.last().unwrap();
            let lsp = opened.map().to_lsp(last.line as u32, last.column as u32);
            let (g, took) =
                timed(
                    opened
                        .beam
                        .goals(opened.doc(), opened.version, lsp, Mode::Before, T_STEP),
                )
                .await;
            g?;
            edited = json!({"sync_ms": resync, "goals_ms": ms(took)});
        }
        modules.push(json!({
            "module": src.module,
            "positions": positions.len(),
            "repl": {"header_ms": ms(header_t), "elaborate_with_tree_ms": ms(tree_t),
                     "read_positions_ms": ms(read_t), "usage": repl_usage},
            "beam": {"sync_ms": opened.sync_ms, "goals": summarize(&warm),
                     "edit_then_query": edited, "usage": env.usage(slot)},
            "parity": {"same": same, "of": positions.len(), "kinds": kinds, "examples": differ},
        }));
    }
    Ok(json!({"modules": modules}))
}

/// Proof states at each usable block's sorry, in a REPL process.
async fn repl_states(
    env: &Env,
    work: &ModuleWork,
) -> Result<(pisa_repl::Repl, usize, u64, HashMap<u32, u64>, f64)> {
    let slot = env.slot();
    let mut repl = env.repl(slot)?;
    let header = lean::repl_header(&mut repl, &work.source.header).await?;
    let (r, took) = timed(lean::repl_body(&mut repl, header, &work.sorried, None)).await;
    let states = r?
        .sorries
        .iter()
        .filter_map(|s| Some((s.pos?.line as u32, s.proof_state?)))
        .collect();
    Ok((repl, slot, header, states, ms(took)))
}

/// Where Beam probes each block from: its sorry's `run_at_position`.
async fn beam_sorries(opened: &Opened) -> Result<HashMap<u32, LspPos>> {
    let todo = opened
        .beam
        .todo(opened.doc(), opened.version, &["sorry"], T_STEP)
        .await?;
    let map = opened.map();
    Ok(todo
        .items
        .iter()
        .filter_map(|i| {
            let at = i.run_at_position?;
            Some((map.to_body(at)?.0, at))
        })
        .collect())
}

/// Replays each usable block step by step: REPL tactic plus pickle (a
/// session step) from the sorry's proof state, and a Beam handle chain from
/// the sorry's position. Latencies are of the steps that succeeded; the
/// paired figures use only the blocks both backends replayed to the end.
pub async fn replay(env: &Env, workload: &Workload) -> Result<Value> {
    let mut modules = Vec::new();
    for work in &workload.modules {
        let blocks: Vec<&Block> = work.usable().collect();
        if blocks.is_empty() {
            continue;
        }
        let (mut repl, rslot, _, states, repl_body_ms) = repl_states(env, work).await?;
        let pickles = env.slot_dir(rslot).join("pickles");
        std::fs::create_dir_all(&pickles)?;
        // Per block: each successful step's (tactic, tactic + pickle), and
        // whether the steps closed the goal.
        let mut repl_runs: Vec<(Vec<(f64, f64)>, bool)> = Vec::new();
        for (n, block) in blocks.iter().enumerate() {
            let mut steps = Vec::new();
            let mut closed = false;
            if let Some(&root) = states.get(&block.sorry_line) {
                let mut state = root;
                for (k, step) in block.steps.iter().enumerate() {
                    let (s, t) = timed(lean::repl_tactic(&mut repl, state, &step.text)).await;
                    let s = s?;
                    let Some(next) = s.proof_state.filter(|_| s.ok) else {
                        break;
                    };
                    let path = pickles.join(format!("{n}-{k}.olean"));
                    let (p, pt) = timed(lean::repl_pickle(&mut repl, next, &path)).await;
                    p?;
                    steps.push((ms(t), ms(t + pt)));
                    state = next;
                    closed = s.complete && k + 1 == block.steps.len();
                }
            }
            repl_runs.push((steps, closed));
        }
        let repl_usage = env.usage(rslot);
        drop(repl);

        let bslot = env.slot();
        let opened = open(env, bslot, &work.source.header, &work.sorried, false).await?;
        let sorries = beam_sorries(&opened).await?;
        let mut beam_runs: Vec<(Vec<f64>, bool)> = Vec::new();
        for block in &blocks {
            let mut steps = Vec::new();
            let mut closed = false;
            if let Some(&at) = sorries.get(&block.sorry_line) {
                let mut handle = None;
                for (k, step) in block.steps.iter().enumerate() {
                    let doc = opened.doc();
                    let (r, t) = match &handle {
                        None => {
                            timed(opened.beam.run_at_handle(
                                doc,
                                opened.version,
                                at,
                                &step.text,
                                T_STEP,
                            ))
                            .await
                        }
                        Some(h) => {
                            timed(opened.beam.run_with_linear(doc, h, &step.text, T_STEP)).await
                        }
                    };
                    let Ok(r) = r else { break };
                    if !r.success {
                        break;
                    }
                    steps.push(ms(t));
                    let done = r.proof_state.as_ref().is_some_and(|p| p.goals.is_empty());
                    closed = done && k + 1 == block.steps.len();
                    match r.next_handle {
                        Some(h) => handle = Some(h),
                        None => break,
                    }
                }
            }
            beam_runs.push((steps, closed));
        }
        let all = |f: &dyn Fn(&(f64, f64)) -> f64| -> Vec<f64> {
            repl_runs
                .iter()
                .flat_map(|(s, _)| s.iter().map(f))
                .collect()
        };
        let beam_first: Vec<f64> = beam_runs
            .iter()
            .filter_map(|(s, _)| s.first().copied())
            .collect();
        let beam_next: Vec<f64> = beam_runs
            .iter()
            .flat_map(|(s, _)| s.iter().skip(1).copied())
            .collect();
        let both: Vec<usize> = (0..blocks.len())
            .filter(|&i| repl_runs[i].1 && beam_runs[i].1)
            .collect();
        let paired_repl: Vec<f64> = both
            .iter()
            .flat_map(|&i| repl_runs[i].0.iter().map(|s| s.1))
            .collect();
        let paired_beam: Vec<f64> = both
            .iter()
            .flat_map(|&i| beam_runs[i].0.iter().copied())
            .collect();
        modules.push(json!({
            "module": work.source.module,
            "blocks": blocks.len(),
            "steps": blocks.iter().map(|b| b.steps.len()).sum::<usize>(),
            "repl": {"elaborate_sorried_ms": repl_body_ms,
                     "closed": repl_runs.iter().filter(|r| r.1).count(),
                     "steps_ok": repl_runs.iter().map(|r| r.0.len()).sum::<usize>(),
                     "tactic": summarize(&all(&|s| s.0)), "tactic_and_pickle": summarize(&all(&|s| s.1)),
                     "usage": repl_usage},
            "beam": {"sync_sorried_ms": opened.sync_ms,
                     "closed": beam_runs.iter().filter(|r| r.1).count(),
                     "steps_ok": beam_runs.iter().map(|r| r.0.len()).sum::<usize>(),
                     "first_step": summarize(&beam_first), "next_steps": summarize(&beam_next),
                     "usage": env.usage(bslot)},
            "paired": {"blocks": both.len(), "repl_tactic_and_pickle": summarize(&paired_repl),
                       "beam_step": summarize(&paired_beam)},
        }));
    }
    Ok(json!({"modules": modules}))
}

#[derive(Debug, Clone, Serialize)]
struct Tried {
    ms: f64,
    ok: bool,
}

fn candidates_of(block: &Block) -> Vec<String> {
    let mut c: Vec<String> = AUTOMATION.iter().map(|s| s.to_string()).collect();
    c.push(block.steps[0].text.clone());
    c.push(block.whole());
    c
}

/// Candidates at each sorry: T1 (exploration: a tactic on a held state
/// against a Beam probe, one at a time) and T2 (acceptance: the body with the
/// candidate elaborated, then the axiom report, as one `/check/batch` item).
pub async fn candidates(
    env: &Env,
    workload: &Workload,
    per_module: usize,
    t2_per_module: usize,
) -> Result<Value> {
    let mut modules = Vec::new();
    for work in &workload.modules {
        let blocks: Vec<&Block> = work.usable().take(per_module).collect();
        if blocks.is_empty() {
            continue;
        }
        let (mut repl, rslot, header, states, _) = repl_states(env, work).await?;
        let mut repl_t1 = Vec::new();
        for block in &blocks {
            let Some(&root) = states.get(&block.sorry_line) else {
                continue;
            };
            for cand in candidates_of(block) {
                let (s, t) = timed(lean::repl_tactic(&mut repl, root, &cand)).await;
                let ok = s.map(|s| s.ok && s.complete).unwrap_or(false);
                repl_t1.push(Tried { ms: ms(t), ok });
            }
        }
        let mut repl_t2 = Vec::new();
        let mut reports = Vec::new();
        for block in blocks.iter().take(t2_per_module) {
            for cand in [block.whole(), "simp".to_string()] {
                let body = work.filled(block, &cand);
                let start = Instant::now();
                let r = lean::repl_body(&mut repl, header, &body, None).await?;
                let report = lean::repl_report(&mut repl, r.env).await?;
                repl_t2.push(ms(start.elapsed()));
                reports.push((body, report));
            }
        }
        let repl_usage = env.usage(rslot);
        drop(repl);

        let bslot = env.slot();
        let mut opened = open(env, bslot, &work.source.header, &work.sorried, false).await?;
        let sorries = beam_sorries(&opened).await?;
        let mut beam_t1 = Vec::new();
        for block in &blocks {
            let Some(&at) = sorries.get(&block.sorry_line) else {
                continue;
            };
            for cand in candidates_of(block) {
                let (r, t) =
                    timed(
                        opened
                            .beam
                            .run_at(opened.doc(), opened.version, at, &cand, T_STEP),
                    )
                    .await;
                let ok = r
                    .map(|r| r.success && r.proof_state.is_some_and(|p| p.goals.is_empty()))
                    .unwrap_or(false);
                beam_t1.push(Tried { ms: ms(t), ok });
            }
        }
        // T2 on Beam: the document rewritten, synced, and the report probed
        // at its anchor.
        let mut beam_t2 = Vec::new();
        let mut agree = 0;
        let mut disagreements = Vec::new();
        for (body, repl_report) in &reports {
            let (text, anchor) = anchored(&lean::document(&work.source.header, body).0);
            let start = Instant::now();
            opened.rewrite(text).await?;
            let r = opened
                .beam
                .run_at(opened.doc(), opened.version, anchor, AXIOM_REPORT, T_STEP)
                .await?;
            beam_t2.push(ms(start.elapsed()));
            let beam_report = lean::beam_report(&r.messages);
            if beam_report == *repl_report {
                agree += 1;
            } else if disagreements.len() < 3 {
                let texts: Vec<&str> = r.messages.iter().map(|m| m.text.as_str()).collect();
                let first = match (&beam_report, repl_report) {
                    (Some(b), Some(a)) => a
                        .iter()
                        .zip(b)
                        .find(|(x, y)| x != y)
                        .map(|(x, y)| format!("{x:?}\n  vs\n{y:?}")),
                    _ => None,
                };
                disagreements.push(json!({
                    "repl_declarations": repl_report.as_ref().map(Vec::len),
                    "beam_declarations": beam_report.as_ref().map(Vec::len),
                    "beam_why": beam_report.is_none().then(|| lean::why_no_report(&texts)),
                    "first_difference": first,
                }));
            }
        }
        modules.push(json!({
            "module": work.source.module,
            "blocks": blocks.len(),
            "repl": {"t1": tried(&repl_t1), "t2": summarize(&repl_t2), "usage": repl_usage},
            "beam": {"t1": tried(&beam_t1), "t2": summarize(&beam_t2), "usage": env.usage(bslot)},
            "report_agreement": {"same": agree, "of": reports.len(), "disagreements": disagreements},
        }));
    }
    Ok(json!({"modules": modules}))
}

fn tried(samples: &[Tried]) -> Value {
    let ms: Vec<f64> = samples.iter().map(|s| s.ms).collect();
    json!({"latency": summarize(&ms), "closed": samples.iter().filter(|s| s.ok).count()})
}

/// Starting a session: REPL import, body, report and a pickle per sorry;
/// Beam spawn, sync and the sorries' proof states, fresh and on an open
/// document whose body changed.
pub async fn session(env: &Env, workload: &Workload) -> Result<Value> {
    let mut modules = Vec::new();
    for work in &workload.modules {
        let slot = env.slot();
        let start = Instant::now();
        let mut repl = env.repl(slot)?;
        let (header, header_t) = timed(lean::repl_header(&mut repl, &work.source.header)).await;
        let header = header?;
        let (r, body_t) = timed(lean::repl_body(&mut repl, header, &work.sorried, None)).await;
        let r = r?;
        let (_, report_t) = timed(lean::repl_report(&mut repl, r.env)).await;
        let dir = env.slot_dir(slot).join("pickles");
        std::fs::create_dir_all(&dir)?;
        let pickle_start = Instant::now();
        let mut pickled = 0;
        for (i, s) in r.sorries.iter().enumerate() {
            if let Some(ps) = s.proof_state {
                lean::repl_pickle(&mut repl, ps, &dir.join(format!("{i}.olean"))).await?;
                pickled += 1;
            }
        }
        let pickle_t = pickle_start.elapsed();
        let repl_total = start.elapsed();
        let repl_usage = env.usage(slot);
        drop(repl);

        let slot = env.slot();
        let start = Instant::now();
        let (beam, spawn_t) = timed(env.beam(slot)).await;
        let beam = beam?;
        let ws = env.workspace(slot, "ws")?;
        let (text, offset) = lean::document(&work.source.header, &work.sorried);
        let path = doc_path("Doc");
        ws.write(&path, &text)?;
        let (sync, sync_t) = timed(beam.sync(ws.doc(&path), false, T_BODY)).await;
        let version = sync?.version;
        let (todo, todo_t) = timed(beam.todo(ws.doc(&path), version, &["sorry"], T_STEP)).await;
        let sorries = todo?.items.len();
        let beam_total = start.elapsed();
        // The same open document, one proof filled: what a session's next start costs.
        let mut opened = Opened {
            beam,
            ws,
            path,
            version,
            map_offset: offset,
            text,
            sync_ms: ms(sync_t),
        };
        let mut reused = Value::Null;
        if let Some(block) = work.usable().next() {
            let (text, _) =
                lean::document(&work.source.header, &work.filled(block, &block.whole()));
            let start = Instant::now();
            opened.rewrite(text).await?;
            let todo = opened
                .beam
                .todo(opened.doc(), opened.version, &["sorry"], T_STEP)
                .await?;
            reused = json!({"ms": ms(start.elapsed()), "sorries": todo.items.len()});
        }
        modules.push(json!({
            "module": work.source.module,
            "repl": {"total_ms": ms(repl_total), "header_ms": ms(header_t), "body_ms": ms(body_t),
                     "report_ms": ms(report_t), "pickles": pickled, "pickles_ms": ms(pickle_t), "usage": repl_usage},
            "beam": {"total_ms": ms(beam_total), "spawn_ms": ms(spawn_t), "sync_ms": ms(sync_t),
                     "todo_ms": ms(todo_t), "sorries": sorries, "reused": reused, "usage": env.usage(slot)},
        }));
    }
    Ok(json!({"modules": modules}))
}

/// Concurrent Beam probes on one process: throughput at each level.
pub async fn concurrency(
    env: &Env,
    workload: &Workload,
    per_module: usize,
    levels: &[usize],
) -> Result<Value> {
    // One module with enough candidates, on one process.
    let Some(work) = workload
        .modules
        .iter()
        .max_by_key(|w| w.usable().take(per_module).count())
    else {
        bail!("no module with usable blocks");
    };
    let slot = env.slot();
    let opened = open(env, slot, &work.source.header, &work.sorried, false).await?;
    let sorries = beam_sorries(&opened).await?;
    let jobs: Vec<(LspPos, String)> = work
        .usable()
        .take(per_module)
        .filter_map(|b| Some((*sorries.get(&b.sorry_line)?, b)))
        .flat_map(|(at, b)| candidates_of(b).into_iter().map(move |c| (at, c)))
        .collect();
    let mut out = Vec::new();
    for &level in levels {
        let start = Instant::now();
        let results: Vec<Result<pisa_beam::RunResult, BeamError>> = futures::stream::iter(&jobs)
            .map(|(at, cand)| {
                opened
                    .beam
                    .run_at(opened.doc(), opened.version, *at, cand, T_STEP)
            })
            .buffer_unordered(level)
            .collect()
            .await;
        let wall = start.elapsed();
        let busy = results
            .iter()
            .filter(|r| matches!(r, Err(BeamError::Busy(_))))
            .count();
        let timeouts = results
            .iter()
            .filter(|r| matches!(r, Err(BeamError::Timeout)))
            .count();
        let errors = results.iter().filter(|r| r.is_err()).count();
        out.push(json!({
            "concurrency": level, "probes": jobs.len(), "wall_ms": ms(wall),
            "per_second": jobs.len() as f64 / wall.as_secs_f64(),
            "busy": busy, "timeouts": timeouts, "errors": errors,
        }));
    }
    Ok(json!({"module": work.source.module, "levels": out, "usage": env.usage(slot)}))
}
