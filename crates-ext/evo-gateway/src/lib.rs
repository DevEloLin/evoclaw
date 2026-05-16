//! evo-gateway — minimal local HTTP daemon. Phase 5.1+5.2+5.3+5.7.
//!
//! Hardened (security review fixes 1..10):
//!   * constant-time bearer compare
//!   * 2 MB request-body cap, 15 s read deadline, 60 s write deadline
//!   * per-process concurrency cap via `Semaphore`
//!   * Origin/Referer enforcement on `POST /chat`
//!   * generic 500 bodies with server-side trace_id
//!   * vault-backed `Redactor` wired into every WebChat task
//!   * CSP / X-Frame-Options / nosniff / no-referrer on the static page

use evo_core::{ConversationRuntime, Memory, Session};
use evo_policy::{BudgetCfg, CostEngine, PolicyConfig, Redactor, Vault};
use evo_providers::Provider;
use evo_tools::{ToolContext, ToolRegistry};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;

pub const INDEX_HTML: &str = include_str!("../static/index.html");
pub const ZH_HTML: &str = include_str!("../static/zh.html");

/// Hard cap on request body bytes. The header is rejected before we allocate.
pub const MAX_BODY_BYTES: usize = 2_000_000;
/// Total deadline for reading the request line + headers + body.
pub const READ_TIMEOUT: Duration = Duration::from_secs(15);
/// Total deadline for streaming the response back.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(60);
/// Time we will wait for a concurrency permit before declaring 503.
pub const PERMIT_TIMEOUT: Duration = Duration::from_secs(1);
/// Default max in-flight connections per gateway process.
pub const DEFAULT_MAX_CONCURRENT: usize = 32;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayConfig {
    pub bind: String,
    pub allowlist: Vec<String>,
    pub workspace: PathBuf,
    pub logs_dir: PathBuf,
    pub memory_dir: PathBuf,
    pub skills_dir: PathBuf,
    pub cost_log: PathBuf,
    /// Path to the on-disk vault (`~/.evoclaw/secrets/vault.json`). Optional —
    /// missing file means an empty redactor (pattern fallback only).
    #[serde(default)]
    pub vault_path: Option<PathBuf>,
    /// Per-process limit on concurrently handled connections.
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,
}

fn default_max_concurrent() -> usize {
    DEFAULT_MAX_CONCURRENT
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
            vault_path: Some(evo.join("secrets").join("vault.json")),
            max_concurrent: DEFAULT_MAX_CONCURRENT,
        }
    }
}

/// Shared, immutable, cheaply cloneable runtime state.
///
/// `Clone` is implemented manually so the bound on `P` only requires
/// `Provider`, not `Provider + Clone` — every `P`-bearing field is already
/// wrapped in `Arc`, which clones unconditionally.
pub struct GatewayState<P: Provider> {
    pub cfg: Arc<GatewayConfig>,
    pub provider: Arc<P>,
    pub registry: Arc<ToolRegistry>,
    pub model: Arc<String>,
    pub identity_summary: Arc<String>,
    pub redactor: Arc<Redactor>,
    pub bind_port: u16,
}

impl<P: Provider> Clone for GatewayState<P> {
    fn clone(&self) -> Self {
        Self {
            cfg: Arc::clone(&self.cfg),
            provider: Arc::clone(&self.provider),
            registry: Arc::clone(&self.registry),
            model: Arc::clone(&self.model),
            identity_summary: Arc::clone(&self.identity_summary),
            redactor: Arc::clone(&self.redactor),
            bind_port: self.bind_port,
        }
    }
}

#[derive(Debug)]
pub struct ParsedRequest {
    pub method: String,
    pub path: String,
    pub auth: Option<String>,
    pub origin: Option<String>,
    pub referer: Option<String>,
    pub body: Vec<u8>,
}

/// Parse failure modes. The handler maps each to a canned HTTP response (or
/// drops the connection silently on transport errors).
#[derive(Debug)]
pub enum ParseError {
    Io(std::io::Error),
    PayloadTooLarge,
    BadRequest,
}

impl From<std::io::Error> for ParseError {
    fn from(e: std::io::Error) -> Self {
        ParseError::Io(e)
    }
}

