//! Whole-proof checks: import the header once per process, run only the body,
//! then apply the verification policy.

use std::sync::Arc;
use std::time::{Duration, Instant};

use pisa_protocol::api::{Error, ErrorKind};
use pisa_protocol::header::split_header;
use pisa_protocol::repl::{CommandResponse, ReplCommand, ReplResponse, Severity};
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
        outcome.error = Some(policy_error(&rejected));
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
            // Header positions are already those of the submitted code, and
            // its error messages say what went wrong.
            outcome.verification = Some(slot.policy().judge(&r, None));
            outcome.response = Some(r);
            return outcome;
        }
        Err(other) => {
            outcome.error = Some(header_error(other, slot.import_timeout()));
            return outcome;
        }
    };

    let cmd = ReplCommand::cmd(split.body.clone(), Some(env));
    let start = Instant::now();
    let result = run(&mut slot, &cmd, Duration::from_secs(job.timeout_secs)).await;
    outcome.time = round6(start.elapsed().as_secs_f64());

    match result {
        Ok(ReplResponse::Command(mut r)) => {
            let report = axiom_report(&mut slot, &r).await;
            outcome.verification = Some(slot.policy().judge(&r, report.as_ref()));
            r.shift_lines(split.header_lines);
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

pub fn policy_error(reasons: &[String]) -> Error {
    Error::new(
        ErrorKind::Policy,
        format!("rejected: {}", reasons.join(", ")),
    )
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
