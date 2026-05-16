# EvoClaw v1.0.1-beta.2 — Release Notes

> **Release date**: 2026-05-05
> **Branch**: `main`
> **Previous release**: v1.0.0-beta.1

---

## What changed

### New: User-configurable tool execution policy (`policy.toml`)

EvoClaw now ships a human-editable policy layer that lets you define **allow/deny rules and pre-execution hooks** for every tool call — without touching source code or recompiling.

On first run, `~/.evoclaw/policy.toml` is created automatically (chmod 600) with a commented template. Open it in any text editor to configure your rules.

#### Allow / deny rules

```toml
[deny]
# Block any shell command that touches .ssh
bash = ["* .ssh*", "*~/.ssh*"]

# Block writes outside safe paths
write_file = ["~/.ssh/**", "/etc/**"]

[allow]
# Optional: restrict bash to an explicit whitelist (uncomment to enable)
# bash = ["cargo *", "git *", "ls *"]
```

- **Pattern syntax**: `*` matches any sequence of characters, `?` matches one character
- **Deny always takes precedence** over allow
- **Tool keys**: `bash`, `write_file`, `read_file`, `patch_file`, `web_fetch`, `*` (matches all tools)
- **Subject matched**: for `bash` → full command string; for file tools → file path; for `web_fetch` → URL

#### Pre-execution hooks

Hooks are shell commands that run before a tool invocation. They receive `{"tool": "...", "args": {...}}` on stdin and control execution via exit code.

```toml
[[hooks.pre_exec]]
tool    = "bash"                                    # or "*" for all tools
command = "python3 ~/.evoclaw/hooks/audit.py"
on_fail = "block"                                   # "block" (default) or "warn"
```

Exit code protocol:

| Code | Meaning |
|------|---------|
| `0` | Proceed — tool runs normally |
| `2` | Block — hook's stdout is shown as the denial reason |
| other | Controlled by `on_fail`: `"block"` → denied, `"warn"` → log warning and continue |

#### How it fits the existing permission ladder

The built-in permission ladder (P0–P8) is the **hard floor** and cannot be overridden by policy rules. Policy enforcement runs _after_ the permission check and _before_ `tool.run()`:

```
Permission check (P0–P8)  ──▶  deny/allow rules  ──▶  pre-exec hooks  ──▶  tool.run()
```

#### Default template (auto-created)

```toml
# ~/.evoclaw/policy.toml  (chmod 600 — edit with any text editor)

[deny]
bash = [
    "* .ssh*",
    "*~/.ssh*",
]
```

The default template only adds the SSH deny guard. All other tool calls proceed exactly as before.

#### No migration required

Existing installs are unaffected. `policy.toml` is created automatically on the first run after upgrade. No existing behaviour changes unless you add custom rules.

---

### Fix: TUI streaming divider covers user question

**Bug**: During a streaming response, a `─────────────────` separator was rendered at the top of the streaming block. When the response grew long enough to fill the terminal height, this rule pushed the user's original question into the scrollback buffer — hiding it until the full response finished.

**Fix**: The top separator is no longer drawn during streaming. The streaming block now opens directly with the header line (`title · streaming · Xs`), consistent with finished response blocks. The bottom separator and task-status line are unchanged.

---

## Files changed

| File | Change |
|------|--------|
| `crates/evo-policy/src/policy.rs` | **New** — `PolicyConfig`, glob rule matching, `PolicyDecision` enum |
| `crates/evo-policy/src/hook.rs` | **New** — async pre-exec hook runner (stdin JSON, exit-code protocol) |
| `crates/evo-policy/src/lib.rs` | Export `policy` and `hook` modules + re-exports |
| `crates/evo-policy/Cargo.toml` | Add `toml` workspace dependency |
| `crates/evo-tools/src/lib.rs` | `ToolContext.policy` field; enforce in `ToolRegistry::invoke()` |
| `crates/evo-cli/src/config.rs` | Drop `policy.toml` template on first run (chmod 600); add `policy_path()` helper |
| `crates/evo-cli/src/task.rs` | Load `policy.toml`, inject into `ToolContext` (both runner sites) |
| `crates/evo-cli/src/commands/channel.rs` | Same for channel adapter runner |
| `crates/evo-cli/src/ui/renderer.rs` | Remove top separator from streaming block (`render_streaming_block`) |
| `scripts/check.sh` | Bump `evo-policy` LOC cap 1700 → 2000; core total 21900 → 22200 |

---

## Upgrade

```bash
git pull
cargo build --release
```

No config migration needed. `~/.evoclaw/policy.toml` is created automatically on the next `evoclaw` invocation.

---

## Known limitations

- **Workspace-level overrides** (per-project `.evoclaw.toml`) are not yet implemented — planned for a later beta.
- Hook commands inherit the same shell environment as `evoclaw` itself. No additional sandboxing is applied to hook processes.

---

*For earlier history see `git log --oneline`.*
