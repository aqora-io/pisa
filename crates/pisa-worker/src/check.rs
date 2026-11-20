//! Whole-proof checks: import the header once per process, run only the body,
//! then apply the verification policy. Goals and tactics are traced in a
//! second run of the body, so that a trace that fails keeps the first's
//! diagnostics.

use std::sync::Arc;
use std::time::{Duration, Instant};

use pisa_protocol::api::{Detail, Error, ErrorKind, Replaces, Timing};
use pisa_protocol::header::{header_to_code, split_header};
use pisa_protocol::repl::{
    self, CommandResponse, Pos, ReplCommand, ReplResponse, Severity, at_positions, tactics,
};
use pisa_protocol::verify::{Change, Verification, statement_changes};
use pisa_protocol::wire::{CheckJob, CheckOutcome};
use pisa_repl::ReplFailure;
use serde_json::Value;

use crate::policy::{AXIOM_REPORT, Finding, auto_implicit_hint};
use crate::pool::{HeaderFailure, Lease, Pool};

/// Time allowed for the axiom report, which only walks the new declarations.
const REPORT_TIMEOUT: Duration = Duration::from_secs(60);

/// Compares the statements the checked code shares with snapshot `id`, a
/// check's `against`: each declaration of both that is another kind or has
/// another type is one of the verification's `changes` and `reasons`, with
/// the snapshot's signature as `was`, and the code isn't `ok`.
pub async fn compare_against(pool: &Arc<Pool>, id: &str, outcome: &mut CheckOutcome) {
    if outcome.verification.is_none() {
        return;
    }
    let reader = pool.snapshots().reader();
    let replaces = Replaces {
        snapshot_id: id.to_string(),
        allow: None,
    };
    let spec = match reader.replacing(&replaces).await {
        Ok(spec) => spec,
        Err(e) => {
            outcome.error = Some(e);
            return;
        }
    };
    let Some(v) = outcome.verification.as_mut() else {
        return;
    };
    let mut changes: Vec<_> = statement_changes(&spec.declarations, &v.declarations)
        .into_iter()
        .filter(|c| matches!(c.change, Change::Kind | Change::Type))
        .collect();
    if changes.is_empty() {
        return;
    }
    let now = std::collections::HashMap::new();
    reader.sign_from(&mut changes, &[spec.module], &now).await;
    v.ok = false;
    v.reasons.extend(changes.iter().map(ToString::to_string));
    v.changes.extend(changes);
}

