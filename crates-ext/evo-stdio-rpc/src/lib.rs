//! evo-stdio-rpc — minimal JSON-RPC 2.0 client over a spawned child's stdio.
//! Shared by evo-acp-client (Zed ACP) and evo-mcp-client (Anthropic MCP).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    #[error("io: {0}")] Io(#[from] std::io::Error),
    #[error("decode: {0}")] Decode(String),
    #[error("rpc {code}: {message}")] Rpc { code: i64, message: String },
    #[error("subprocess exited unexpectedly")] Exited,
    #[error("command not found: {0}")] CommandNotFound(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct JsonRpcRequest<'a> {
    pub jsonrpc: &'a str,
    pub id: u64,
    pub method: &'a str,
    pub params: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct JsonRpcNotification<'a> {
    pub jsonrpc: &'a str,
    pub method: &'a str,
    pub params: Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcResponse {
    #[serde(default)] pub id: Option<u64>,
    #[serde(default)] pub result: Option<Value>,
    #[serde(default)] pub error: Option<JsonRpcErrorBody>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcErrorBody { pub code: i64, pub message: String }

#[derive(Debug, Clone)]
pub struct SpawnConfig {
    pub command: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: Option<PathBuf>,
}

pub struct StdioRpcClient {
    inner: Arc<Mutex<Inner>>,
    next_id: Arc<AtomicU64>,
}

struct Inner {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    reader: Option<BufReader<ChildStdout>>,
}

impl StdioRpcClient {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner { child: None, stdin: None, reader: None })),
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    pub async fn spawn(&self, cfg: SpawnConfig) -> Result<(), RpcError> {
        let mut g = self.inner.lock().await;
        if g.child.is_some() { return Ok(()); }
        let mut cmd = Command::new(&cfg.command);
        cmd.args(&cfg.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            // Tokio does not kill children on drop by default; without this
            // flag we'd leak zombie processes whenever a registry / Arc<Client>
            // is dropped without an explicit shutdown(). Belt-and-braces:
            // shutdown() still calls start_kill+wait when invoked directly.
            .kill_on_drop(true);
        for (k, v) in &cfg.env { cmd.env(k, v); }
        if let Some(d) = &cfg.cwd { cmd.current_dir(d); }
        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                RpcError::CommandNotFound(cfg.command.to_string_lossy().into_owned())
            } else { RpcError::Io(e) }
        })?;
        let stdin = child.stdin.take().ok_or_else(|| RpcError::Decode("no stdin".into()))?;
        let stdout = child.stdout.take().ok_or_else(|| RpcError::Decode("no stdout".into()))?;
        g.child = Some(child);
        g.stdin = Some(stdin);
        g.reader = Some(BufReader::new(stdout));
        Ok(())
    }

    pub fn next_id(&self) -> u64 { self.next_id.fetch_add(1, Ordering::SeqCst) }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let id = self.next_id();
        let req = JsonRpcRequest { jsonrpc: "2.0", id, method, params };
        let mut line = serde_json::to_string(&req).map_err(|e| RpcError::Decode(e.to_string()))?;
        line.push('\n');
        let mut g = self.inner.lock().await;
        let stdin = g.stdin.as_mut().ok_or(RpcError::Exited)?;
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await?;
        loop {
            let reader = g.reader.as_mut().ok_or(RpcError::Exited)?;
            let mut buf = String::new();
            let n = reader.read_line(&mut buf).await?;
            if n == 0 { return Err(RpcError::Exited); }
            let resp: JsonRpcResponse = serde_json::from_str(&buf)
                .map_err(|e| RpcError::Decode(format!("response: {e}; line: {buf}")))?;
            if resp.id != Some(id) { continue; } // skip notifications / older ids
            if let Some(err) = resp.error {
                return Err(RpcError::Rpc { code: err.code, message: err.message });
            }
            return Ok(resp.result.unwrap_or(Value::Null));
        }
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<(), RpcError> {
        let n = JsonRpcNotification { jsonrpc: "2.0", method, params };
        let mut line = serde_json::to_string(&n).map_err(|e| RpcError::Decode(e.to_string()))?;
        line.push('\n');
        let mut g = self.inner.lock().await;
        let stdin = g.stdin.as_mut().ok_or(RpcError::Exited)?;
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await?;
        Ok(())
    }

    pub async fn shutdown(&self) -> Result<(), RpcError> {
        let mut g = self.inner.lock().await;
        if let Some(mut child) = g.child.take() {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
        g.stdin = None;
        g.reader = None;
        Ok(())
    }
}

impl Default for StdioRpcClient {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn req_serialises_with_id() {
        let r = JsonRpcRequest { jsonrpc: "2.0", id: 7, method: "ping", params: json!({}) };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"id\":7"));
        assert!(s.contains("\"method\":\"ping\""));
    }

    #[test]
    fn notification_has_no_id() {
        let n = JsonRpcNotification { jsonrpc: "2.0", method: "log", params: json!({"x":1}) };
        let s = serde_json::to_string(&n).unwrap();
        assert!(!s.contains("\"id\""));
    }

    #[test]
    fn response_parses_result() {
        let s = r#"{"jsonrpc":"2.0","id":1,"result":{"a":1}}"#;
        let r: JsonRpcResponse = serde_json::from_str(s).unwrap();
        assert!(r.result.is_some() && r.error.is_none());
    }

    #[test]
    fn response_parses_error() {
        let s = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"unknown"}}"#;
        let r: JsonRpcResponse = serde_json::from_str(s).unwrap();
        assert_eq!(r.error.unwrap().code, -32601);
    }

    #[tokio::test]
    async fn spawning_missing_command_returns_clear_error() {
        let cli = StdioRpcClient::new();
        let cfg = SpawnConfig {
            command: PathBuf::from("/definitely/not/a/real/path/zzzz"),
            args: vec![], env: vec![], cwd: None,
        };
        let err = cli.spawn(cfg).await.unwrap_err();
        assert!(matches!(err, RpcError::CommandNotFound(_)));
    }
}