pub async fn parse_request(stream: &mut TcpStream) -> Result<ParsedRequest, ParseError> {
    // Read until we have the full header section (\r\n\r\n) or hit a sane cap
    // on header size (8 KiB). This stops a slow-loris from feeding us bytes
    // forever; the outer `tokio::time::timeout` provides the wall-clock fence.
    const HEADER_CAP: usize = 8 * 1024;
    let mut buf: Vec<u8> = Vec::with_capacity(2048);
    let head_end: usize;
    let mut chunk = [0u8; 1024];
    loop {
        if let Some(idx) = find_subsequence(&buf, b"\r\n\r\n") {
            head_end = idx;
            break;
        }
        if buf.len() >= HEADER_CAP {
            return Err(ParseError::BadRequest);
        }
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Err(ParseError::BadRequest);
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let head = std::str::from_utf8(&buf[..head_end]).map_err(|_| ParseError::BadRequest)?;
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let path = parts.next().unwrap_or("/").to_string();

    let mut auth = None;
    let mut origin = None;
    let mut referer = None;
    let mut content_length: usize = 0;
    for line in lines {
        if let Some(rest) = strip_header_ci(line, "Authorization:") {
            auth = Some(rest.to_string());
        } else if let Some(rest) = strip_header_ci(line, "Content-Length:") {
            content_length = rest.parse().map_err(|_| ParseError::BadRequest)?;
        } else if let Some(rest) = strip_header_ci(line, "Origin:") {
            origin = Some(rest.to_string());
        } else if let Some(rest) = strip_header_ci(line, "Referer:") {
            referer = Some(rest.to_string());
        }
    }

    if content_length > MAX_BODY_BYTES {
        return Err(ParseError::PayloadTooLarge);
    }

    let body_start = head_end + 4;
    let mut body = if buf.len() > body_start {
        buf[body_start..].to_vec()
    } else {
        Vec::new()
    };
    if body.len() > content_length {
        body.truncate(content_length);
    }
    while body.len() < content_length {
        let want = (content_length - body.len()).min(4096);
        let mut tmp = vec![0u8; want];
        let m = stream.read(&mut tmp).await?;
        if m == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..m]);
        if body.len() > MAX_BODY_BYTES {
            return Err(ParseError::PayloadTooLarge);
        }
    }
    body.truncate(content_length);
    Ok(ParsedRequest {
        method,
        path,
        auth,
        origin,
        referer,
        body,
    })
}

/// Case-insensitive header strip. Returns the trimmed value if `line` starts
/// with `name` (e.g. `"Content-Length:"`).
fn strip_header_ci<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    if line.len() < name.len() {
        return None;
    }
    if !line.as_bytes()[..name.len()].eq_ignore_ascii_case(name.as_bytes()) {
        return None;
    }
    Some(line[name.len()..].trim())
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Constant-time byte-slice equality. Returns `false` on length mismatch
/// without short-circuiting on content. Inline so no extra crate is needed.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

pub fn auth_ok(req: &ParsedRequest, cfg: &GatewayConfig) -> bool {
    let Some(header) = req.auth.as_deref() else {
        return false;
    };
    let Some(token) = header.strip_prefix("Bearer ") else {
        return false;
    };
    let token_bytes = token.as_bytes();
    cfg.allowlist
        .iter()
        .any(|t| ct_eq(t.as_bytes(), token_bytes))
}

/// Allow-list of acceptable `Origin` (or fallback `Referer`) values for
/// browser-initiated `POST /chat`. `null` covers `file://` and curl; the two
/// loopback origins must match the bind port we are actually listening on.
pub fn is_origin_allowed(req: &ParsedRequest, bind_port: u16) -> bool {
    let candidate = req.origin.as_deref().or(req.referer.as_deref());
    let Some(raw) = candidate else {
        // No Origin and no Referer → curl / same-origin server-to-self → allow.
        return true;
    };
    let raw = raw.trim();
    if raw.is_empty() || raw == "null" {
        return true;
    }
    let lo_127 = format!("http://127.0.0.1:{bind_port}");
    let lo_name = format!("http://localhost:{bind_port}");
    if raw == lo_127 || raw == lo_name {
        return true;
    }
    // `Referer` carries a path; match by prefix on the origin portion.
    if raw.starts_with(&format!("{lo_127}/")) || raw.starts_with(&format!("{lo_name}/")) {
        return true;
    }
    false
}

