//! evo-gateway — minimal local HTTP daemon. Phase 5.1+5.2+5.3+5.7.

use evo_core::{ConversationRuntime, Memory, Session};
use evo_policy::{BudgetCfg, CostEngine};
use evo_providers::Provider;
use evo_tools::{ToolContext, ToolRegistry};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub const INDEX_HTML: &str = include_str!("../static/index.html");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayConfig {
    pub bind: String,
    pub allowlist: Vec<String>,
    pub workspace: PathBuf,
    pub logs_dir: PathBuf,
    pub memory_dir: PathBuf,
    pub skills_dir: PathBuf,
    pub cost_log: PathBuf,
}

impl GatewayConfig {
    pub fn local_default(home: &std::path::Path) -> Self {
        let evo = home.join(".evoclaw");
        Self {
            bind: "127.0.0.1:7878".into(),
            allowlist: vec!["dev".into()],
            workspace: evo.join("workspace"),
            logs_dir: evo.join("logs"),
            memory_dir: evo.join("memory"),
            skills_dir: evo.join("skills"),
            cost_log: evo.join("cost.jsonl"),
        }
    }
}

#[derive(Debug)]
pub struct ParsedRequest {
    pub method: String,
    pub path: String,
    pub auth: Option<String>,
    pub body: Vec<u8>,
}

pub async fn parse_request(stream: &mut TcpStream) -> std::io::Result<ParsedRequest> {
    let mut buf = vec![0u8; 8192];
    let n = stream.read(&mut buf).await?;
    buf.truncate(n);
    let head_end = find_subsequence(&buf, b"\r\n\r\n").unwrap_or(buf.len());
    let head = std::str::from_utf8(&buf[..head_end]).unwrap_or("");
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let path = parts.next().unwrap_or("/").to_string();
    let mut auth = None;
    let mut content_length = 0usize;
    for line in lines {
        if let Some(rest) = line.strip_prefix("Authorization: ") {
            auth = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("Content-Length: ") {
            content_length = rest.trim().parse().unwrap_or(0);
        }
    }
    let mut body = if buf.len() > head_end + 4 {
        buf[head_end + 4..].to_vec()
    } else {
        Vec::new()
    };
    while body.len() < content_length {
        let mut chunk = vec![0u8; 4096];
        let m = stream.read(&mut chunk).await?;
        if m == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..m]);
    }
    body.truncate(content_length);
    Ok(ParsedRequest {
        method,
        path,
        auth,
        body,
    })
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

pub fn auth_ok(req: &ParsedRequest, cfg: &GatewayConfig) -> bool {
    let Some(header) = req.auth.as_deref() else {
        return false;
    };
    let Some(token) = header.strip_prefix("Bearer ") else {
        return false;
    };
    cfg.allowlist.iter().any(|t| t == token)
}

pub async fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
        _ => "OK",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await
}

#[derive(Debug, Deserialize)]
pub struct ChatReq {
    pub input: String,
}

#[derive(Debug, Serialize)]
pub struct ChatResp {
    pub task_id: String,
    pub turns: u64,
    pub final_text: String,
}

pub async fn handle_chat<P: Provider>(
    provider: Arc<P>,
    registry: Arc<ToolRegistry>,
    cfg: &GatewayConfig,
    model: &str,
    req: ChatReq,
) -> eyre::Result<ChatResp> {
    let task_id = format!("task-{}", chrono::Utc::now().format("%Y%m%dT%H%M%S%.3f"));
    let log_path = cfg.logs_dir.join(format!("{task_id}.jsonl"));
    let session = Session::open(&log_path).await?;
    let memory = Memory::at(cfg.memory_dir.clone());
    let cost = Arc::new(CostEngine::at(cfg.cost_log.clone(), BudgetCfg::default()));
    let tool_ctx = ToolContext {
        workspace: cfg.workspace.clone(),
        allow_user_prompt: false,
        ..Default::default()
    };
    let mut runtime = ConversationRuntime::new(
        provider,
        registry,
        session,
        tool_ctx,
        evo_core::runtime::RuntimeConfig {
            model: model.into(),
            ..Default::default()
        },
    )
    .with_cost_engine(cost)
    .with_memory(memory)
    .with_skills_dir(cfg.skills_dir.clone());
    let outcome = runtime.run(&req.input).await?;
    Ok(ChatResp {
        task_id: outcome.task_id,
        turns: outcome.turns,
        final_text: outcome.final_text,
    })
}

