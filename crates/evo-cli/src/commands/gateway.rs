//! Gateway daemon command and token management helpers.

use crate::config::{ensure_layout, evoclaw_dir};
use eyre::Result;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

/// Path to the persistent gateway token. We co-locate it with the rest of
/// `~/.evoclaw` runtime state, in its own subdirectory so the file's chmod
/// 600 is meaningful (not shared with public artefacts).
pub(crate) fn gateway_token_path() -> Result<PathBuf> {
    Ok(evoclaw_dir()?.join("gateway").join("token"))
}

// ---------------------------------------------------------------------------
// Token management
// ---------------------------------------------------------------------------

/// SHA-256 fingerprint of the bearer token, first 8 hex chars. Safe to log /
/// print: an attacker cannot recover the token from this — but operators can
/// confirm the same value is in use across processes.
pub(crate) fn token_fingerprint(s: &str) -> String {
    evo_policy::fingerprint_of(s)
}

/// Resolve the bearer token to use for `evo gateway`:
///   * `--token <T>` provided   → use as-is (operator override)
///   * persisted file present   → read it, validate, reuse
///   * neither                  → generate a fresh 32-hex random token,
///     write it (mode 0600), print it ONCE.
pub(crate) async fn resolve_gateway_token(cli_override: Option<&str>) -> Result<(String, bool)> {
    if let Some(t) = cli_override {
        let t = t.trim();
        if t.is_empty() {
            return Err(eyre::eyre!("--token may not be empty"));
        }
        return Ok((t.to_string(), false));
    }
    let path = gateway_token_path()?;
    if let Ok(raw) = tokio::fs::read_to_string(&path).await {
        let trimmed = raw.trim().to_string();
        if !trimmed.is_empty() && trimmed.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Ok((trimmed, false));
        }
        // Corrupt or empty file — fall through and regenerate.
    }
    let fresh = generate_token_hex(16).await?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&path, &fresh).await?;
    set_file_mode_600(&path).await.ok();
    Ok((fresh, true))
}

/// Read `n_bytes` bytes from `/dev/urandom` (Unix) and hex-encode them. On
/// non-Unix platforms, fall back to a SHA-256 of high-entropy process state.
pub(crate) async fn generate_token_hex(n_bytes: usize) -> Result<String> {
    #[cfg(unix)]
    {
        use tokio::io::AsyncReadExt;
        if let Ok(mut f) = tokio::fs::File::open("/dev/urandom").await {
            let mut buf = vec![0u8; n_bytes];
            f.read_exact(&mut buf).await?;
            return Ok(bytes_to_hex(&buf));
        }
    }
    let _ = n_bytes; // signature consistency on non-Unix fallback
    let now = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
    let pid = std::process::id();
    let stack_addr = &now as *const _ as usize;
    let env_hash: usize = std::env::vars()
        .map(|(k, v)| k.len().wrapping_mul(31).wrapping_add(v.len()))
        .fold(0usize, |a, b| a.wrapping_add(b));
    let seed = format!("{now}-{pid}-{stack_addr}-{env_hash}");
    let a = evo_policy::fingerprint_of(&format!("{seed}-A"));
    let b = evo_policy::fingerprint_of(&format!("{seed}-B"));
    let c = evo_policy::fingerprint_of(&format!("{seed}-C"));
    let d = evo_policy::fingerprint_of(&format!("{seed}-D"));
    Ok(format!("{a}{b}{c}{d}"))
}

/// Inline lower-case hex encoder — avoids pulling the `hex` crate in as a new
/// dep of `evo-cli`.
pub(crate) fn bytes_to_hex(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(TABLE[(b >> 4) as usize] as char);
        out.push(TABLE[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(unix)]
pub(crate) async fn set_file_mode_600(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o600);
    tokio::fs::set_permissions(path, perms).await
}

#[cfg(not(unix))]
pub(crate) async fn set_file_mode_600(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

// ---------------------------------------------------------------------------
// gateway command
// ---------------------------------------------------------------------------

pub(crate) async fn gateway(bind: &str, token_arg: Option<&str>) -> Result<()> {
    use std::process::Stdio;

    ensure_layout().await?;
    let (token, freshly_generated) = resolve_gateway_token(token_arg).await?;
    let fp = token_fingerprint(&token);
    let token_path = gateway_token_path()?;

    let mut cmd = tokio::process::Command::new("evo-gateway");
    cmd.env("EVO_GATEWAY_BIND", bind)
        .env("EVO_GATEWAY_ALLOWLIST", &token)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let mut child = cmd.spawn().map_err(|e| {
        eyre::eyre!(
            "evo-gateway binary not found on PATH: {e}. Build with `cargo build -p evo-gateway`."
        )
    })?;
    // Never echo the raw token after this point.
    println!("→ evo-gateway started, bound to {bind} (token fingerprint: {fp})");
    println!("  WebChat: http://{bind}");
    if freshly_generated {
        println!();
        println!("  ╔══════════════════════════════════════════════════════════════╗");
        println!("  ║  A NEW gateway token has been generated and saved to disk.   ║");
        println!("  ║  Save this — it WILL NOT be shown again.                     ║");
        println!("  ║                                                              ║");
        println!("  ║    token: {token:<50}║");
        println!("  ║    file : {:<50}║", token_path.display());
        println!("  ║    chmod: 0600 (owner read/write only)                       ║");
        println!("  ╚══════════════════════════════════════════════════════════════╝");
        println!();
    } else if token_arg.is_none() {
        println!("  (token loaded from {})", token_path.display());
    }
    let status = child.wait().await?;
    if !status.success() {
        return Err(eyre::eyre!("evo-gateway exited: {status}"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod gateway_tests {
    use super::*;

    #[test]
    fn token_fingerprint_is_8_hex() {
        let fp = token_fingerprint("hello");
        assert_eq!(fp.len(), 8);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn token_fingerprint_is_stable_and_distinguishes_inputs() {
        assert_eq!(token_fingerprint("same"), token_fingerprint("same"));
        assert_ne!(token_fingerprint("a"), token_fingerprint("b"));
    }

    #[test]
    fn bytes_to_hex_matches_known_vectors() {
        assert_eq!(bytes_to_hex(&[]), "");
        assert_eq!(bytes_to_hex(&[0x00, 0xff]), "00ff");
        assert_eq!(bytes_to_hex(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
    }

    #[tokio::test]
    async fn generate_token_hex_returns_32_hex_chars() {
        let t = generate_token_hex(16).await.expect("generate");
        assert_eq!(t.len(), 32);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
