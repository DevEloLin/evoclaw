//! Pre-execution hook runner.
//!
//! Each hook is a shell command that receives the tool name and its arguments
//! as JSON on stdin.  Exit codes:
//!   0  → proceed
//!   2  → block (stdout is shown as the denial reason)
//!   *  → depends on `HookDef.on_fail` (block or warn-and-continue)

use crate::policy::{HookDef, OnFail, PolicyDecision};
use serde_json::Value;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tracing::warn;

const HOOK_TIMEOUT: Duration = Duration::from_secs(10);

/// Run a single pre-exec hook.  Returns `Allow` or `Block`.
pub async fn run_pre_exec(hook: &HookDef, tool_name: &str, args: &Value) -> PolicyDecision {
    let payload = serde_json::json!({
        "tool": tool_name,
        "args": args,
    });
    let payload_bytes = match serde_json::to_vec(&payload) {
        Ok(b) => b,
        Err(e) => {
            warn!("hook: failed to serialize args: {e}");
            return PolicyDecision::Allow;
        }
    };

    let mut child = match Command::new("sh")
        .arg("-c")
        .arg(&hook.command)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("hook: failed to spawn '{}': {e}", hook.command);
            return on_spawn_fail(&hook.on_fail, &hook.command);
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        if let Err(e) = stdin.write_all(&payload_bytes).await {
            warn!("hook: failed to write args to stdin of '{}': {e}", hook.command);
        }
        // Drop closes stdin so the hook process can proceed.
    }

    let result = tokio::time::timeout(HOOK_TIMEOUT, child.wait_with_output()).await;

    match result {
        Err(_elapsed) => {
            warn!("hook: '{}' timed out after 10s", hook.command);
            on_fail_decision(&hook.on_fail, format!("hook timed out: {}", hook.command))
        }
        Ok(Err(e)) => {
            warn!("hook: '{}' wait error: {e}", hook.command);
            on_spawn_fail(&hook.on_fail, &hook.command)
        }
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let code = output.status.code().unwrap_or(-1);
            match code {
                0 => PolicyDecision::Allow,
                2 => PolicyDecision::Block(if stdout.is_empty() {
                    format!("hook blocked: {}", hook.command)
                } else {
                    stdout
                }),
                _ => on_fail_decision(
                    &hook.on_fail,
                    if stdout.is_empty() {
                        format!("hook exited {code}: {}", hook.command)
                    } else {
                        stdout
                    },
                ),
            }
        }
    }
}

fn on_spawn_fail(on_fail: &OnFail, cmd: &str) -> PolicyDecision {
    on_fail_decision(on_fail, format!("hook failed to start: {cmd}"))
}

fn on_fail_decision(on_fail: &OnFail, reason: String) -> PolicyDecision {
    match on_fail {
        OnFail::Block => PolicyDecision::Block(reason),
        OnFail::Warn => {
            warn!("{reason}");
            PolicyDecision::Allow
        }
    }
}
