//! Client side of the daemon's local channel, used by `nanace-mcp`, the
//! desktop app's shell and the `nanace-p2p call` command. It carries no
//! networking code, so depending on it does not pull libp2p in.

use std::path::Path;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, Lines, ReadHalf, WriteHalf};

#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    /// The daemon is not running, or the socket is not reachable.
    #[error("the nanace P2P daemon is not reachable: {0}")]
    Unreachable(String),
    /// The daemon answered with an error. `reason` is set when a rule
    /// refused the call (`policy`, `pattern`, …).
    #[error("{message}")]
    Rpc {
        code: i64,
        message: String,
        reason: Option<String>,
    },
}

trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

/// One authenticated session with the daemon.
pub struct IpcClient {
    lines: Lines<BufReader<ReadHalf<Box<dyn Stream>>>>,
    writer: WriteHalf<Box<dyn Stream>>,
    next_id: u64,
    /// Notices that arrived while waiting for a response.
    notices: Vec<Value>,
}

#[cfg(unix)]
async fn open(socket: &Path) -> std::io::Result<Box<dyn Stream>> {
    Ok(Box::new(tokio::net::UnixStream::connect(socket).await?))
}

#[cfg(windows)]
async fn open(socket: &Path) -> std::io::Result<Box<dyn Stream>> {
    Ok(Box::new(
        tokio::net::windows::named_pipe::ClientOptions::new().open(socket)?,
    ))
}

impl IpcClient {
    /// Connects and authenticates. `agent_label` registers the agent this
    /// session speaks for; the desktop app passes `None`.
    pub async fn connect(
        socket: &Path,
        secret: &str,
        agent_label: Option<&str>,
        capabilities: &[String],
    ) -> Result<Self, IpcError> {
        let stream = open(socket).await.map_err(|e| IpcError::Unreachable(e.to_string()))?;
        let (reader, writer) = tokio::io::split(stream);
        let mut client = Self {
            lines: BufReader::new(reader).lines(),
            writer,
            next_id: 0,
            notices: Vec::new(),
        };
        client
            .call(
                "hello",
                json!({ "secret": secret, "agent_label": agent_label, "capabilities": capabilities }),
            )
            .await?;
        Ok(client)
    }

    /// Calls a method and waits for its result.
    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value, IpcError> {
        self.next_id += 1;
        let id = self.next_id;
        let request = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        self.writer
            .write_all(format!("{request}\n").as_bytes())
            .await
            .map_err(|e| IpcError::Unreachable(e.to_string()))?;
        loop {
            let message = self.read().await?;
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                if message.get("method").and_then(Value::as_str) == Some("notice") {
                    self.notices.push(message["params"].clone());
                }
                continue;
            }
            if let Some(err) = message.get("error") {
                return Err(IpcError::Rpc {
                    code: err["code"].as_i64().unwrap_or_default(),
                    message: err["message"].as_str().unwrap_or("error").to_owned(),
                    reason: err["data"]["reason"].as_str().map(str::to_owned),
                });
            }
            return Ok(message["result"].clone());
        }
    }

    /// The next notice from the daemon (desktop app only).
    pub async fn next_notice(&mut self) -> Result<Value, IpcError> {
        if !self.notices.is_empty() {
            return Ok(self.notices.remove(0));
        }
        loop {
            let message = self.read().await?;
            if message.get("method").and_then(Value::as_str) == Some("notice") {
                return Ok(message["params"].clone());
            }
        }
    }

    async fn read(&mut self) -> Result<Value, IpcError> {
        let line = self
            .lines
            .next_line()
            .await
            .map_err(|e| IpcError::Unreachable(e.to_string()))?
            .ok_or_else(|| IpcError::Unreachable("the daemon closed the connection".to_owned()))?;
        serde_json::from_str(&line).map_err(|e| IpcError::Unreachable(e.to_string()))
    }
}
