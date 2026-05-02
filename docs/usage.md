# Usage reference

Every CLI command, every config knob, every environment variable, every slash command.

---

## Two binaries, one CLI

EvoClaw ships **two** identical binaries with different names:

- **`evoclaw`** — long-form, project-named (recommended for scripts and docs)
- **`evo`** — 3-letter alias (terser; same code, same behaviour)

Both call into `evo_cli::entry()`. Pick whichever you like.

---

## Interactive shell vs subcommand mode

Run **with no subcommand** to enter the interactive REPL — banner, status panel, slash commands, just like `claude` or `codex`:

```bash
evoclaw                    # interactive
# or equivalently:
evoclaw shell
```

Run **with a subcommand** for one-shot non-interactive operation:

```bash
evoclaw run "..."          # one task and exit
evoclaw doctor             # health check and exit
evoclaw skill tree         # render tree and exit
```

The first time you run `evoclaw` with no config, the onboarding wizard appears automatically. After that, the banner shows up immediately.

---

## CLI subcommands

```
evoclaw [subcommand] [args]
```

| Subcommand | Purpose |
|------------|---------|
| *(none)* / `shell` | Enter interactive REPL |
| `onboard` | First-run setup; pick provider, write `~/.evoclaw/config.toml` |
| `login` | Switch provider / re-enter API key (interactive wizard) |
| `run <input>` | Run a one-shot task; full agent loop + reflection + skill save |
| `doctor` | Health check: config, model, fs, api_key |
| `doctor-of tokens` | 7-day / 30-day cost & cache stats |
| `doctor-of closure` | Audit recent session JSONLs against PRD §39 closure matrix |
| `replay [path]` | Pretty-print a session JSONL (defaults to most recent) |
| `skill list` | List every skill with state / score / version |
| `skill show <id>` | Print a skill's full YAML |
| `skill tree` | Rebuild & print the skill tree, grouped by kind |
| `memory search <query> [--limit N]` | Grep memory L1/L2/L3 |
| `agent ...` | External ACP agents (Claude / Codex / Cursor / Copilot) — see [agents.md](agents.md) |
| `mcp ...` | MCP servers (filesystem / github / fetch / time / brave / postgres / slack) — see [mcp.md](mcp.md) |
| `secret ...` | Local secret vault — values never reach the model (PRD §13.4) |
| `gateway [--bind X --token Y]` | Spawn `evo-gateway` HTTP daemon |

### Interactive REPL slash commands

When inside `evoclaw>`, type any of:

| Slash | Equivalent subcommand | Notes |
|-------|------------------------|-------|
| `/help` or `/?` | (no equivalent) | Print slash command list |
| `/login` | `evoclaw login` | Switch provider |
| `/agent [list/catalog/add/remove/test <id>]` | `evoclaw agent ...` | ACP agent management |
| `/mcp [list/catalog/add/remove/test <id>]` | `evoclaw mcp ...` | MCP server management |
| `/secret [list/add/remove/test ...]` | `evoclaw secret ...` | Secret vault management |
| `/skill [list/tree/show <id>]` | `evoclaw skill ...` | Skill management |
| `/memory <query>` | `evoclaw memory search <query>` | Memory grep |
| `/tokens` | `evoclaw doctor-of tokens` | Cost stats |
| `/closure` | `evoclaw doctor-of closure` | Closure audit |
| `/replay [path]` | `evoclaw replay [path]` | Session replay |
| `/doctor` | `evoclaw doctor` | Health check |
| `/clear` | (no equivalent) | Clear screen |
| `/exit` `/quit` `/q` | (no equivalent) | Exit (also Ctrl-D / EOF) |

Anything **not** starting with `/` is treated as a task and runs through the agent loop, identical to `evoclaw run "..."`.

---

## `evoclaw run` — full reference

```bash
evoclaw run "your natural-language task here"
```

What the runtime does, in order:

