//! Configuration structs, path helpers, and layout/load functions.

use directories::BaseDirs;
use evo_providers::AuthMethod;
use eyre::{Result, WrapErr};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// Structs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Config {
    #[serde(default)]
    pub(crate) meta: ProfileMeta,
    pub(crate) model: ModelCfg,
    #[serde(default)]
    pub(crate) auth: AuthCfg,
    pub(crate) budget: ConfigBudget,
    pub(crate) security: SecurityCfg,
    /// Optional logging override. Older config.toml files without this
    /// section keep working — `logs_dir()` falls back to the platform
    /// temp dir (`/tmp/evoclaw` on Unix, `%TEMP%\\evoclaw` on Windows).
    #[serde(default)]
    pub(crate) logs: Option<LogsCfg>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct ProfileMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct LogsCfg {
    /// Directory where session JSONL logs are written. Tilde (`~`) is
    /// expanded against `$HOME`. Missing directories are created on demand.
    #[serde(default)]
    pub(crate) dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct AuthCfg {
    /// Selected auth method: `api_key` (default) | `browser` | `acp`.
    /// Old config.toml files without an `[auth]` block decode to default ⇒
    /// `api_key`, preserving backward compatibility with existing installs.
    #[serde(default = "default_auth_method")]
    pub(crate) method: String,
}

fn default_auth_method() -> String {
    AuthMethod::ApiKey.as_str().to_string()
}

impl AuthCfg {
    pub(crate) fn parsed(&self) -> AuthMethod {
        AuthMethod::parse(&self.method).unwrap_or(AuthMethod::ApiKey)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ModelCfg {
    /// Provider id from the catalog (`deepseek`, `kimi`, ...). When present,
    /// drives api-key resolution. Older configs without this field still work
    /// — `evoclaw login` adds it.
    #[serde(default)]
    pub(crate) provider: Option<String>,
    pub(crate) default: String,
    pub(crate) base_url: String,
    #[serde(default)]
    pub(crate) fallback: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ConfigBudget {
    pub(crate) per_task_usd: f64,
    pub(crate) per_day_usd: f64,
    pub(crate) per_month_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SecurityCfg {
    pub(crate) default_permission: String,
    pub(crate) high_risk_intercept: bool,
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

pub(crate) fn home() -> Result<PathBuf> {
    Ok(BaseDirs::new()
        .ok_or_else(|| eyre::eyre!("cannot determine home dir"))?
        .home_dir()
        .to_path_buf())
}

pub(crate) fn evoclaw_dir() -> Result<PathBuf> {
    Ok(home()?.join(".evoclaw"))
}

pub(crate) fn profiles_dir() -> Result<PathBuf> {
    Ok(evoclaw_dir()?.join("profiles"))
}

pub(crate) fn active_profile_file() -> Result<PathBuf> {
    Ok(evoclaw_dir()?.join("active-profile.txt"))
}

pub(crate) fn config_path() -> Result<PathBuf> {
    Ok(evoclaw_dir()?.join("config.toml"))
}

pub(crate) fn workspace_dir() -> Result<PathBuf> {
    Ok(evoclaw_dir()?.join("workspace"))
}

pub(crate) fn skills_dir() -> Result<PathBuf> {
    Ok(evoclaw_dir()?.join("skills"))
}

pub(crate) fn playbooks_dir() -> Result<PathBuf> {
    Ok(evoclaw_dir()?.join("playbooks"))
}

pub(crate) fn memory_dir() -> Result<PathBuf> {
    Ok(evoclaw_dir()?.join("memory"))
}

pub(crate) fn cost_log_path() -> Result<PathBuf> {
    Ok(evoclaw_dir()?.join("cost.jsonl"))
}

pub(crate) fn vault_path() -> Result<PathBuf> {
    Ok(evo_policy::default_vault_path(&evoclaw_dir()?))
}

pub(crate) fn policy_path() -> Result<PathBuf> {
    Ok(evoclaw_dir()?.join("policy.toml"))
}

// ---------------------------------------------------------------------------
// Logs directory management
// ---------------------------------------------------------------------------

/// Resolution order, evaluated once per process (first call wins):
///   1. env `EVO_LOG_DIR`        — operator override
///   2. config.toml `[logs] dir` — user override
///   3. platform default         — `/tmp/evoclaw` on Unix, `%TEMP%\evoclaw` on Windows
///
/// Initialised by `init_logs_dir(...)` from the entry point. Calling
/// `logs_dir()` before initialisation falls through to the platform
/// default — safe but ignores any `[logs]` block in config.toml.
static LOGS_DIR: OnceLock<PathBuf> = OnceLock::new();

pub(crate) fn compute_logs_dir(cfg: Option<&Config>) -> PathBuf {
    if let Ok(v) = std::env::var("EVO_LOG_DIR") {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            return expand_tilde(trimmed);
        }
    }
    if let Some(c) = cfg {
        if let Some(LogsCfg { dir: Some(d) }) = &c.logs {
            let trimmed = d.trim();
            if !trimmed.is_empty() {
                return expand_tilde(trimmed);
            }
        }
    }
    if cfg!(windows) {
        std::env::temp_dir().join("evoclaw")
    } else {
        PathBuf::from("/tmp/evoclaw")
    }
}

pub(crate) fn expand_tilde(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Ok(h) = std::env::var("HOME") {
            return PathBuf::from(h).join(rest);
        }
    }
    PathBuf::from(raw)
}

pub(crate) fn init_logs_dir(cfg: Option<&Config>) {
    let _ = LOGS_DIR.set(compute_logs_dir(cfg));
}

pub(crate) fn logs_dir() -> Result<PathBuf> {
    Ok(LOGS_DIR
        .get()
        .cloned()
        .unwrap_or_else(|| compute_logs_dir(None)))
}

/// One log file per shell session. Inside `interactive()` we compute this
/// once on entry and pass it down to every `run_task_with_provider` call,
/// so all `Task`/`Turn`/`End` records from the same window land in the
/// same JSONL file (instead of one file per `evoclaw>` ask).
pub(crate) fn session_log_path() -> Result<PathBuf> {
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S");
    Ok(logs_dir()?.join(format!("session-{stamp}.jsonl")))
}

// ---------------------------------------------------------------------------
// Layout + config load
// ---------------------------------------------------------------------------

pub(crate) async fn ensure_layout() -> Result<()> {
    // Logs no longer live under `~/.evoclaw/`. The destination is decided
    // by `logs_dir()` (env / config / platform default) and created on
    // demand by `Session::open`, so we don't touch it here.
    for sub in [
        "workspace",
        "skills",
        "playbooks",
        "browser_profiles",
        "secrets",
        "plugins",
        "cache",
        "mcp",
        "channels",
        "agents",
        "memory",
        "gateway",
    ] {
        tokio::fs::create_dir_all(evoclaw_dir()?.join(sub))
            .await
            .wrap_err_with(|| format!("create {sub}"))?;
    }

    // Harden browser_profiles/ — session data must not be world-readable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let bp = evoclaw_dir()?.join("browser_profiles");
        if bp.exists() {
            let _ = tokio::fs::set_permissions(
                &bp,
                std::fs::Permissions::from_mode(0o700),
            )
            .await;
        }
    }

    // Drop a ready-to-edit credentials.toml on first run if absent.
    let creds_path = evoclaw_dir()?.join("secrets").join("credentials.toml");
    if !creds_path.exists() {
        tokio::fs::write(&creds_path, CREDENTIALS_TEMPLATE)
            .await
            .wrap_err("create credentials.toml")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(
                &creds_path,
                std::fs::Permissions::from_mode(0o600),
            )
            .await
            .wrap_err("chmod credentials.toml")?;
        }
    }

    // Drop a ready-to-edit policy.toml on first run if absent.
    let policy_path = evoclaw_dir()?.join("policy.toml");
    if !policy_path.exists() {
        tokio::fs::write(&policy_path, POLICY_TEMPLATE)
            .await
            .wrap_err("create policy.toml")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(
                &policy_path,
                std::fs::Permissions::from_mode(0o600),
            )
            .await
            .wrap_err("chmod policy.toml")?;
        }
    }

    Ok(())
}

const CREDENTIALS_TEMPLATE: &str = r#"# EvoClaw credentials — edit with any text editor.
# This file is chmod 600; values never leave this machine.
#
# Group credentials by account using [section] headers.
# The section name becomes the key prefix in placeholders:
#   [google_work]  ->  ${SECRET:google_work_user}, ${SECRET:google_work_pass}
#                      ${TOTP:google_work_totp}
#
# Examples:
#
# [google_work]
# user = "work@company.com"
# pass = "yourpassword"
# totp = "BASE32SEED"        # optional — remove if site has no TOTP
#
# [hsbc_main]
# user = "myuser"
# pass = "mypass"
#
# Flat (top-level) keys are also supported:
#   my_api_key = "sk-abc123"   ->  ${SECRET:my_api_key}
"#;

const POLICY_TEMPLATE: &str = r#"# EvoClaw policy — edit with any text editor (chmod 600).
#
# deny rules are always checked first and take precedence over allow rules.
# Pattern syntax: * matches any sequence of characters, ? matches one character.
# Tool keys: "bash", "write_file", "read_file", "patch_file", "web_fetch", "*" (all tools).
#
# Examples:
#
# [deny]
# bash = [
#   "* .ssh*",          # block any bash command touching .ssh
#   "rm -rf *",         # block destructive deletes
#   "* gpg *",          # block GPG operations
# ]
# write_file = [
#   "~/.ssh/**",        # block writes to SSH directory
#   "/etc/**",          # block writes to system directories
# ]
#
# [allow]
# # Restrict bash to only these commands (uncomment to enable whitelist mode):
# # bash = ["cargo *", "git *", "ls *", "cat *"]
#
# # Pre-execution hooks — shell commands run before each tool invocation.
# # The hook receives {"tool": "...", "args": {...}} on stdin.
# # Exit 0 = allow, exit 2 = block (stdout shown as reason), other = see on_fail.
#
# [[hooks.pre_exec]]
# tool = "bash"                                    # or "*" for all tools
# command = "python3 ~/.evoclaw/hooks/check.py"
# on_fail = "block"                                # "block" (default) or "warn"

[deny]
bash = [
    "* .ssh*",
    "*~/.ssh*",
]
"#;

pub(crate) async fn load_config() -> Result<Config> {
    let p = config_path()?;
    let text = tokio::fs::read_to_string(&p).await.wrap_err_with(|| {
        format!(
            "read config at {}; run `evoclaw onboard` first",
            p.display()
        )
    })?;
    toml::from_str(&text).wrap_err("parse config.toml")
}