pub async fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    write_response_with_headers(stream, status, content_type, &[], body).await
}

/// Variant that lets the caller inject extra response headers — used by
/// `GET /` to ship CSP / X-Frame-Options / nosniff / Referrer-Policy.
pub async fn write_response_with_headers(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    extra_headers: &[(&str, &str)],
    body: &[u8],
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        413 => "Payload Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "OK",
    };
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in extra_headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
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

#[derive(Debug, Serialize)]
pub struct SessionResp {
    pub provider: String,
    pub model: String,
    pub identity: String,
}

pub async fn handle_chat<P: Provider>(
    provider: Arc<P>,
    registry: Arc<ToolRegistry>,
    cfg: &GatewayConfig,
    redactor: Arc<Redactor>,
    model: &str,
    req: ChatReq,
) -> eyre::Result<ChatResp> {
    let task_id = format!("task-{}", chrono::Utc::now().format("%Y%m%dT%H%M%S%.3f"));
    let log_path = cfg.logs_dir.join(format!("{task_id}.jsonl"));
    let session = Session::open(&log_path).await?;
    let memory = Memory::at(cfg.memory_dir.clone());
    let cost = Arc::new(CostEngine::at(cfg.cost_log.clone(), BudgetCfg::default()));
    let policy_path = cfg
        .workspace
        .parent()
        .map(|d| d.join("policy.toml"))
        .unwrap_or_else(|| cfg.workspace.join("policy.toml"));
    let tool_ctx = ToolContext {
        workspace: cfg.workspace.clone(),
        allow_user_prompt: false,
        policy: Some(Arc::new(PolicyConfig::load(&policy_path).await)),
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
    .with_skills_dir(cfg.skills_dir.clone())
    .with_redactor((*redactor).clone());
    let outcome = runtime.run(&req.input).await?;
    Ok(ChatResp {
        task_id: outcome.task_id,
        turns: outcome.turns,
        final_text: outcome.final_text,
    })
}

/// Bootstrap the redactor from disk. A missing/unreadable vault file is fine —
/// return an empty redactor (pattern fallback still scrubs sk-*, ghp_* etc.).
pub async fn load_redactor(cfg: &GatewayConfig) -> eyre::Result<Redactor> {
    let Some(vault_path) = cfg.vault_path.as_ref() else {
        return Ok(Redactor::default());
    };
    let vault = Vault::load(vault_path).await.unwrap_or_default();
    Ok(Redactor::from_vault(&vault))
}

pub async fn serve<P: Provider + 'static>(
    cfg: GatewayConfig,
    provider: Arc<P>,
    registry: Arc<ToolRegistry>,
    model: String,
    identity_summary: String,
) -> eyre::Result<()> {
    tokio::fs::create_dir_all(&cfg.workspace).await.ok();
    tokio::fs::create_dir_all(&cfg.logs_dir).await.ok();
    tokio::fs::create_dir_all(&cfg.memory_dir).await.ok();
    tokio::fs::create_dir_all(&cfg.skills_dir).await.ok();

    let redactor = Arc::new(load_redactor(&cfg).await?);
    if !redactor.is_empty() {
        tracing::info!(entries = redactor.entry_count(), "vault redactor active");
    }

    let listener = TcpListener::bind(&cfg.bind).await?;
    let bind_port = listener.local_addr().map(|sa| sa.port()).unwrap_or(0);
    tracing::info!(bind = %cfg.bind, port = bind_port, "evo-gateway listening");

    let semaphore = Arc::new(Semaphore::new(cfg.max_concurrent.max(1)));
    let state = GatewayState {
        cfg: Arc::new(cfg),
        provider,
        registry,
        model: Arc::new(model),
        identity_summary: Arc::new(identity_summary),
        redactor,
        bind_port,
    };

    loop {
        let (stream, _) = listener.accept().await?;
        let state = state.clone();
        let sem = semaphore.clone();
        tokio::spawn(async move {
            handle_connection(stream, state, sem).await;
        });
    }
}