1. **Scrub user input through the redactor** (PRD §13.4) — vault values become `${SECRET:NAME}`, pattern matches become `[REDACTED:<kind>:<fp>]`. The unscrubbed string never re-enters the pipeline.
2. **Build system prompt** (PROMPTS §1, exactly 6 lines, cached as `persistent`).
3. **Compose user message** with `<history>` block (last 40 `<summary>` records) + `<user_input>`.
4. **Append `TaskRecord`** to the per-task JSONL log under `~/.evoclaw/logs/`.
5. **Loop** until the model emits zero tool calls or `max_turns` is hit:
   - Apply tag-level compression every 5 turns.
   - Pre-flight cost-budget check.
   - Build `ChatRequest` with `ToolFingerprint` short-circuit.
   - Stream model events; collect tool calls and assistant text.
   - Scrub assistant text and tool args through the redactor.
   - Dispatch tool calls via `ToolRegistry::invoke`. Built-in tools run inline; MCP-bridged tools invoke `mcp__<server>__<tool>` against the appropriate spawned subprocess.
   - Scrub each tool result, append `TurnRecord`, send tool results back to the model.
6. **Reflection round** (PROMPTS §4 → `ReflectionRecord`).
7. **Distillation** (PROMPTS §5 → `Skill` DRAFT, saved to `~/.evoclaw/skills/`).
8. **Memory L3 write** (one record per task).
9. **EndRecord** (`COMPLETED` or `FAILED`).

Hard caps:

- `max_turns = 25`
- `max_tokens = 1024` per response
- `temperature = 0.2`

Tune via `RuntimeConfig` if you embed the runtime; the CLI defaults are deliberate.

---

## `evoclaw secret` — local key vault (PRD §13.4)

| Sub | Effect |
|-----|--------|
| `add <name> [value]` | Insert/overwrite. Without a value the CLI prompts for it; with `--stdin` the value is read from one stdin line. |
| `list` | Show entries — only `name`, `kind`, fingerprint, `created_at`. **Raw values are never printed.** |
| `remove <name>` | Delete by name. |
| `test <text>` | Run the redactor on a sample string and print the scrubbed output. |

Inside the REPL the same operations are available as `/secret list`, `/secret add NAME VALUE` (or `/secret add NAME` for prompted entry), `/secret remove NAME`, `/secret test TEXT…`.

The vault file `~/.evoclaw/secrets/vault.json` is written `chmod 600` on Unix:

```json
{
  "version": 1,
  "entries": [
    { "name": "github_pat",
      "value": "ghp_actual_value",
      "kind": "github_pat",
      "fingerprint": "b4824fbd",
      "created_at": "2026-05-02T17:52:00Z" }
  ]
}
```

What the redactor catches even **without** a vault entry:

| Pattern | Example | Replaced with |
|---------|---------|----------------|
| `sk-ant-*` | Anthropic API key | `[REDACTED:anthropic_key:<fp>]` |
| `sk-*` (≥20 chars) | OpenAI API key | `[REDACTED:openai_key:<fp>]` |
| `ghp_/gho_/ghu_/ghs_/ghr_*` | GitHub PAT | `[REDACTED:github_pat:<fp>]` |
| `AKIA*` (20 alnum) | AWS key id | `[REDACTED:aws_key_id:<fp>]` |
| `eyJ*.*.*` | JWT | `[REDACTED:jwt:<fp>]` |
| any 32+ char string with Shannon entropy ≥ 4 bits/char | unspecified | `[REDACTED:high_entropy:<fp>]` |

`<fp>` is a stable 8-character SHA-256 prefix — same secret always gets the same fingerprint, useful for cross-referencing without ever leaking the value.

---

## `evoclaw agent` — external ACP agents

See **[docs/agents.md](agents.md)** for the dedicated guide. Quick reference:

```bash
evoclaw agent catalog          # show built-in agents
evoclaw agent add claude       # write profile to ~/.evoclaw/agents/claude.toml
evoclaw agent test claude      # spawn `claude --acp` and run initialize handshake
evoclaw agent list
evoclaw agent remove claude
```

