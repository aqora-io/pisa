//! Drives one Lean REPL child process over stdin/stdout.
//!
//! The REPL dies on malformed input and keeps every environment it creates, so
//! a [`Repl`] is treated as disposable: any timeout, crash or protocol error
//! kills it, and the owner replaces it.

use std::collections::VecDeque;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::{Buf, BytesMut};
use futures::StreamExt;
use pisa_protocol::repl::{ReplCommand, ReplResponse};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio_util::codec::{Decoder, FramedRead};

pub mod proc;

const STDERR_TAIL: usize = 20;

#[derive(Debug, thiserror::Error)]
pub enum ReplFailure {
    #[error("timed out")]
    Timeout,
    #[error("process exited ({status}): {stderr}")]
    Exited { status: String, stderr: String },
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("process is not running")]
    Dead,
}

pub struct Repl {
    child: Child,
    stdin: ChildStdin,
    stdout: FramedRead<ChildStdout, ResponseCodec>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    dead: bool,
}

impl Repl {
    /// Spawns `cmd` with piped stdio. The child is killed when the `Repl` drops.
    pub fn spawn(mut cmd: Command) -> std::io::Result<Self> {
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = FramedRead::new(child.stdout.take().expect("piped stdout"), ResponseCodec);
        let stderr = child.stderr.take().expect("piped stderr");
        let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));
        let tail = stderr_tail.clone();
        let pid = child.id();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(?pid, "repl stderr: {line}");
                let mut tail = tail.lock().unwrap();
                if tail.len() == STDERR_TAIL {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
        });
        Ok(Repl {
            child,
            stdin,
            stdout,
            stderr_tail,
            dead: false,
        })
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    pub fn is_alive(&self) -> bool {
        !self.dead
    }

    /// Sends one command and waits up to `timeout` for its response.
    pub async fn send(
        &mut self,
        cmd: &ReplCommand,
        timeout: Duration,
    ) -> Result<ReplResponse, ReplFailure> {
        if self.dead {
            return Err(ReplFailure::Dead);
        }
        // Dead until the response is read: if this is dropped before, the
        // process's next response would be this command's.
        self.dead = true;
        let result = tokio::time::timeout(timeout, self.exchange(cmd)).await;
        let failure = match result {
            Ok(Ok(response)) => {
                self.dead = false;
                return Ok(response);
            }
            Ok(Err(e)) => e,
            Err(_) => ReplFailure::Timeout,
        };
        self.kill().await;
        Err(match failure {
            // A broken pipe means the process died; report how.
            ReplFailure::Io(_) => self.exit_failure().await,
            other => other,
        })
    }

    async fn exchange(&mut self, cmd: &ReplCommand) -> Result<ReplResponse, ReplFailure> {
        // serde_json escapes newlines in strings, so this is a single line, and
        // exactly one blank line terminates it. A second blank line would kill the REPL.
        let mut line = serde_json::to_vec(cmd).map_err(|e| ReplFailure::Protocol(e.to_string()))?;
        line.extend_from_slice(b"\n\n");
        self.stdin.write_all(&line).await?;
        self.stdin.flush().await?;
        match self.stdout.next().await {
            Some(frame) => {
                let frame = frame?;
                serde_json::from_slice(&frame).map_err(|e| {
                    ReplFailure::Protocol(format!("{e}: {}", String::from_utf8_lossy(&frame)))
                })
            }
            None => Err(self.exit_failure().await),
        }
    }

    async fn exit_failure(&mut self) -> ReplFailure {
        let status = match tokio::time::timeout(Duration::from_secs(1), self.child.wait()).await {
            Ok(Ok(s)) => s.to_string(),
            _ => "unknown".into(),
        };
        // Give the stderr reader a moment to drain the last lines.
        tokio::time::sleep(Duration::from_millis(20)).await;
        let stderr = self
            .stderr_tail
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        ReplFailure::Exited { status, stderr }
    }

    pub async fn kill(&mut self) {
        self.dead = true;
        let _ = self.child.kill().await;
    }
}