pub async fn run_check(pool: &Arc<Pool>, job: CheckJob) -> CheckOutcome {
    let split = split_header(&job.code);
    let mut outcome = CheckOutcome {
        time: 0.0,
        timing: Timing::default(),
        error: None,
        response: None,
        verification: None,
        runs: job.runs.clone(),
        warnings: Vec::new(),
    };

    // Before waiting for a slot: refused code needs none.
    let found = pool.policy().findings(&split.body, false);
    if !found.is_empty() {
        outcome.error = Some(policy_error(&found, split.header_lines));
        outcome.response = Some(CommandResponse {
            env: 0,
            messages: policy_messages(&found, split.header_lines),
            sorries: Vec::new(),
            tactics: Vec::new(),
            at: Vec::new(),
            infotree: None,
        });
        outcome.verification = Some(Verification {
            ok: false,
            reasons: found.into_iter().map(|f| f.reason).collect(),
            declarations: Vec::new(),
            changes: Vec::new(),
        });
        return outcome;
    }
    // Nor does a header whose imports declare one name twice. One whose
    // imports can't be read is left to the import to report.
    if let Ok(Some(e)) = pool.snapshots().clash(&split.header).await {
        outcome.error = Some(e);
        return outcome;
    }
    let mut slot = pool.acquire(&split.header_key).await;

    let loading = Instant::now();
    let warmed = slot.warm_up(&split.header, &split.header_key).await;
    // Importing is the load, unless the process had the header already.
    if !matches!(warmed, Ok((_, true))) {
        outcome.timing.load = round6(loading.elapsed().as_secs_f64());
    }
    let env = match warmed {
        Ok((env, _)) => env,
        Err(HeaderFailure::Lean(mut r)) => {
            // Its error messages say what went wrong, at positions in the
            // normalised header: placed on the submitted lines.
            let submitted = job.source.as_deref().unwrap_or(&job.code);
            let place = |p: &mut Pos| {
                if let Some((line, column)) = header_to_code(&split, submitted, p.line, p.column) {
                    *p = Pos { line, column };
                }
            };
            for m in &mut r.messages {
                place(&mut m.pos);
                m.end_pos.as_mut().map(place);
            }
            outcome.verification = Some(slot.policy().judge(&r, None));
            outcome.response = Some(*r);
            return outcome;
        }
        Err(other) => {
            outcome.error = Some(header_error(other, slot.import_timeout()));
            return outcome;
        }
    };

    let timeout = Duration::from_secs(job.timeout_secs);
    let cmd = ReplCommand::cmd(split.body.clone(), Some(env));
    let start = Instant::now();
    let result = run(&mut slot, &cmd, timeout).await;
    outcome.time = round6(start.elapsed().as_secs_f64());
    outcome.timing.execution = outcome.time;

    match result {
        Ok(ReplResponse::Command(mut r)) => {
            let verifying = Instant::now();
            let report = axiom_report(&mut slot, &r).await;
            outcome.timing.verification = round6(verifying.elapsed().as_secs_f64());
            outcome.verification = Some(slot.policy().judge(&r, report.as_ref()));
            if let Some(hint) = auto_implicit_hint(&split.body, &r.messages) {
                r.messages.push(hint);
            }
            if let Some((mode, at)) = infotree(&job, split.header_lines) {
                let cmd = ReplCommand::Cmd {
                    cmd: split.body.clone(),
                    env: Some(env),
                    all_tactics: None,
                    root_goals: None,
                    infotree: Some(mode.to_string()),
                    infotree_at: at,
                };
                let tracing = Instant::now();
                let traced = trace(&mut slot, &cmd, timeout).await;
                outcome.timing.tracing = round6(tracing.elapsed().as_secs_f64());
                match traced {
                    Ok(tree) => {
                        read_tree(&mut r, &tree, &job, &split.body, split.header_lines);
                        let slow = slow_trace(outcome.timing.execution, outcome.timing.tracing);
                        outcome.warnings.extend(slow);
                    }
                    Err(e) => outcome.warnings.push(trace_failed(&e, job.timeout_secs)),
                }
            }
            r.shift_lines(split.header_lines);
            // Positions in the header were asked about on line 0 of the body.
            for (at, asked) in r.at.iter_mut().zip(&job.at) {
                at.pos = Pos {
                    line: asked.line,
                    column: asked.column,
                };
            }
            outcome.response = Some(r);
        }
        Ok(ReplResponse::Error(e)) => outcome.error = Some(Error::new(ErrorKind::Lean, e.message)),
        Ok(other) => outcome.error = Some(unexpected(other)),
        Err(e) => {
            if e.kind == ErrorKind::Timeout {
                outcome.time = job.timeout_secs as f64;
                outcome.timing.execution = outcome.time;
            }
            outcome.error = Some(e);
        }
    }
    slot.finish_job();
    outcome
}

/// Reads what `job` asked for from `tree`, the info tree of `body`:
/// `tactics`, and an entry for each position in `at`, which is in the lines
/// of the code, `header_lines` more than the body's.
fn read_tree(r: &mut CommandResponse, tree: &Value, job: &CheckJob, body: &str, header_lines: u64) {
    if job.detail.contains(&Detail::Tactics) {
        match tactics(tree, body) {
            Ok(t) => r.tactics = t,
            Err(e) => tracing::warn!("unreadable info tree: {e}"),
        }
    }
    if job.at.is_empty() {
        return;
    }
    let positions = body_positions(job, header_lines);
    let terms = job.detail.contains(&Detail::Terms);
    match at_positions(tree, body, &positions, terms) {
        Ok(at) => r.at = at,
        Err(e) => tracing::warn!("unreadable info tree: {e}"),
    }
}