async fn handle_connection<P: Provider + 'static>(
    mut stream: TcpStream,
    state: GatewayState<P>,
    semaphore: Arc<Semaphore>,
) {
    // Acquire a permit or shed load. The permit lives for the entire
    // request/response — including writing 503 — so a flood of 503s does not
    // itself starve the pool.
    let permit = match tokio::time::timeout(PERMIT_TIMEOUT, semaphore.acquire_owned()).await {
        Ok(Ok(p)) => p,
        _ => {
            let _ = tokio::time::timeout(
                WRITE_TIMEOUT,
                write_response(
                    &mut stream,
                    503,
                    "application/json",
                    br#"{"error":"service unavailable"}"#,
                ),
            )
            .await;
            return;
        }
    };

    let parse = tokio::time::timeout(READ_TIMEOUT, parse_request(&mut stream)).await;
    let req = match parse {
        Ok(Ok(r)) => r,
        Ok(Err(ParseError::PayloadTooLarge)) => {
            let _ = tokio::time::timeout(
                WRITE_TIMEOUT,
                write_response(
                    &mut stream,
                    413,
                    "application/json",
                    br#"{"error":"payload too large"}"#,
                ),
            )
            .await;
            drop(permit);
            return;
        }
        Ok(Err(ParseError::BadRequest)) => {
            let _ = tokio::time::timeout(
                WRITE_TIMEOUT,
                write_response(
                    &mut stream,
                    400,
                    "application/json",
                    br#"{"error":"bad request"}"#,
                ),
            )
            .await;
            drop(permit);
            return;
        }
        // I/O error or read timeout — drop silently.
        _ => {
            drop(permit);
            return;
        }
    };

    let _ = tokio::time::timeout(WRITE_TIMEOUT, route(&mut stream, req, &state)).await;
    drop(permit);
}

async fn route<P: Provider>(
    stream: &mut TcpStream,
    req: ParsedRequest,
    state: &GatewayState<P>,
) -> std::io::Result<()> {
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") => {
            let security_headers: &[(&str, &str)] = &[
                (
                    "Content-Security-Policy",
                    "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'",
                ),
                ("X-Frame-Options", "DENY"),
                ("X-Content-Type-Options", "nosniff"),
                ("Referrer-Policy", "no-referrer"),
            ];
            write_response_with_headers(
                stream,
                200,
                "text/html; charset=utf-8",
                security_headers,
                INDEX_HTML.as_bytes(),
            )
            .await
        }
        ("GET", "/zh.html") => {
            let security_headers: &[(&str, &str)] = &[
                (
                    "Content-Security-Policy",
                    "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'",
                ),
                ("X-Frame-Options", "DENY"),
                ("X-Content-Type-Options", "nosniff"),
                ("Referrer-Policy", "no-referrer"),
            ];
            write_response_with_headers(
                stream,
                200,
                "text/html; charset=utf-8",
                security_headers,
                ZH_HTML.as_bytes(),
            )
            .await
        }
        ("GET", "/healthz") => write_response(stream, 200, "text/plain", b"ok").await,
        ("GET", "/session") => handle_get_session(stream, req, state).await,
        ("POST", "/chat") => handle_post_chat(stream, req, state).await,
        _ => write_response(stream, 404, "text/plain", b"not found").await,
    }
}

async fn handle_get_session<P: Provider>(
    stream: &mut TcpStream,
    req: ParsedRequest,
    state: &GatewayState<P>,
) -> std::io::Result<()> {
    if !auth_ok(&req, &state.cfg) {
        return write_response(
            stream,
            401,
            "application/json",
            br#"{"error":"unauthorized"}"#,
        )
        .await;
    }
    let resp = SessionResp {
        provider: state
            .identity_summary
            .split(" · ")
            .next()
            .unwrap_or("")
            .into(),
        model: (*state.model).clone(),
        identity: (*state.identity_summary).clone(),
    };
    let json = serde_json::to_vec(&resp).unwrap_or_default();
    write_response(stream, 200, "application/json", &json).await
}

