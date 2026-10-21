//! Router↔worker protocol: length-prefixed JSON frames over TCP.
//!
//! Workers dial the router, announce themselves with `Hello`, then receive
//! `Job`s and send back `Result`s tagged with the job's request ID. Results may
//! arrive in any order, and the TCP connection doubles as the health check.

use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

use crate::kimina::CheckOutput;
use crate::session;
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Job {
    Check(CheckJob),
    SessionStart(SessionStartJob),
    SessionTactic(SessionTacticJob),
    SessionCheck(SessionCheckJob),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckJob {
    pub code: String,
    pub timeout_secs: u64,
    #[serde(default)]
    pub infotree: Option<String>,
    /// When false, the Lean process is discarded after this job.
    pub reuse: bool,
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
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JobResult {
    Check(CheckOutcome),
    SessionStarted(session::StartResponse),
    TacticApplied(session::TacticResponse),
    SessionChecked(SessionChecked),
    /// The worker could not run the job at all.
    Failed {
        error: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckOutcome {
    pub time: f64,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub response: Option<CheckOutput>,
    #[serde(default)]
    pub verification: Option<Verification>,
    /// Whether the header was already imported in the process that ran the job.
    pub warm: bool,
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
                infotree: None,
                reuse: true,
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
}
