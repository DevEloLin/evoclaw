use crate::{Tool, ToolContext, ToolError, ToolFactory};
use async_trait::async_trait;
use evo_policy::Permission;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

const RUN_SHELL_TIMEOUT_CAP_MS: u64 = 60_000;
const RUN_SHELL_SAFE_ENV: &[&str] = &["PATH", "HOME", "USER", "LANG", "LC_ALL", "TZ", "TERM"];

#[derive(Deserialize)]
struct ShellArgs {
    cmd: String,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

pub struct RunShell;

#[async_trait]
impl Tool for RunShell {
    fn name(&self) -> &str {
        "run_shell"
    }
    fn description(&self) -> &str {
        "Run shell. Sandboxed, 30s default, output truncated 8K."
    }
    fn permission(&self) -> Permission {
        Permission::P2
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "cmd": { "type": "string" },
                "timeout_ms": { "type": "integer", "minimum": 1, "maximum": 60000 },
            },
            "required": ["cmd"],
            "additionalProperties": false,
        })
    }
    async fn run(&self, ctx: &ToolContext, args: Value) -> Result<String, ToolError> {
        let a: ShellArgs =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        ctx.deny_if_shell_targets_self(&a.cmd)?;
        ctx.deny_if_targets_ssh_dir(&a.cmd)?;
        let requested = a
            .timeout_ms
            .map(Duration::from_millis)
            .unwrap_or(ctx.default_shell_timeout);
        let cap = Duration::from_millis(RUN_SHELL_TIMEOUT_CAP_MS);
        let timeout = if requested > cap { cap } else { requested };

        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c")
            .arg(&a.cmd)
            .current_dir(&ctx.workspace)
            .env_clear()
            .kill_on_drop(true);
        // Re-add only safe environment variables from the parent process so a
        // model-issued command cannot exfiltrate secrets like EVO_API_KEY.
        for key in RUN_SHELL_SAFE_ENV {
            if let Ok(val) = std::env::var(key) {
                cmd.env(key, val);
            }
        }
        let output = tokio::time::timeout(timeout, cmd.output())
            .await
            .map_err(|_| ToolError::Timeout(timeout))?
            .map_err(ToolError::Io)?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let code = output.status.code().unwrap_or(-1);
        Ok(format!(
            "exit={code}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            stdout.trim_end(),
            stderr.trim_end()
        ))
    }
}

inventory::submit!(ToolFactory {
    build: || Box::new(RunShell)
});

#[cfg(test)]
mod tests {
    use super::*;
    use evo_policy::Permission;
    use serde_json::json;
    use std::path::Path;
    use std::sync::Arc;

    fn ctx_in(dir: &Path) -> ToolContext {
        ToolContext::default_for_workspace(dir.to_path_buf()).with_max_permission(Permission::P3)
    }

    fn unique_tmp(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("evo-tools-{name}-{stamp}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn ctx_with_fake_exe(dir: &Path, exe_name: &str) -> ToolContext {
        let exe_path = dir.join(exe_name);
        std::fs::write(&exe_path, b"fake-binary").unwrap();
        let canonical = exe_path.canonicalize().unwrap();
        ToolContext {
            self_exe: Some(Arc::new(canonical)),
            ..ctx_in(dir)
        }
    }

    #[tokio::test]
    async fn run_shell_captures_exit() {
        let dir = unique_tmp("shell");
        let ctx = ctx_in(&dir);
        let out = RunShell
            .run(&ctx, json!({"cmd": "echo hello"}))
            .await
            .unwrap();
        assert!(out.contains("exit=0"));
        assert!(out.contains("hello"));
    }

    #[tokio::test]
    async fn run_shell_strips_parent_env() {
        // Set a sentinel var on the parent. With env_clear() it must NOT
        // appear in the child's environment.
        std::env::set_var("EVO_SECRET_SENTINEL", "should-not-leak");
        let dir = unique_tmp("shell-env");
        let ctx = ctx_in(&dir);
        let out = RunShell
            .run(
                &ctx,
                json!({"cmd": "echo \"sentinel=${EVO_SECRET_SENTINEL:-missing}\""}),
            )
            .await
            .unwrap();
        assert!(out.contains("sentinel=missing"), "leaked env: {out}");
    }

    #[tokio::test]
    async fn run_shell_blocked_when_cmd_references_self_exe() {
        let dir = unique_tmp("self-shell");
        let ctx = ctx_with_fake_exe(&dir, "evoclaw");
        let err = RunShell
            .run(&ctx, json!({"cmd": "rm evoclaw"}))
            .await
            .expect_err("must deny shell referencing own exe");
        assert!(matches!(err, ToolError::Denied(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn run_shell_allowed_when_cmd_unrelated_to_self() {
        let dir = unique_tmp("self-shell-ok");
        let ctx = ctx_with_fake_exe(&dir, "evoclaw");
        let out = RunShell
            .run(&ctx, json!({"cmd": "echo hello"}))
            .await
            .unwrap();
        assert!(out.contains("hello"));
    }

    #[tokio::test]
    async fn run_shell_blocked_for_tilde_ssh() {
        let dir = unique_tmp("ssh-tilde");
        let ctx = ctx_in(&dir);
        let err = RunShell
            .run(&ctx, json!({"cmd": "cat ~/.ssh/id_rsa"}))
            .await
            .expect_err("must block ~/.ssh access");
        assert!(matches!(err, ToolError::Denied(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn run_shell_blocked_for_absolute_ssh_path() {
        let dir = unique_tmp("ssh-abs");
        let ctx = ctx_in(&dir);
        let err = RunShell
            .run(&ctx, json!({"cmd": "ls /Users/wei/.ssh/"}))
            .await
            .expect_err("must block absolute .ssh path");
        assert!(matches!(err, ToolError::Denied(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn run_shell_blocked_for_rm_ssh() {
        let dir = unique_tmp("ssh-rm");
        let ctx = ctx_in(&dir);
        let err = RunShell
            .run(&ctx, json!({"cmd": "rm -rf ~/.ssh/authorized_keys"}))
            .await
            .expect_err("must block .ssh deletion");
        assert!(matches!(err, ToolError::Denied(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn run_shell_blocked_for_home_env_ssh() {
        let dir = unique_tmp("ssh-home-env");
        let ctx = ctx_in(&dir);
        let err = RunShell
            .run(&ctx, json!({"cmd": "cat $HOME/.ssh/config"}))
            .await
            .expect_err("must block $HOME/.ssh access");
        assert!(matches!(err, ToolError::Denied(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn run_shell_allowed_when_unrelated_to_ssh() {
        let dir = unique_tmp("ssh-unrelated");
        let ctx = ctx_in(&dir);
        let out = RunShell
            .run(&ctx, json!({"cmd": "echo sshd is running"}))
            .await
            .unwrap();
        assert!(out.contains("sshd is running"));
    }
}
