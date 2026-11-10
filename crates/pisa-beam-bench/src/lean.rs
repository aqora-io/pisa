//! Lean code and the two backends' basic operations, as the worker does them:
//! the header the worker sends (`header_command`), the body after it, tactic
//! steps, pickles and the axiom report.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use pisa_beam::RunMessage;
use pisa_protocol::header::split_header;
use pisa_protocol::repl::{CommandResponse, ReplCommand, ReplResponse, Severity};
use pisa_protocol::verify::{DescribedDeclaration, ReportedDeclaration};
use pisa_repl::Repl;
use pisa_worker::policy::{AXIOM_REPORT, header_command, parse_report};
use serde::{Deserialize, Serialize};

pub const T_HEADER: Duration = Duration::from_secs(600);
pub const T_BODY: Duration = Duration::from_secs(1800);
pub const T_STEP: Duration = Duration::from_secs(60);

/// A module of a project dependency, split as the worker splits code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    pub module: String,
    pub header: String,
    pub body: String,
}

/// Reads `module` (e.g. `Physlib.SpaceAndTime.Space.Basic`) from the
/// project's packages.
pub fn load_module(project: &Path, module: &str) -> Result<Source> {
    let package = module.split('.').next().unwrap_or(module);
    let file = project
        .join(".lake/packages")
        .join(package)
        .join(module.replace('.', "/"))
        .with_extension("lean");
    let code =
        std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
    let split = split_header(&code);
    Ok(Source {
        module: module.to_string(),
        header: split.header,
        body: split.body,
    })
}

/// The file Beam reads for `body` under `header`: the worker's header
/// command, then the body. Body line `n` is document line `offset + n - 1`.
pub fn document(header: &str, body: &str) -> (String, u32) {
    let header = header_command(header);
    let offset = header.split('\n').count() as u32;
    (format!("{header}\n{body}"), offset)
}

/// Imports `header` as the worker does; returns its env.
pub async fn repl_header(repl: &mut Repl, header: &str) -> Result<u64> {
    let r = command(
        repl,
        ReplCommand::cmd(header_command(header), None),
        T_HEADER,
    )
    .await?;
    if r.has_errors() {
        bail!("header failed: {}", errors(&r));
    }
    Ok(r.env)
}

pub async fn repl_body(
    repl: &mut Repl,
    env: u64,
    body: &str,
    infotree: Option<&str>,
) -> Result<CommandResponse> {
    let cmd = ReplCommand::Cmd {
        cmd: body.to_string(),
        env: Some(env),
        all_tactics: None,
        root_goals: None,
        infotree: infotree.map(str::to_string),
    };
    command(repl, cmd, T_BODY).await
}

pub async fn repl_report(repl: &mut Repl, env: u64) -> Result<Option<Vec<ReportedDeclaration>>> {
    Ok(parse_report(&repl_report_response(repl, env).await?))
}

/// The REPL's whole answer to the axiom report, messages included.
pub async fn repl_report_response(repl: &mut Repl, env: u64) -> Result<CommandResponse> {
    command(repl, ReplCommand::cmd(AXIOM_REPORT, Some(env)), T_STEP).await
}

/// Why messages hold no report the worker can read: no `PISA_DECLS` message,
/// one that isn't JSON, or JSON that isn't declarations.
pub fn why_no_report(texts: &[&str]) -> String {
    let Some(json) = texts.iter().find_map(|t| t.strip_prefix("PISA_DECLS ")) else {
        let first: Vec<String> = texts
            .iter()
            .take(2)
            .map(|t| t.chars().take(300).collect())
            .collect();
        return format!("no PISA_DECLS message; {} messages: {first:?}", texts.len());
    };
    let value: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(e) => return format!("not JSON ({} bytes): {e}", json.len()),
    };
    match serde_json::from_value::<Vec<DescribedDeclaration>>(value) {
        Ok(d) => format!("{} declarations parse", d.len()),
        Err(e) => format!("not declarations: {e}"),
    }
}

async fn command(repl: &mut Repl, cmd: ReplCommand, timeout: Duration) -> Result<CommandResponse> {
    match repl.send(&cmd, timeout).await? {
        ReplResponse::Command(r) => Ok(r),
        ReplResponse::Error(e) => bail!("REPL: {}", e.message),
        ReplResponse::ProofStep(_) => bail!("REPL answered a command with a proof step"),
    }
}

/// The outcome of one tactic on a REPL proof state.
#[derive(Debug, Clone, Serialize)]
pub struct Step {
    pub ok: bool,
    pub proof_state: Option<u64>,
    pub complete: bool,
    pub goals: Vec<String>,
    pub error: Option<String>,
}

pub async fn repl_tactic(repl: &mut Repl, proof_state: u64, tactic: &str) -> Result<Step> {
    let cmd = ReplCommand::Tactic {
        tactic: tactic.to_string(),
        proof_state,
    };
    Ok(match repl.send(&cmd, T_STEP).await? {
        ReplResponse::ProofStep(s) => {
            let error = s
                .messages
                .iter()
                .find(|m| m.severity == Severity::Error)
                .map(|m| m.data.clone());
            Step {
                ok: error.is_none(),
                proof_state: Some(s.proof_state),
                complete: s.proof_status == "Completed",
                goals: s.goals,
                error,
            }
        }
        ReplResponse::Error(e) => Step {
            ok: false,
            proof_state: None,
            complete: false,
            goals: Vec::new(),
            error: Some(e.message),
        },
        ReplResponse::Command(_) => bail!("REPL answered a tactic with a command response"),
    })
}

/// Pickles `proof_state` to `path`, as a session step does.
pub async fn repl_pickle(repl: &mut Repl, proof_state: u64, path: &Path) -> Result<()> {
    let cmd = ReplCommand::PickleProofState {
        pickle_to: path.to_string_lossy().into_owned(),
        proof_state,
    };
    match repl.send(&cmd, T_STEP).await? {
        ReplResponse::Error(e) => bail!("pickle: {}", e.message),
        _ => Ok(()),
    }
}

pub fn errors(r: &CommandResponse) -> String {
    let errors: Vec<&str> = r
        .messages
        .iter()
        .filter(|m| m.severity == Severity::Error)
        .map(|m| m.data.as_str())
        .take(3)
        .collect();
    errors.join(" | ")
}

/// The axiom report in a Beam probe's messages, read by the worker's own
/// parser: they are put in the REPL's response shape first, so whatever the
/// report becomes, both backends are read alike.
pub fn beam_report(messages: &[RunMessage]) -> Option<Vec<ReportedDeclaration>> {
    let messages: Vec<serde_json::Value> = messages
        .iter()
        .map(|m| {
            let severity = match m.severity.as_str() {
                "error" => "error",
                "warning" => "warning",
                _ => "info",
            };
            serde_json::json!({"severity": severity, "pos": {"line": 1, "column": 0}, "data": m.text})
        })
        .collect();
    let response: CommandResponse =
        serde_json::from_value(serde_json::json!({"env": 0, "messages": messages})).ok()?;
    parse_report(&response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documents_put_the_body_under_the_worker_header() {
        let (doc, offset) = document(
            "module\n\npublic import Mathlib.Data.Nat",
            "theorem t : True := trivial",
        );
        let lines: Vec<&str> = doc.split('\n').collect();
        assert_eq!(lines[offset as usize], "theorem t : True := trivial");
        assert!(doc.starts_with("module\n"), "{doc}");
        assert!(doc.contains("meta import Pisa"), "{doc}");
    }
}
