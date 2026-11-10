//! The MCP connection: one `lean-beam-mcp` child process, requests matched to
//! replies by id so several can be in flight, and tool errors sorted into
//! [`BeamError`]s.

use std::collections::{HashMap, VecDeque};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::oneshot;
use tokio_util::codec::{FramedRead, LinesCodec};

/// The MCP revision asked for in `initialize`; Beam also speaks 2026-07-28.
const PROTOCOL: &str = "2025-11-25";
/// Beam's own broker frames are at most 16 MiB, and so is one line here.
const MAX_LINE: usize = 16 * 1024 * 1024;
const STDERR_TAIL: usize = 20;

#[derive(Debug, thiserror::Error)]
pub enum BeamError {
    #[error("timed out")]
    Timeout,
    #[error("process exited: {stderr}")]
    Exited { stderr: String },
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("process is not running")]
    Dead,
    /// A JSON-RPC error that isn't one of the kinds below.
    #[error("JSON-RPC error {code}: {message}")]
    Rpc { code: i64, message: String },
    /// The server refused the request without running it.
    #[error("server busy: {0}")]
    Busy(String),
    /// The document changed since the version the request named.
    #[error("stale document: {0}")]
    Stale(String),
    /// The tool ran and reported an error (`isError`).
    #[error("{tool} failed ({code}): {message}")]
    Tool {
        tool: String,
        code: String,
        message: String,
    },
}

type Reply = Result<Value, BeamError>;
type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Reply>>>>;

/// One `lean-beam-mcp` process. Requests take `&self`, so callers may run
/// several at once; Beam runs them concurrently up to its own limit.
pub struct Beam {
    child: tokio::sync::Mutex<Child>,
    stdin: tokio::sync::Mutex<ChildStdin>,
    pending: Pending,
    next_id: AtomicU64,
    dead: Arc<AtomicBool>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    pid: Option<u32>,
    tools: Vec<String>,
    tools_list: Value,
}

impl Beam {
    /// Spawns `cmd` with piped stdio, initializes the MCP session and lists
    /// the tools, each step within `timeout`. The child is killed when the
    /// `Beam` drops.
    pub async fn spawn(mut cmd: Command, timeout: Duration) -> Result<Beam, BeamError> {
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let pid = child.id();
        let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));
        let tail = stderr_tail.clone();
        let stderr_done = tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(?pid, "lean-beam-mcp stderr: {line}");
                let mut tail = tail.lock().unwrap();
                if tail.len() == STDERR_TAIL {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
        });
        let pending: Pending = Arc::default();
        let dead = Arc::new(AtomicBool::new(false));
        tokio::spawn(read_replies(
            stdout,
            pending.clone(),
            dead.clone(),
            (stderr_tail.clone(), stderr_done),
            pid,
        ));
        let mut beam = Beam {
            child: tokio::sync::Mutex::new(child),
            stdin: tokio::sync::Mutex::new(stdin),
            pending,
            next_id: AtomicU64::new(0),
            dead,
            stderr_tail,
            pid,
            tools: Vec::new(),
            tools_list: Value::Null,
        };
        let client = json!({"name": "pisa", "version": env!("CARGO_PKG_VERSION")});
        let init = json!({"protocolVersion": PROTOCOL, "capabilities": {}, "clientInfo": client});
        beam.request("initialize", init, timeout).await?;
        beam.notify("notifications/initialized", None).await?;
        let list = beam.request("tools/list", json!({}), timeout).await?;
        beam.tools = list["tools"]
            .as_array()
            .ok_or_else(|| BeamError::Protocol("tools/list without tools".into()))?
            .iter()
            .filter_map(|t| t["name"].as_str().map(str::to_string))
            .collect();
        beam.tools_list = list;
        Ok(beam)
    }

    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    pub fn is_alive(&self) -> bool {
        !self.dead.load(Ordering::SeqCst)
    }

    /// The tool names the server listed.
    pub fn tools(&self) -> &[String] {
        &self.tools
    }

    /// The server's `tools/list` answer, schemas included.
    pub fn tools_list(&self) -> &Value {
        &self.tools_list
    }

    pub fn stderr_tail(&self) -> String {
        let tail = self.stderr_tail.lock().unwrap();
        tail.iter().cloned().collect::<Vec<_>>().join("\n")
    }

    /// Calls `tool` with `args` and returns its structured result. On a
    /// timeout the request is cancelled and the process kept: a cancelled MCP
    /// request gets no reply, so whether Lean stopped can't be told from here,
    /// and the caller decides whether to [`kill`](Self::kill).
    pub async fn call(&self, tool: &str, args: Value, timeout: Duration) -> Reply {
        let params = json!({"name": tool, "arguments": args});
        let result = self.request("tools/call", params, timeout).await?;
        let content = match result.get("structuredContent") {
            Some(content) => content.clone(),
            None => text_content(&result),
        };
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            let code = content["code"].as_str().unwrap_or_default().to_string();
            let message = content["message"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| content.to_string());
            return Err(match code.as_str() {
                "contentModified" | "documentVersionMismatch" => BeamError::Stale(message),
                _ if is_busy(&message) => BeamError::Busy(message),
                _ => BeamError::Tool {
                    tool: tool.to_string(),
                    code,
                    message,
                },
            });
        }
        Ok(content)
    }

    /// Kills the process; requests still in flight fail with [`BeamError::Dead`].
    pub async fn kill(&self) {
        self.dead.store(true, Ordering::SeqCst);
        let mut child = self.child.lock().await;
        let _ = child.start_kill();
        let _ = child.wait().await;
        for (_, tx) in self.pending.lock().unwrap().drain() {
            let _ = tx.send(Err(BeamError::Dead));
        }
    }

    async fn request(&self, method: &str, params: Value, timeout: Duration) -> Reply {
        if !self.is_alive() {
            return Err(BeamError::Dead);
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        if let Err(e) = self.write(&msg).await {
            self.pending.lock().unwrap().remove(&id);
            return Err(self.io_failure(e));
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(reply)) => reply,
            // The reader dropped the sender: the process is gone.
            Ok(Err(_)) => Err(self.exited()),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                let cancel = json!({"requestId": id, "reason": "timed out"});
                let _ = self.notify("notifications/cancelled", Some(cancel)).await;
                Err(BeamError::Timeout)
            }
        }
    }

    async fn notify(&self, method: &str, params: Option<Value>) -> Result<(), BeamError> {
        let mut msg = json!({"jsonrpc": "2.0", "method": method});
        if let Some(params) = params {
            msg["params"] = params;
        }
        self.write(&msg).await.map_err(|e| self.io_failure(e))
    }

    async fn write(&self, msg: &Value) -> std::io::Result<()> {
        // serde_json escapes newlines in strings, so this is a single line.
        let mut line = serde_json::to_vec(msg)?;
        line.push(b'\n');
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(&line).await?;
        stdin.flush().await
    }

    fn io_failure(&self, e: std::io::Error) -> BeamError {
        if self.is_alive() {
            BeamError::Io(e)
        } else {
            self.exited()
        }
    }

    fn exited(&self) -> BeamError {
        BeamError::Exited {
            stderr: self.stderr_tail(),
        }
    }
}

