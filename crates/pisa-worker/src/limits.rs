//! Lean's runtime limits, which Beam reports as a failed tool call.

use pisa_protocol::api::{Error, ErrorKind, LeanLimit};

/// Lean's default `maxRecDepth`.
const DEFAULT_REC_DEPTH: u64 = 512;

/// The error for Beam's failure `message` if it is Lean reaching its
/// recursion or heartbeat limit, `None` otherwise. `code_text` is what ran, to
/// find the `maxRecDepth` it set.
pub fn lean_limit(code: &str, message: &str, code_text: &str) -> Option<Error> {
    if code == "workerExited" {
        return None;
    }
    if message.contains("maximum recursion depth has been reached") {
        let value = last_option(code_text, "maxRecDepth").unwrap_or(DEFAULT_REC_DEPTH);
        return Some(limited(
            ErrorKind::RecursionLimit,
            "maxRecDepth",
            value,
            None,
            message,
        ));
    }
    let (_, rest) = message.split_once("maximum number of heartbeats (")?;
    let value = rest.split_once(')')?.0.parse().ok()?;
    let phase = message
        .split_once("timeout at `")
        .and_then(|(_, rest)| rest.split_once('`'))
        .map(|(phase, _)| phase.to_string());
    Some(limited(
        ErrorKind::HeartbeatLimit,
        "maxHeartbeats",
        value,
        phase,
        message,
    ))
}

fn limited(
    kind: ErrorKind,
    option: &str,
    value: u64,
    phase: Option<String>,
    message: &str,
) -> Error {
    Error {
        limit: Some(LeanLimit {
            option: option.to_string(),
            value,
            phase,
        }),
        ..Error::new(kind, message)
    }
}

/// The error for a process the memory limit killed. `what` is the process
/// as the message names it, `cap` its slot's setting and value in MiB (see
/// [`crate::launcher::Launcher::memory_max_mb`]), which the error names.
pub fn out_of_memory(what: &str, cap: Option<(&'static str, u64)>) -> Error {
    let message = format!("{what} exceeded its memory limit{}", cap_suffix(cap));
    Error {
        limit: cap.map(|(option, value)| LeanLimit {
            option: option.to_string(),
            value,
            phase: None,
        }),
        ..Error::new(ErrorKind::OutOfMemory, message)
    }
}

/// ` (PISA_MEMORY_MAX_MB = 16000 MiB)`, or nothing without a limit.
pub fn cap_suffix(cap: Option<(&'static str, u64)>) -> String {
    cap.map_or_else(String::new, |(setting, mb)| {
        format!(" ({setting} = {mb} MiB)")
    })
}

/// The value of the last `set_option <option> N` in `text`.
fn last_option(text: &str, option: &str) -> Option<u64> {
    let mut found = None;
    let mut words = text.split_whitespace();
    while let Some(word) = words.next() {
        if word == "set_option"
            && words.next() == Some(option)
            && let Some(n) = words.next().and_then(|n| n.parse().ok())
        {
            found = Some(n);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_out_of_memory_error_names_its_cap() {
        let repl = out_of_memory("Lean", Some(("PISA_MEMORY_MAX_MB", 16000)));
        assert_eq!(repl.kind, ErrorKind::OutOfMemory);
        assert_eq!(
            repl.message,
            "Lean exceeded its memory limit (PISA_MEMORY_MAX_MB = 16000 MiB)"
        );
        let limit = repl.limit.unwrap();
        assert_eq!(
            (limit.option.as_str(), limit.value),
            ("PISA_MEMORY_MAX_MB", 16000)
        );
        let beam = out_of_memory("Lean Beam", Some(("PISA_BEAM_MEMORY_MAX_MB", 8000)));
        assert_eq!(beam.limit.unwrap().option, "PISA_BEAM_MEMORY_MAX_MB");
        assert!(out_of_memory("Lean", None).limit.is_none());
    }

    /// `maxRecDepthErrorMessage` in `Init/Prelude.lean`.
    const RECURSION: &str = "maximum recursion depth has been reached\n\
        use `set_option maxRecDepth <num>` to increase limit\n\
        use `set_option diagnostics true` to get diagnostic information";
    /// `throwMaxHeartbeat` in `Lean/CoreM.lean`, for 200000 heartbeats.
    const HEARTBEAT: &str = "(deterministic) timeout at `whnf`, maximum number of heartbeats \
        (200000) has been reached\nUse `set_option maxHeartbeats <num>` to set the limit.\n\
        Additional diagnostic information may be available using the \
        `set_option diagnostics true` command.";

    fn limit(error: Error) -> (ErrorKind, LeanLimit) {
        (error.kind, error.limit.expect("a limit"))
    }

    #[test]
    fn recursion_defaults_to_lean_s_512() {
        let error = lean_limit("internalError", RECURSION, "simp").unwrap();
        assert_eq!(error.message, RECURSION);
        let (kind, limit) = limit(error);
        assert_eq!(kind, ErrorKind::RecursionLimit);
        assert_eq!(limit.option, "maxRecDepth");
        assert_eq!(limit.value, 512);
        assert_eq!(limit.phase, None);
    }

    #[test]
    fn recursion_takes_the_last_option_set_in_the_code() {
        let text = "set_option maxRecDepth 100 in\nexample := 1\nset_option maxRecDepth 2000\nsimp";
        let (_, limit) = limit(lean_limit("internalError", RECURSION, text).unwrap());
        assert_eq!(limit.value, 2000);
    }

    #[test]
    fn heartbeats_name_the_phase_and_value() {
        let (kind, limit) = limit(lean_limit("internalError", HEARTBEAT, "").unwrap());
        assert_eq!(kind, ErrorKind::HeartbeatLimit);
        assert_eq!(limit.option, "maxHeartbeats");
        assert_eq!(limit.value, 200000);
        assert_eq!(limit.phase.as_deref(), Some("whnf"));
    }

    #[test]
    fn other_failures_are_not_limits() {
        assert!(lean_limit("internalError", "unknown constant", "").is_none());
        assert!(lean_limit("internalError", "timeout at the door", "").is_none());
    }

    #[test]
    fn a_worker_exit_stays_unavailable() {
        assert!(lean_limit("workerExited", RECURSION, "").is_none());
    }
}
