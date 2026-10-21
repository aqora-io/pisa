//! Whole-proof checks: import the header once per process, run only the body,
//! then apply the verification policy.

use std::sync::Arc;
use std::time::{Duration, Instant};

use pisa_protocol::header::split_header;
use pisa_protocol::kimina::{CheckOutput, header_timeout_error, timeout_error};
use pisa_protocol::repl::{CommandResponse, ReplCommand, ReplResponse};
use pisa_protocol::verify::Verification;
use pisa_protocol::wire::{CheckJob, CheckOutcome};
use pisa_repl::ReplFailure;

use crate::policy::AXIOM_REPORT;
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
        warm: false,
    };

    let mut slot = pool.acquire(&split.header_key, None).await;
    let rejected = slot.policy().scan(&split.body);
    if !rejected.is_empty() {
        outcome.error = Some(format!("Rejected by policy: {}", rejected.join(", ")));
        outcome.verification = Some(Verification {
            ok: false,
            reasons: rejected,
            declarations: Vec::new(),
        });
        return outcome;
    }

    let env = match slot.warm_up(&split.header, &split.header_key).await {
        Ok((env, warm)) => {
            outcome.warm = warm;
            env
        }
        Err(HeaderFailure::Lean(r)) => {
            // Header positions are already those of the submitted code.
            outcome.verification = Some(slot.policy().judge(&r, None));
            outcome.response = Some(CheckOutput::Command(r));
            return outcome;
        }
        Err(HeaderFailure::Timeout) => {
            outcome.error = Some(header_timeout_error(slot.import_timeout().as_secs()));
            return outcome;
        }
        Err(HeaderFailure::Repl(e)) => {
            outcome.error = Some(format!("Lean REPL failed on header: {e}"));
            return outcome;
        }
    };

    let cmd = ReplCommand::Cmd {
        cmd: split.body.clone(),
        env: Some(env),
        all_tactics: None,
        root_goals: None,
        infotree: job.infotree.clone(),
    };
    let start = Instant::now();
    let result = run(&mut slot, &cmd, Duration::from_secs(job.timeout_secs)).await;
    outcome.time = round6(start.elapsed().as_secs_f64());

    match result {
        Ok(ReplResponse::Command(mut r)) => {
            let report = axiom_report(&mut slot, &r).await;
            outcome.verification = Some(slot.policy().judge(&r, report.as_ref()));
            r.shift_lines(split.header_lines);
            outcome.response = Some(CheckOutput::Command(r));
        }
        Ok(ReplResponse::Error(e)) => outcome.response = Some(CheckOutput::Error(e)),
        Ok(other) => outcome.error = Some(format!("unexpected REPL response: {other:?}")),
        Err(Failure::Timeout) => {
            outcome.error = Some(timeout_error(job.timeout_secs));
            outcome.time = job.timeout_secs as f64;
        }
        Err(Failure::OutOfMemory) => outcome.error = Some("Lean REPL ran out of memory".into()),
        Err(Failure::Crashed(e)) => outcome.error = Some(format!("Lean REPL failed: {e}")),
    }
    slot.finish_job(job.reuse);
    outcome
}

pub enum Failure {
    Timeout,
    OutOfMemory,
    Crashed(String),
}

/// Sends one command on the slot's live process. On failure the process is
/// retired with its cause.
pub async fn run(
    slot: &mut Lease,
    cmd: &ReplCommand,
    timeout: Duration,
) -> Result<ReplResponse, Failure> {
    let Some(repl) = slot.repl() else {
        return Err(Failure::Crashed("no live process".into()));
    };
    match repl.send(cmd, timeout).await {
        Ok(r) => Ok(r),
        Err(ReplFailure::Timeout) => {
            slot.retire("timeout");
            Err(Failure::Timeout)
        }
        Err(e) => {
            let cause = slot.failure_cause();
            slot.retire(cause);
            Err(if cause == "oom" {
                Failure::OutOfMemory
            } else {
                Failure::Crashed(e.to_string())
            })
        }
    }
}

/// Runs the axiom report in the environment the body produced. Skipped when
/// the body has errors, since the verdict is already negative.
pub async fn axiom_report(slot: &mut Lease, body: &CommandResponse) -> Option<CommandResponse> {
    if body.has_errors() {
        return None;
    }
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