To route the loop through an ACP agent, set `provider = "acp:<id>"` in `~/.evoclaw/config.toml` (the wizard's "External ACP agent" option does this automatically).

---

## `evoclaw mcp` — MCP servers

See **[docs/mcp.md](mcp.md)** for the dedicated guide. Quick reference:

```bash
export GITHUB_PERSONAL_ACCESS_TOKEN=ghp_xxx   # captured into the profile by `add`
evoclaw mcp add github
evoclaw mcp test github                       # spawn + initialize + tools/list
evoclaw mcp list
```

On every `evoclaw run` (and every prompt inside the REPL), the runtime walks `~/.evoclaw/mcp/`, spawns each server, and registers each advertised tool as `mcp__<server>__<tool>` in the same `ToolRegistry` the built-ins live in.

---

## `evoclaw replay [path]`

If `path` is omitted, EvoClaw picks the most recently modified `*.jsonl` under `~/.evoclaw/logs/`. Sections:

- `[TASK] <id>` — task record (input, source, model, started_at)
- `[TURN N]` — per-turn summary, tool calls, usage with cache-hit %
- `[END]` — final state (`COMPLETED` / `FAILED`)

Because every text field has already been scrubbed before write, the JSONL log is safe to share for debugging.

---

## `evoclaw skill <subcommand>`

| Sub | Effect |
|-----|--------|
| `list` | Tabular view of every `*.yaml` under `~/.evoclaw/skills/` |
| `show <id>` | Cat-style dump of one skill's YAML |
| `tree` | Re-scan all skills, rebuild `index.json`, render grouped by `SkillKind` |

Skills aging through the EWMA state machine:

```
Draft → Candidate → Active → Degraded → Deprecated
        (sandbox)   (≥3 wins)  (score<0.7)  (5 fails or score<0.3)
```

Only `Active` skills are auto-loaded into the planner. `Candidate` can be opted in.

---

## `evoclaw memory search`

```bash
evoclaw memory search "ssh timeout" --limit 10
```

Searches L1 (preferences), L2 (env facts), L3 (task records) by case-insensitive substring on `content` or `tags`. Newest-first. Memory is **append-only**; deletion happens via Phase 6 GC.

---

## `evoclaw gateway`

```bash
evoclaw gateway --bind 127.0.0.1:7878 --token mychat
```

Spawns the `evo-gateway` binary with two env vars set:

- `EVO_GATEWAY_BIND` — TCP listen address
- `EVO_GATEWAY_ALLOWLIST` — comma-separated bearer tokens

Routes:

| Method / Path | Auth | Action |
|---------------|------|--------|
| `GET /` | none | Static WebChat HTML |
| `GET /healthz` | none | `200 ok` |
| `POST /chat` | `Authorization: Bearer <token>` | Body `{"input": "..."}`, returns `{task_id, turns, final_text}` |

Local-only by design. No TLS. Bind to `0.0.0.0` only on a trusted network.

---

## Configuration: `~/.evoclaw/config.toml`

```toml
[model]
provider = "deepseek"                                 # picks the API key file + provider adapter
default  = "deepseek-chat"
base_url = "https://api.deepseek.com/v1"
fallback = ["qwen-plus", "kimi-k2"]

[budget]
per_task_usd  = 0.50
per_day_usd   = 5.0
per_month_usd = 100.0

[security]
default_permission  = "P1"
high_risk_intercept = true
```

| Field | Type | Default | Notes |
|-------|------|---------|-------|
| `model.provider` | string | `deepseek` | Drives both API-key resolution and provider adapter (or `acp:<id>` for ACP) |
| `model.default` | string | `deepseek-chat` | Sent as `model` to provider |
| `model.base_url` | URL | `https://api.deepseek.com/v1` | Any OpenAI-compat endpoint; ignored for ACP providers |
| `model.fallback` | array | `[]` | (Phase 3+) used when primary fails |
| `budget.per_task_usd` | float | 0.50 | HardStop trigger (PRD §35) |
| `budget.per_day_usd` | float | 5.0 | SoftWarn (3-tier; hard cap is 4× this) |
| `budget.per_month_usd` | float | 100.0 | HardStop |
| `security.default_permission` | enum | `P1` | P0..P8 ladder |
| `security.high_risk_intercept` | bool | `true` | Force `ask_user` on P5+ ops |

Permission ladder (PRD §13.1):

```
P0 read-only     P1 workspace write   P2 local-safe shell
P3 network       P4 browser           P5 user-dir write
P6 system        P7 credentials       P8 production
```

Channel senders are hard-capped at P4 even when `default_permission` is higher.

---

## Provider field cheatsheet

| `provider` value | Meaning | Where the auth lives |
|------------------|---------|----------------------|
| `deepseek` `kimi` `qwen` `openai` `openrouter` | OpenAI-compat HTTP | `~/.evoclaw/secrets/<provider>.key` (chmod 600) |
| `anthropic` | Native Anthropic Messages API | `~/.evoclaw/secrets/anthropic.key` |
| `copilot` | GitHub Copilot via OAuth Device Flow | `~/.evoclaw/secrets/copilot.key` (refresh token) |
| `ollama` (or other `local`) | OpenAI-compat at `http://localhost:11434/v1` | (no key) |
| `acp:claude` `acp:codex` `acp:cursor` `acp:copilot` | External CLI via ACP | The CLI's own login (`claude login`, `gh auth login`, …) |
| `custom` | Any OpenAI-compat endpoint you set | Your own key |

---

## Environment variables

| Var | Required by | Purpose |
|-----|-------------|---------|
| `EVO_API_KEY` | `evoclaw run`, `evoclaw gateway`, `evo-gateway` | Bearer for the model provider — overrides the on-disk key |
| `EVO_GATEWAY_BIND` | `evo-gateway` | Override `127.0.0.1:7878` |
| `EVO_GATEWAY_ALLOWLIST` | `evo-gateway` | Comma-separated bearer tokens |
| `RUST_LOG` | all | `tracing` filter, e.g. `RUST_LOG=evo_core=debug` |
| `NO_COLOR` / `EVO_NO_COLOR` | `evoclaw` | Suppress ANSI in the welcome banner |

API keys captured by the wizard are persisted under `~/.evoclaw/secrets/` (chmod 600). Vault entries — separate from provider keys — are stored in `~/.evoclaw/secrets/vault.json` and used exclusively for redaction.

---

## Tool inventory (built-ins, capped at 10)

| # | Name | Permission | Purpose |
|---|------|------------|---------|
| 1 | `read_file` | P0 | Read with line numbers |
| 2 | `write_file` | P1 | Workspace-bounded write |
| 3 | `patch_file` | P1 | Replace unique substring (must match exactly once) |
| 4 | `list_dir` | P0 | Directory listing, excludes `node_modules`/`.git`/`target` |
| 5 | `run_shell` | P2 | Sandboxed `sh -c`, 30s default timeout |
| 6 | `web_fetch` | P3 | HTTPS only, cookies excluded from LLM context |
| 7 | `ask_user` | P0 | High-risk / ambiguous / missing-param confirmation |

MCP-bridged tools (e.g. `mcp__github__list_issues`) live in the same registry but do **not** count against the 10-tool cap. They are wrapped as `McpToolWrapper` with `Permission::P3` (network).

---

## File system contract

```
~/.evoclaw/
├── config.toml                          # this file
├── workspace/                           # tools sandboxed here
├── logs/{task-id}.jsonl                 # one log per task
├── skills/{skill-id}.yaml               # one YAML per skill
├── skills/index.json                    # skill tree summary
├── memory/{L0..L5}.jsonl                # one file per memory layer
├── secrets/<provider>.key               # API key per provider, chmod 600
├── secrets/vault.json                   # named secret vault, chmod 600
├── agents/<id>.toml                     # ACP agent profiles
├── mcp/<id>.toml                        # MCP server profiles
├── plugins/                             # reserved
├── cache/                               # transient
└── cost.jsonl                           # one cost event per turn
```

JSONL records are typed via `kind: "task" | "turn" | "end"` (`evo_core::session::SessionRecord`). Schemas are stable from v0.4.

---

## Exit codes

| Code | Cause |
|------|-------|
| 0 | success |
| 1 | runtime error (model 4xx/5xx, tool error, IO, missing key) |
| 2 | budget hard-stop (`Budget(...)` in error chain) |
| 130 | Ctrl-C |

---

## Cheatsheet

```bash
# the loop
evoclaw run "..."                       # do work
evoclaw replay                          # see what just happened
evoclaw doctor-of tokens                # how much it cost
evoclaw doctor-of closure               # session integrity audit

# the brain
evoclaw skill list                      # what we've learned
evoclaw skill tree                      # by domain
evoclaw memory search "..."             # find a fact

# the integrations
evoclaw agent catalog                   # ACP agents
evoclaw mcp catalog                     # MCP servers
evoclaw secret add github_pat           # local-only key vault

# the hub
evoclaw gateway --bind 127.0.0.1:7878   # WebChat for browser users
```