/// The positions of `job.at` in the body, below `header_lines` lines of
/// the code; those in the header are on line 0.
fn body_positions(job: &CheckJob, header_lines: u64) -> Vec<Pos> {
    job.at
        .iter()
        .map(|p| Pos {
            line: p.line.saturating_sub(header_lines),
            column: p.column,
        })
        .collect()
}

/// What the traced run has the REPL print of the info tree, if `job` asks
/// for any of it: every node for terms, which costs several times the
/// check; the original tactics otherwise. Tactics need the whole tree;
/// positions alone only what contains one of them, in the body below
/// `header_lines`.
fn infotree(job: &CheckJob, header_lines: u64) -> Option<(&'static str, Option<Vec<Pos>>)> {
    if job.at.is_empty() && !job.detail.contains(&Detail::Tactics) {
        return None;
    }
    let mode = match job.detail.contains(&Detail::Terms) {
        true => "full",
        false => "original",
    };
    let at = (!job.detail.contains(&Detail::Tactics)).then(|| body_positions(job, header_lines));
    Some((mode, at))
}

/// Runs the body again for its info tree, which the REPL leaves out when it
/// has nothing in it.
async fn trace(slot: &mut Lease, cmd: &ReplCommand, timeout: Duration) -> Result<Value, Error> {
    match run(slot, cmd, timeout).await? {
        ReplResponse::Command(r) => Ok(r.infotree.unwrap_or_else(|| serde_json::json!([]))),
        ReplResponse::Error(e) => Err(Error::new(ErrorKind::Lean, e.message)),
        other => Err(unexpected(other)),
    }
}

/// The warning for a trace that failed with `e`: the result has only what
/// the plain run found.
fn trace_failed(e: &Error, timeout_secs: u64) -> String {
    let what = match e.kind {
        ErrorKind::Timeout => format!("ran past {timeout_secs}s"),
        _ => format!("failed ({})", e.message),
    };
    format!("goal reporting {what}; diagnostics are from a plain check")
}

/// The warning for a trace that took more than three times the check, and
/// a second or more longer: three times a few milliseconds costs nothing.
fn slow_trace(execution: f64, tracing: f64) -> Option<String> {
    let slow = tracing > 3.0 * execution && tracing - execution >= 1.0;
    (execution > 0.0 && slow).then(|| {
        let times = tracing / execution;
        format!("goal reporting took {times:.1} times as long as the check")
    })
}

/// The error for code the policy refuses, which `found` says why, with
/// where the first reason is and what to do instead. The scanned text starts
/// after `header_lines` lines of the code.
pub fn policy_error(found: &[Finding], header_lines: u64) -> Error {
    let reasons: Vec<&str> = found.iter().map(|f| f.reason.as_str()).collect();
    let mut message = format!("rejected: {}", reasons.join(", "));
    if let Some(f) = found.first() {
        message.push_str(&format!(" (line {}: {})", f.line + header_lines, f.hint));
    }
    Error::new(ErrorKind::Policy, message)
}

/// What the policy found, as messages at their places in the code.
pub fn policy_messages(found: &[Finding], header_lines: u64) -> Vec<repl::Message> {
    let message = |f: &Finding| repl::Message {
        severity: Severity::Error,
        pos: repl::Pos {
            line: f.line + header_lines,
            column: f.column,
        },
        end_pos: None,
        data: format!("{}: {}", f.reason, f.hint),
    };
    found.iter().map(message).collect()
}

pub fn unexpected(response: ReplResponse) -> Error {
    Error::new(
        ErrorKind::Internal,
        format!("unexpected REPL response: {response:?}"),
    )
}

/// The error for a header that could not be imported.
pub fn header_error(failure: HeaderFailure, import_timeout: Duration) -> Error {
    match failure {
        HeaderFailure::Lean(r) => {
            let first = r
                .messages
                .iter()
                .find(|m| m.severity == Severity::Error)
                .map_or("errors in the imports", |m| &m.data);
            Error::new(ErrorKind::Lean, format!("header failed: {first}"))
        }
        HeaderFailure::Timeout => Error::new(
            ErrorKind::Timeout,
            format!(
                "importing the header took more than {}s",
                import_timeout.as_secs()
            ),
        ),
        HeaderFailure::Failed(e) => e,
    }
}

