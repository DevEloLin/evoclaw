# External ACP Agents

EvoClaw can delegate the agent loop to an external CLI that speaks the
[Agent Client Protocol](https://github.com/zed-industries/agent-client-protocol)
(ACP) — the same open spec Zed uses to integrate Claude Code, Codex, Cursor,
and GitHub Copilot. When you pick an ACP agent, that agent handles its own
authentication and tool-use loop; EvoClaw treats it as a black-box turn
responder.

## Why ACP

- **Auth stays where it belongs.** Claude Code logs in with `claude login`,
  Cursor with `cursor login`, GitHub Copilot via OAuth device flow. EvoClaw
  never sees, stores, or proxies their credentials.
- **No browser scraping, no ToS risk.** No headless Chrome, no session-cookie
  theft. The ACP CLI is the official client, used as intended.
- **One protocol, many agents.** Add a new ACP-capable CLI to `~/.evoclaw/agents/`
  and EvoClaw can drive it without code changes.

## Built-in catalog

| ID        | Bin             | Args                     | Install                                  |
|-----------|-----------------|--------------------------|------------------------------------------|
| `claude`  | `claude`        | `--acp`                  | `npm i -g @anthropic-ai/claude-code`     |
| `codex`   | `codex`         | `--acp`                  | `pip install codex-cli` (or `cargo`)     |
| `cursor`  | `cursor-agent`  | `--acp`                  | Bundled with the Cursor desktop app      |
| `copilot` | `gh`            | `copilot suggest --acp`  | `gh extension install github/gh-copilot` |

Inspect the catalog at any time:

```bash
evoclaw agent catalog
```

## Adding an agent

```bash
# Pick from the catalog
evoclaw agent add claude

# Verify the agent spawns and the ACP initialize handshake completes
evoclaw agent test claude

# List configured agents
evoclaw agent list

# Remove
evoclaw agent remove claude
```

`add` writes a TOML profile to `~/.evoclaw/agents/<id>.toml`:

```toml
id           = "claude"
name         = "Claude Agent"
command      = "claude"
args         = ["--acp"]
env          = []
installed_at = "2026-05-02T17:52:00Z"
```

## Routing the agent loop through an ACP agent

Two equivalent ways to switch:

1. **Wizard** — run `evoclaw onboard` (or `evoclaw login`) and pick the last
   menu entry "External ACP agent", then choose one. The wizard writes
   `~/.evoclaw/config.toml` with `provider = "acp:<id>"`.
2. **Manual** — edit `~/.evoclaw/config.toml` directly:
   ```toml
   [model]
   provider = "acp:claude"
   default  = "acp:claude"
   base_url = ""
   fallback = []
   ```

Once configured, anything you type into `evoclaw` (interactive REPL or
`evoclaw run "<task>"`) flows through `AcpProvider` → ACP `session/prompt`
→ the agent's final text.

## Slash commands

Inside the REPL:

```text
/agent              list configured agents
/agent catalog      show built-ins
/agent add <id>     add from catalog
/agent test <id>    spawn + initialize handshake
/agent remove <id>  delete profile
```

## Troubleshooting

**`spawn <id> failed: No such file or directory`** — the binary is not on
PATH. Install per the catalog hint, or set `command` in the TOML profile
to an absolute path.

**`ACP initialize: ...`** — the agent spawned but the handshake failed.
Run the agent's own login command (e.g. `claude login`) and retry
`evoclaw agent test <id>`.

**Slow first turn** — many ACP agents fetch their own remote config on
boot. The first prompt after `add` may take a few seconds longer than
subsequent ones.
