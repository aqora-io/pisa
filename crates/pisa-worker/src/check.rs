//! Whole-proof checks: import the header once per process, run only the body,
//! then apply the verification policy.

use std::sync::Arc;
use std::time::{Duration, Instant};

use pisa_protocol::api::{Detail, Error, ErrorKind};
use pisa_protocol::header::split_header;
use pisa_protocol::repl::{
    self, CommandResponse, Pos, ReplCommand, ReplResponse, Severity, at_positions, tactics,
};
use pisa_protocol::verify::Verification;
use pisa_protocol::wire::{CheckJob, CheckOutcome};
use pisa_repl::ReplFailure;

use crate::policy::{AXIOM_REPORT, Finding};
use crate::pool::{HeaderFailure, Lease, Pool};

/// Time allowed for the axiom report, which only walks the new declarations.
const REPORT_TIMEOUT: Duration = Duration::from_secs(60);

pub async fn run_check(pool: &Arc<Pool>, job: CheckJob) -> CheckOutcome {
    let split = split_header(&job.code);
    let mut outcome = CheckOutcome {
        time: 0.0,
        error: None,
        response: None,
        verification: None,
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
    let mut slot = pool.acquire(&split.header_key, None).await;

    let env = match slot.warm_up(&split.header, &split.header_key).await {
        Ok((env, _)) => env,
        Err(HeaderFailure::Lean(r)) => {
            // Header positions are already those of the submitted code, and
            // its error messages say what went wrong.
            outcome.verification = Some(slot.policy().judge(&r, None));
            outcome.response = Some(*r);
            return outcome;
        }
        Err(other) => {
            outcome.error = Some(header_error(other, slot.import_timeout()));
            return outcome;
        }
    };

    let cmd = ReplCommand::Cmd {
        cmd: split.body.clone(),
        env: Some(env),
        all_tactics: None,
        root_goals: None,
        infotree: infotree(&job).map(str::to_string),
    };
    let start = Instant::now();
    let result = run(&mut slot, &cmd, Duration::from_secs(job.timeout_secs)).await;
    outcome.time = round6(start.elapsed().as_secs_f64());

    match result {
        Ok(ReplResponse::Command(mut r)) => {
            let report = axiom_report(&mut slot, &r).await;
            outcome.verification = Some(slot.policy().judge(&r, report.as_ref()));
            read_tree(&mut r, &job, &split.body, split.header_lines);
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
            }
            outcome.error = Some(e);
        }
    }
    slot.finish_job();
    outcome
}

/// Reads what `job` asked for from the info tree of the response to `body`,
/// then drops the tree: `tactics`, and an entry for each position in `at`,
/// which is in the lines of the code, `header_lines` more than the body's.
fn read_tree(r: &mut CommandResponse, job: &CheckJob, body: &str, header_lines: u64) {
    // The REPL leaves out a tree with nothing in it.
    let tree = r.infotree.take().unwrap_or_else(|| serde_json::json!([]));
    if job.detail.contains(&Detail::Tactics) {
        match tactics(&tree, body) {
            Ok(t) => r.tactics = t,
            Err(e) => tracing::warn!("unreadable info tree: {e}"),
        }
    }
    if job.at.is_empty() {
        return;
    }
    let positions: Vec<Pos> = job
        .at
        .iter()
        .map(|p| Pos {
            line: p.line.saturating_sub(header_lines),
            column: p.column,
        })
        .collect();
    let terms = job.detail.contains(&Detail::Terms);
    match at_positions(&tree, body, &positions, terms) {
        Ok(at) => r.at = at,
        Err(e) => tracing::warn!("unreadable info tree: {e}"),
    }
}

/// What of the info tree the REPL prints: every node for terms, which costs
/// several times the check; the original tactics for tactics and goals.
fn infotree(job: &CheckJob) -> Option<&'static str> {
    if job.at.is_empty() && !job.detail.contains(&Detail::Tactics) {
        None
    } else if job.detail.contains(&Detail::Terms) {
        Some("full")
    } else {
        Some("original")
    }
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
                Error::new(ErrorKind::OutOfMemory, "Lean exceeded its memory limit")
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

fn round6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

#[cfg(test)]
mod tests {
    use super::*;
    use pisa_protocol::api;

    #[test]
    fn only_terms_ask_for_the_full_tree() {
        let job = |detail: &[Detail], at: usize| CheckJob {
            code: String::new(),
            timeout_secs: 1,
            detail: detail.to_vec(),
            at: vec![api::Pos { line: 1, column: 0 }; at],
        };
        assert_eq!(infotree(&job(&[], 0)), None);
        assert_eq!(infotree(&job(&[Detail::Tactics], 0)), Some("original"));
        assert_eq!(infotree(&job(&[], 2)), Some("original"));
        assert_eq!(infotree(&job(&[Detail::Terms], 2)), Some("full"));
        let both = [Detail::Tactics, Detail::Terms];
        assert_eq!(infotree(&job(&both, 2)), Some("full"));
        // The server refuses terms without positions; a worker prints none.
        assert_eq!(infotree(&job(&[Detail::Terms], 0)), None);
    }
}