async fn handle_post_chat<P: Provider>(
    stream: &mut TcpStream,
    req: ParsedRequest,
    state: &GatewayState<P>,
) -> std::io::Result<()> {
    if !is_origin_allowed(&req, state.bind_port) {
        return write_response(
            stream,
            403,
            "application/json",
            br#"{"error":"forbidden origin"}"#,
        )
        .await;
    }
    if !auth_ok(&req, &state.cfg) {
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
        Err(_) => {
            return write_response(
                stream,
                400,
                "application/json",
                br#"{"error":"bad request"}"#,
            )
            .await;
        }
    };
    match handle_chat(
        state.provider.clone(),
        state.registry.clone(),
        &state.cfg,
        state.redactor.clone(),
        &state.model,
        body,
    )
    .await
    {
        Ok(resp) => {
            let json = serde_json::to_vec(&resp).unwrap_or_default();
            write_response(stream, 200, "application/json", &json).await
        }
        Err(e) => {
            let trace_id = new_trace_id();
            // Full report goes to the operator only — never the client body.
            tracing::error!(trace_id = %trace_id, error = ?e, "chat handler failed");
            let body = format!(r#"{{"error":"internal error","trace_id":"{trace_id}"}}"#);
            write_response(stream, 500, "application/json", body.as_bytes()).await
        }
    }
}

/// Lightweight trace id: sha256(now_nanos || pid || counter), first 8 hex.
/// Reuses `evo_policy::fingerprint_of` (already available transitively) so
/// the gateway pulls in no extra dependency.
fn new_trace_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let now = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
    let pid = std::process::id();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    evo_policy::fingerprint_of(&format!("{now}-{pid}-{n}"))
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
            vault_path: None,
            max_concurrent: DEFAULT_MAX_CONCURRENT,
        }
    }

    fn req_with(auth: Option<&str>, origin: Option<&str>, referer: Option<&str>) -> ParsedRequest {
        ParsedRequest {
            method: "POST".into(),
            path: "/chat".into(),
            auth: auth.map(String::from),
            origin: origin.map(String::from),
            referer: referer.map(String::from),
            body: Vec::new(),
        }
    }

    #[test]
    fn auth_ok_accepts_listed_token() {
        assert!(auth_ok(&req_with(Some("Bearer dev"), None, None), &cfg()));
        assert!(auth_ok(
            &req_with(Some("Bearer alice-pair-123"), None, None),
            &cfg()
        ));
    }

    #[test]
    fn auth_rejects_missing_or_unknown() {
        assert!(!auth_ok(&req_with(None, None, None), &cfg()));
        assert!(!auth_ok(
            &req_with(Some("Bearer wrong"), None, None),
            &cfg()
        ));
        assert!(!auth_ok(&req_with(Some("dev"), None, None), &cfg()));
    }

    #[test]
    fn ct_eq_is_length_safe() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"abcd"));
        assert!(!ct_eq(b"abcd", b"abc"));
        assert!(ct_eq(b"", b""));
    }

    #[test]
    fn html_index_is_present() {
        assert!(INDEX_HTML.contains("EvoClaw"));
    }

    #[test]
    fn origin_allows_loopback_and_null() {
        let port = 7878u16;
        assert!(is_origin_allowed(&req_with(None, Some("null"), None), port));
        assert!(is_origin_allowed(
            &req_with(None, Some("http://127.0.0.1:7878"), None),
            port
        ));
        assert!(is_origin_allowed(
            &req_with(None, Some("http://localhost:7878"), None),
            port
        ));
        // No Origin / no Referer → same origin → allowed.
        assert!(is_origin_allowed(&req_with(None, None, None), port));
    }

    #[test]
    fn origin_rejects_other_hosts() {
        assert!(!is_origin_allowed(
            &req_with(None, Some("http://evil.example.com"), None),
            7878
        ));
        assert!(!is_origin_allowed(
            &req_with(None, Some("http://127.0.0.1:9000"), None),
            7878
        ));
    }

    #[test]
    fn origin_falls_back_to_referer_when_origin_absent() {
        assert!(is_origin_allowed(
            &req_with(None, None, Some("http://localhost:7878/chat")),
            7878
        ));
        assert!(!is_origin_allowed(
            &req_with(None, None, Some("http://other.example/")),
            7878
        ));
    }

    #[test]
    fn trace_ids_are_unique_and_short() {
        let a = new_trace_id();
        let b = new_trace_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 8);
    }

    #[test]
    fn header_strip_is_case_insensitive() {
        assert_eq!(
            strip_header_ci("Content-Length: 7", "Content-Length:"),
            Some("7")
        );
        assert_eq!(
            strip_header_ci("content-length: 7", "Content-Length:"),
            Some("7")
        );
        assert_eq!(strip_header_ci("ORIGIN: null", "Origin:"), Some("null"));
        assert!(strip_header_ci("Authorization: Bearer x", "Origin:").is_none());
    }
}
