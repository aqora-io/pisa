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