/// Splits REPL stdout into responses: pretty-printed JSON values separated by
/// blank lines. A response never contains a blank line itself.
pub struct ResponseCodec;

impl Decoder for ResponseCodec {
    type Item = BytesMut;
    type Error = std::io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<BytesMut>, std::io::Error> {
        let leading = src.iter().take_while(|b| b.is_ascii_whitespace()).count();
        src.advance(leading);
        match src.windows(2).position(|w| w == b"\n\n") {
            Some(end) => {
                let frame = src.split_to(end);
                src.advance(2);
                Ok(Some(frame))
            }
            None => Ok(None),
        }
    }

    fn decode_eof(&mut self, src: &mut BytesMut) -> Result<Option<BytesMut>, std::io::Error> {
        match self.decode(src)? {
            Some(frame) => Ok(Some(frame)),
            None if src.is_empty() => Ok(None),
            None => Ok(Some(src.split())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_splits_pretty_printed_responses() {
        let mut buf =
            BytesMut::from(&b"{\"env\": 0}\n\n{\"messages\":\n  [],\n \"env\": 1}\n\n{\"env\""[..]);
        let mut c = ResponseCodec;
        assert_eq!(&c.decode(&mut buf).unwrap().unwrap()[..], b"{\"env\": 0}");
        assert_eq!(
            &c.decode(&mut buf).unwrap().unwrap()[..],
            b"{\"messages\":\n  [],\n \"env\": 1}"
        );
        assert!(c.decode(&mut buf).unwrap().is_none());
        assert_eq!(&buf[..], b"{\"env\"");
    }

    fn sh(script: &str) -> Command {
        let mut c = Command::new("sh");
        c.arg("-c").arg(script);
        c
    }

    #[tokio::test]
    async fn round_trips_a_command() {
        let mut repl = Repl::spawn(sh(
            "read l; read b; printf '{\"env\":\\n 0}\\n\\n'; sleep 5",
        ))
        .unwrap();
        let r = repl
            .send(
                &ReplCommand::cmd("theorem x : 1 = 1 := rfl", None),
                Duration::from_secs(5),
            )
            .await
            .unwrap();
        assert!(matches!(r, ReplResponse::Command(c) if c.env == 0));
        assert!(repl.is_alive());
    }

    /// A send dropped before its response, as when the job it serves is
    /// cancelled, leaves the process dead: its next response would be the
    /// dropped command's.
    #[tokio::test]
    async fn a_dropped_send_leaves_the_process_dead() {
        let mut repl = Repl::spawn(sh("sleep 10")).unwrap();
        let cmd = ReplCommand::cmd("x", None);
        let send = repl.send(&cmd, Duration::from_secs(10));
        assert!(
            tokio::time::timeout(Duration::from_millis(100), send)
                .await
                .is_err()
        );
        assert!(!repl.is_alive());
    }

    #[tokio::test]
    async fn timeout_kills_the_process() {
        let mut repl = Repl::spawn(sh("sleep 10")).unwrap();
        let err = repl
            .send(&ReplCommand::cmd("x", None), Duration::from_millis(100))
            .await
            .unwrap_err();
        assert!(matches!(err, ReplFailure::Timeout));
        assert!(!repl.is_alive());
        assert!(matches!(
            repl.send(&ReplCommand::cmd("x", None), Duration::from_secs(1))
                .await,
            Err(ReplFailure::Dead)
        ));
    }

    #[tokio::test]
    async fn exit_reports_stderr() {
        let mut repl = Repl::spawn(sh("read l; echo boom >&2; exit 3")).unwrap();
        let err = repl
            .send(&ReplCommand::cmd("x", None), Duration::from_secs(5))
            .await
            .unwrap_err();
        match err {
            ReplFailure::Exited { status, stderr } => {
                assert!(status.contains('3'), "{status}");
                assert!(stderr.contains("boom"), "{stderr}");
            }
            other => panic!("{other:?}"),
        }
    }
}