/// Routes each reply to the request waiting for it, until stdout closes;
/// then fails whatever is still waiting, with the last lines of stderr.
async fn read_replies(
    stdout: ChildStdout,
    pending: Pending,
    dead: Arc<AtomicBool>,
    (tail, stderr_done): (Arc<Mutex<VecDeque<String>>>, tokio::task::JoinHandle<()>),
    pid: Option<u32>,
) {
    let mut lines = FramedRead::new(stdout, LinesCodec::new_with_max_length(MAX_LINE));
    while let Some(line) = lines.next().await {
        let line = match line {
            Ok(line) => line,
            Err(e) => {
                tracing::warn!(?pid, "lean-beam-mcp output: {e}");
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(msg) => msg,
            Err(e) => {
                tracing::warn!(?pid, "lean-beam-mcp sent a line that isn't JSON: {e}");
                continue;
            }
        };
        let reply_id = msg
            .get("method")
            .is_none()
            .then(|| msg["id"].as_u64())
            .flatten();
        let Some(id) = reply_id else {
            // A notification, or a request from the server, which pisa doesn't serve.
            tracing::debug!(?pid, "lean-beam-mcp: {}", short(&line));
            continue;
        };
        match pending.lock().unwrap().remove(&id) {
            Some(tx) => {
                let _ = tx.send(reply(&msg));
            }
            None => tracing::debug!(?pid, id, "reply to a request no longer waited for"),
        }
    }
    dead.store(true, Ordering::SeqCst);
    // What the process printed as it died may still be in the pipe.
    let _ = tokio::time::timeout(Duration::from_millis(500), stderr_done).await;
    let stderr = tail.lock().unwrap().iter().cloned().collect::<Vec<_>>();
    for (_, tx) in pending.lock().unwrap().drain() {
        let _ = tx.send(Err(BeamError::Exited {
            stderr: stderr.join("\n"),
        }));
    }
}

fn reply(msg: &Value) -> Reply {
    match msg.get("error") {
        Some(error) => {
            let message = error["message"].as_str().unwrap_or_default().to_string();
            if is_busy(&message) {
                Err(BeamError::Busy(message))
            } else {
                Err(BeamError::Rpc {
                    code: error["code"].as_i64().unwrap_or_default(),
                    message,
                })
            }
        }
        None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
    }
}

/// A result without `structuredContent`: its first text block, as JSON if it is.
fn text_content(result: &Value) -> Value {
    let text = result["content"][0]["text"].as_str().unwrap_or_default();
    serde_json::from_str(text).unwrap_or_else(|_| json!({"message": text}))
}

fn is_busy(message: &str) -> bool {
    message.contains("is busy")
}

fn short(line: &str) -> &str {
    match line.char_indices().nth(300) {
        Some((i, _)) => &line[..i],
        None => line,
    }
}