/// Sends one command on the slot's live process. On failure the process is
/// retired with its cause.
pub async fn run(
    slot: &mut Lease,
    cmd: &ReplCommand,
    timeout: Duration,
) -> Result<ReplResponse, Error> {
    let Some(repl) = slot.repl() else {
        return Err(Error::new(ErrorKind::Crash, "no live Lean process"));
    };
    match repl.send(cmd, timeout).await {
        Ok(r) => Ok(r),
        Err(ReplFailure::Timeout) => {
            slot.retire("timeout");
            Err(Error::timeout(timeout.as_secs()))
        }
        Err(e) => {
            let cause = slot.failure_cause();
            slot.retire(cause);
            Err(if cause == "oom" {
                crate::limits::out_of_memory("Lean", slot.memory_cap())
            } else {
                Error::new(ErrorKind::Crash, format!("Lean process failed: {e}"))
            })
        }
    }
}

/// Runs the axiom report in the environment the body produced, errors or
/// not: a declaration Lean took despite an error in another still has a
/// status.
pub async fn axiom_report(slot: &mut Lease, body: &CommandResponse) -> Option<CommandResponse> {
    let cmd = ReplCommand::cmd(AXIOM_REPORT, Some(body.env));
    match run(slot, &cmd, REPORT_TIMEOUT).await {
        Ok(ReplResponse::Command(r)) => Some(r),
        Ok(other) => {
            tracing::warn!("axiom report failed: {other:?}");
            None
        }
        Err(_) => None,
    }
}

pub(crate) fn round6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

#[cfg(test)]
mod tests {
    use super::*;
    use pisa_protocol::api;

    #[test]
    fn only_terms_ask_for_the_full_tree_and_only_tactics_for_all_of_it() {
        let job = |detail: &[Detail], at: usize| CheckJob {
            code: String::new(),
            timeout_secs: 1,
            detail: detail.to_vec(),
            at: vec![api::Pos { line: 3, column: 2 }; at],
            runs: Vec::new(),
            source: None,
            against: None,
        };
        // Positions are in the body's lines, below a header of 2.
        let traced = |detail: &[Detail], at| infotree(&job(detail, at), 2);
        let at = |n| Some(vec![Pos { line: 1, column: 2 }; n]);
        assert_eq!(traced(&[], 0), None);
        assert_eq!(traced(&[Detail::Tactics], 0), Some(("original", None)));
        assert_eq!(traced(&[Detail::Tactics], 2), Some(("original", None)));
        assert_eq!(traced(&[], 2), Some(("original", at(2))));
        assert_eq!(traced(&[Detail::Terms], 2), Some(("full", at(2))));
        let both = [Detail::Tactics, Detail::Terms];
        assert_eq!(traced(&both, 2), Some(("full", None)));
        // The server refuses terms without positions; a worker prints none.
        assert_eq!(traced(&[Detail::Terms], 0), None);
    }

    #[test]
    fn a_trace_warns_when_it_fails_or_takes_much_longer_than_the_check() {
        let timeout = Error::timeout(90);
        assert_eq!(
            trace_failed(&timeout, 90),
            "goal reporting ran past 90s; diagnostics are from a plain check"
        );
        let crash = Error::new(ErrorKind::Crash, "Lean process failed: killed");
        assert_eq!(
            trace_failed(&crash, 90),
            "goal reporting failed (Lean process failed: killed); diagnostics are \
             from a plain check"
        );
        assert_eq!(slow_trace(5.3, 15.0), None);
        assert_eq!(
            slow_trace(5.3, 129.7).as_deref(),
            Some("goal reporting took 24.5 times as long as the check")
        );
        // Nor does a trace less than a second longer, however many times
        // the check it took.
        assert_eq!(slow_trace(0.0004, 0.0016), None);
        assert_eq!(slow_trace(0.2, 1.1), None);
        assert_eq!(
            slow_trace(0.2, 1.3).as_deref(),
            Some("goal reporting took 6.5 times as long as the check")
        );
        // A check that took no measurable time has nothing to compare with.
        assert_eq!(slow_trace(0.0, 1.0), None);
    }
}
