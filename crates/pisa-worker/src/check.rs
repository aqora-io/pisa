//! Whole-proof checks: import the header once per process, run only the body.

use std::sync::Arc;
use std::time::{Duration, Instant};

use pisa_protocol::header::split_header;
use pisa_protocol::kimina::{CheckOutput, header_timeout_error, timeout_error};
use pisa_protocol::repl::{ReplCommand, ReplResponse};
use pisa_protocol::wire::{CheckJob, CheckOutcome};
use pisa_repl::ReplFailure;

use crate::pool::{HeaderFailure, Pool};

pub async fn run_check(pool: &Arc<Pool>, job: CheckJob) -> CheckOutcome {
    let split = split_header(&job.code);
    let mut slot = pool.acquire(&split.header_key).await;
    let mut outcome = CheckOutcome {
        time: 0.0,
        error: None,
        response: None,
        verification: None,
        warm: false,
    };

    let env = match slot.warm_up(&split.header, &split.header_key).await {
        Ok((env, warm)) => {
            outcome.warm = warm;
            env
        }
        Err(HeaderFailure::Lean(r)) => {
            // Header positions are already those of the submitted code.
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
    let repl = slot.repl().expect("warm_up leaves a live process");
    let start = Instant::now();
    let result = repl.send(&cmd, Duration::from_secs(job.timeout_secs)).await;
    outcome.time = round6(start.elapsed().as_secs_f64());

    match result {
        Ok(ReplResponse::Command(mut r)) => {
            r.shift_lines(split.header_lines);
            outcome.response = Some(CheckOutput::Command(r));
        }
        Ok(ReplResponse::Error(e)) => outcome.response = Some(CheckOutput::Error(e)),
        Ok(other) => outcome.error = Some(format!("unexpected REPL response: {other:?}")),
        Err(ReplFailure::Timeout) => {
            slot.retire("timeout");
            outcome.error = Some(timeout_error(job.timeout_secs));
            outcome.time = job.timeout_secs as f64;
        }
        Err(e) => {
            slot.retire("crash");
            outcome.error = Some(format!("Lean REPL failed: {e}"));
        }
    }
    slot.finish_job(job.reuse);
    outcome
}

fn round6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}