pub async fn serve<P: Provider + 'static>(
    cfg: GatewayConfig,
    provider: Arc<P>,
    registry: Arc<ToolRegistry>,
    model: String,
) -> eyre::Result<()> {
    tokio::fs::create_dir_all(&cfg.workspace).await.ok();
    tokio::fs::create_dir_all(&cfg.logs_dir).await.ok();
    tokio::fs::create_dir_all(&cfg.memory_dir).await.ok();
    tokio::fs::create_dir_all(&cfg.skills_dir).await.ok();
    let listener = TcpListener::bind(&cfg.bind).await?;
    tracing::info!(bind = %cfg.bind, "evo-gateway listening");
    let cfg = Arc::new(cfg);
    let model = Arc::new(model);
    loop {
        let (mut stream, _) = listener.accept().await?;
        let provider = provider.clone();
        let registry = registry.clone();
        let cfg = cfg.clone();
        let model = model.clone();
        tokio::spawn(async move {
            let req = match parse_request(&mut stream).await {
                Ok(r) => r,
                Err(_) => return,
            };
            let _ = route(&mut stream, req, &cfg, provider, registry, &model).await;
        });
    }
}

async fn route<P: Provider>(
    stream: &mut TcpStream,
    req: ParsedRequest,
    cfg: &GatewayConfig,
    provider: Arc<P>,
    registry: Arc<ToolRegistry>,
    model: &str,
) -> std::io::Result<()> {
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") => {
            write_response(
                stream,
                200,
                "text/html; charset=utf-8",
                INDEX_HTML.as_bytes(),
            )
            .await
        }
        ("GET", "/healthz") => write_response(stream, 200, "text/plain", b"ok").await,
        ("POST", "/chat") => {
            if !auth_ok(&req, cfg) {
                return write_response(
                    stream,
                    401,
                    "application/json",
                    br#"{"error":"unauthorized"}"#,
                )
                .await;
            }
            let body: ChatReq = match serde_json::from_slice(&req.body) {
                Ok(b) => b,
                Err(e) => {
                    let msg = format!("{{\"error\":\"bad request: {e}\"}}");
                    return write_response(stream, 400, "application/json", msg.as_bytes()).await;
                }
            };
            match handle_chat(provider, registry, cfg, model, body).await {
                Ok(resp) => {
                    let json = serde_json::to_vec(&resp).unwrap_or_default();
                    write_response(stream, 200, "application/json", &json).await
                }
                Err(e) => {
                    let msg = format!("{{\"error\":\"{e}\"}}");
                    write_response(stream, 500, "application/json", msg.as_bytes()).await
                }
            }
        }
        _ => write_response(stream, 404, "text/plain", b"not found").await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> GatewayConfig {
        GatewayConfig {
            bind: "127.0.0.1:0".into(),
            allowlist: vec!["dev".into(), "alice-pair-123".into()],
            workspace: "/tmp/x".into(),
            logs_dir: "/tmp/x".into(),
            memory_dir: "/tmp/x".into(),
            skills_dir: "/tmp/x".into(),
            cost_log: "/tmp/x".into(),
        }
    }

    fn req_with_auth(auth: Option<&str>) -> ParsedRequest {
        ParsedRequest {
            method: "POST".into(),
            path: "/chat".into(),
            auth: auth.map(String::from),
            body: Vec::new(),
        }
    }

    #[test]
    fn auth_ok_accepts_listed_token() {
        assert!(auth_ok(&req_with_auth(Some("Bearer dev")), &cfg()));
        assert!(auth_ok(
            &req_with_auth(Some("Bearer alice-pair-123")),
            &cfg()
        ));
    }

    #[test]
    fn auth_rejects_missing_or_unknown() {
        assert!(!auth_ok(&req_with_auth(None), &cfg()));
        assert!(!auth_ok(&req_with_auth(Some("Bearer wrong")), &cfg()));
        assert!(!auth_ok(&req_with_auth(Some("dev")), &cfg()));
    }

    #[test]
    fn html_index_is_present() {
        assert!(INDEX_HTML.contains("EvoClaw"));
    }
}
