//! Router↔worker protocol: length-prefixed JSON frames over TCP.
//!
//! Workers dial the router, announce themselves with `Hello`, then receive
//! `Job`s and send back `Result`s tagged with the job's request ID. Results may
//! arrive in any order, and the TCP connection doubles as the health check.

use std::collections::BTreeMap;

use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

use crate::api;
use crate::repl::CommandResponse;
use crate::verify::Verification;

pub const MAX_FRAME: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkerMessage {
    Hello {
        worker_id: String,
        version: String,
        slots: u32,
    },
    /// Header keys this worker has warm. Sent after every change.
    Status {
        warm_headers: Vec<String>,
    },
    Result {
        request_id: u64,
        result: JobResult,
    },
    /// A Lean process was replaced; `cause` is e.g. "uses", "memory", "timeout", "crash".
    Restart {
        cause: String,
    },
    /// The worker is shutting down: send it no new jobs. It finishes the ones
    /// it has, then disconnects.
    Draining,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RouterMessage {
    Job {
        request_id: u64,
        job: Job,
    },
    /// Header texts to import ahead of demand.
    Prewarm {
        headers: Vec<String>,
    },
}

/// A frame from the router, as a worker reads it.
#[derive(Debug)]
pub enum Inbound {
    Message(RouterMessage),
    /// A job whose `job` doesn't decode, e.g. one in another release's format.
    /// The worker answers it with [`JobResult::Failed`]: dropping the
    /// connection instead would make the router retry it, and every other job
    /// on the connection, elsewhere.
    BadJob {
        request_id: u64,
        error: String,
    },
}

/// A frame is an error only if it is neither a job with a `request_id` nor a
/// [`RouterMessage`].
impl<'de> Deserialize<'de> for Inbound {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        use serde_json::Value;
        let mut frame = Value::deserialize(d)?;
        let is_job = frame.get("type").and_then(Value::as_str) == Some("job");
        if is_job && let Some(request_id) = frame.get("request_id").and_then(Value::as_u64) {
            let job = frame.get_mut("job").map(Value::take);
            return Ok(match serde_json::from_value(job.unwrap_or_default()) {
                Ok(job) => Inbound::Message(RouterMessage::Job { request_id, job }),
                Err(e) => Inbound::BadJob {
                    request_id,
                    error: e.to_string(),
                },
            });
        }
        serde_json::from_value(frame)
            .map(Inbound::Message)
            .map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Job {
    Check(CheckJob),
    SessionStart(SessionStartJob),
    SessionTactic(SessionTacticJob),
    SessionCheck(SessionCheckJob),
    SnapshotCreate(SnapshotCreateJob),
    SnapshotGet(SnapshotJob),
    SnapshotDelete(SnapshotJob),
    SnapshotVerify(SnapshotJob),
    SnapshotList(SnapshotListJob),
    SnapshotLabels(SnapshotLabelsJob),
    /// A job kind this worker doesn't know, sent by a newer router. It is
    /// answered with [`JobResult::Failed`] rather than dropping the connection.
    #[serde(other)]
    Unsupported,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckJob {
    pub code: String,
    pub timeout_secs: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detail: Vec<api::Detail>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStartJob {
    pub code: String,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTacticJob {
    pub state: String,
    pub tactic: String,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCheckJob {
    pub session: String,
    pub code: String,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotCreateJob {
    pub code: String,
    pub timeout_secs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify: Option<bool>,
    /// The new entry's labels.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    /// The request's idempotency key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotJob {
    /// The entry id. Routers of the release before entries called it
    /// `snapshot` and sent a module name, which is then a `bad_request`.
    #[serde(alias = "snapshot")]
    pub entry: String,
}

/// `GET /snapshots`, its query parsed and checked by the router.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotListJob {
    /// `(key, value)` pairs that an entry must all have, in request order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<(String, String)>,
    pub limit: usize,
    /// The cursor to start after.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotLabelsJob {
    /// The entry id.
    pub entry: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub set: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JobResult {
    Check(CheckOutcome),
    SessionStarted(Result<api::StartResponse, api::Error>),
    TacticApplied(Result<api::TacticResponse, api::Error>),
    SessionChecked(SessionChecked),
    /// Failures are in-band, in `result.error`, as for checks.
    SnapshotCreated(Box<api::SnapshotResponse>),
    Snapshot(Result<Box<api::SnapshotInfo>, api::Error>),
    SnapshotDeleted(Result<api::Deleted, api::Error>),
    SnapshotVerified(Result<api::SnapshotVerified, api::Error>),
    SnapshotList(Result<api::SnapshotList, api::Error>),
    SnapshotLabels(Result<api::SnapshotSummary, api::Error>),
    /// The worker could not run the job at all.
    Failed {
        error: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckOutcome {
    pub time: f64,
    #[serde(default)]
    pub error: Option<api::Error>,
    /// The REPL's response, positions already shifted to the submitted code.
    #[serde(default)]
    pub response: Option<CommandResponse>,
    #[serde(default)]
    pub verification: Option<Verification>,
    /// Whether the header was already imported in the process that ran the job.
    pub warm: bool,
}

impl From<CheckOutcome> for api::CheckResult {
    fn from(o: CheckOutcome) -> Self {
        let (messages, sorries, tactics) = o.response.map_or_else(Default::default, |r| {
            let sorries = r.sorries.into_iter().map(Into::into).collect();
            let tactics = r.tactics.into_iter().map(Into::into).collect();
            (api::messages(r.messages), sorries, tactics)
        });
        let (verified, reasons, declarations) =
            o.verification.map_or((false, Vec::new(), Vec::new()), |v| {
                (v.ok, v.reasons, v.declarations)
            });
        api::CheckResult {
            id: None,
            ok: verified && o.error.is_none(),
            time: o.time,
            error: o.error,
            messages,
            sorries,
            reasons,
            declarations,
            tactics,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionChecked {
    pub outcome: CheckOutcome,
    /// Reasons the completed code does not prove the session's statements.
    pub statement_reasons: Vec<String>,
}

pub type Connection<T> = Framed<T, LengthDelimitedCodec>;

pub fn connection<T: AsyncRead + AsyncWrite>(io: T) -> Connection<T> {
    LengthDelimitedCodec::builder()
        .max_frame_length(MAX_FRAME)
        .new_framed(io)
}

#[derive(Debug, thiserror::Error)]
pub enum WireError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("bad frame: {0}")]
    Json(#[from] serde_json::Error),
}

pub async fn send<S, T>(sink: &mut S, msg: &T) -> Result<(), WireError>
where
    S: futures::Sink<bytes::Bytes, Error = std::io::Error> + Unpin,
    T: Serialize,
{
    let bytes = serde_json::to_vec(msg)?;
    sink.send(bytes.into()).await?;
    Ok(())
}

/// Reads the next message, or `None` when the peer closed the connection.
pub async fn recv<S, T>(stream: &mut S) -> Result<Option<T>, WireError>
where
    S: futures::Stream<Item = Result<bytes::BytesMut, std::io::Error>> + Unpin,
    T: DeserializeOwned,
{
    match stream.next().await {
        None => Ok(None),
        Some(frame) => Ok(Some(serde_json::from_slice(&frame?)?)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn messages_round_trip_over_a_connection() {
        let (a, b) = tokio::io::duplex(1 << 16);
        let mut a = connection(a);
        let mut b = connection(b);
        let job = RouterMessage::Job {
            request_id: 9,
            job: Job::Check(CheckJob {
                code: "import Mathlib\n".repeat(200),
                timeout_secs: 5,
                detail: Vec::new(),
            }),
        };
        send(&mut a, &job).await.unwrap();
        let got: RouterMessage = recv(&mut b).await.unwrap().unwrap();
        let RouterMessage::Job {
            request_id,
            job: Job::Check(c),
        } = got
        else {
            panic!()
        };
        assert_eq!(request_id, 9);
        assert_eq!(c.code.len(), 15 * 200);
        drop(a);
        assert!(recv::<_, RouterMessage>(&mut b).await.unwrap().is_none());
    }

    #[test]
    fn unknown_job_kinds_parse_as_unsupported() {
        let msg: RouterMessage = serde_json::from_value(serde_json::json!({
            "type": "job", "request_id": 1,
            "job": {"kind": "from_the_future", "code": "x", "timeout_secs": 1}
        }))
        .unwrap();
        assert!(matches!(
            msg,
            RouterMessage::Job {
                job: Job::Unsupported,
                ..
            }
        ));
    }

    #[test]
    fn a_job_that_does_not_decode_keeps_its_request_id() {
        let decode = serde_json::from_value::<Inbound>;
        let bad = decode(serde_json::json!({
            "type": "job", "request_id": 4, "job": {"kind": "check", "code": 5}
        }));
        assert!(
            matches!(bad, Ok(Inbound::BadJob { request_id: 4, .. })),
            "{bad:?}"
        );

        // A job from a router of the release before entries.
        let old = decode(serde_json::json!({
            "type": "job", "request_id": 5,
            "job": {"kind": "snapshot_get", "snapshot": "PisaSnap.s0"}
        }));
        let Ok(Inbound::Message(RouterMessage::Job {
            request_id: 5,
            job: Job::SnapshotGet(job),
        })) = old
        else {
            panic!("{old:?}")
        };
        assert_eq!(job.entry, "PisaSnap.s0");

        let prewarm = decode(serde_json::json!({"type": "prewarm", "headers": ["import A"]}));
        assert!(matches!(
            prewarm,
            Ok(Inbound::Message(RouterMessage::Prewarm { .. }))
        ));
        for unusable in [
            serde_json::json!({"type": "job", "job": {"kind": "check"}}),
            serde_json::json!({"type": "job", "request_id": "x", "job": {}}),
            serde_json::json!({"type": "from_the_future"}),
        ] {
            assert!(decode(unusable.clone()).is_err(), "{unusable}");
        }
    }
}
