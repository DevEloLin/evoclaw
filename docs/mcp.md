# MCP Servers

EvoClaw is a standard
[Model Context Protocol](https://modelcontextprotocol.io)
client. Add any MCP server to your local instance and its tools appear
in the agent's tool registry alongside the built-ins, with names of the
form `mcp__<server_id>__<tool>`.

## Built-in catalog

| ID            | Description                              | Auth env                              |
|---------------|------------------------------------------|---------------------------------------|
| `filesystem`  | Read/write files in configured roots     | —                                     |
| `github`      | Issues, PRs, repos via GitHub API        | `GITHUB_PERSONAL_ACCESS_TOKEN`        |
| `fetch`       | Fetch web pages as markdown              | —                                     |
| `time`        | Timezone & current-time queries          | —                                     |
| `brave-search`| Web search via Brave API                 | `BRAVE_API_KEY`                       |
| `postgres`    | Read-only SQL queries                    | `DATABASE_URL`                        |
| `slack`       | Read messages, post to channels          | `SLACK_BOT_TOKEN`, `SLACK_TEAM_ID`    |

```bash
evoclaw mcp catalog
```

## Adding a server

```bash
# Set the auth env var (if any) first — `add` captures it into the profile.
export GITHUB_PERSONAL_ACCESS_TOKEN=ghp_xxx

evoclaw mcp add github
evoclaw mcp test github      # spawn, initialize, list tools, shutdown
evoclaw mcp list
evoclaw mcp remove github
```

`add` writes a TOML profile to `~/.evoclaw/mcp/<id>.toml`:

```toml
id           = "github"
name         = "GitHub"
command      = "npx"
args         = ["-y", "@modelcontextprotocol/server-github"]
env          = [["GITHUB_PERSONAL_ACCESS_TOKEN", "ghp_xxx"]]
installed_at = "2026-05-02T17:52:00Z"
```

The captured auth env is part of the spawn config; the model never sees
the token directly.

## How tools surface to the model

On every `evoclaw run` (and inside the REPL on every prompt), the runtime
walks `~/.evoclaw/mcp/`, spawns each server, runs the MCP `initialize` +
`tools/list` handshake, and registers each advertised tool through
`McpToolWrapper`. A failed server is logged and skipped — it will not
prevent the agent from starting.

You will see a one-line banner:

```text
→ MCP: 2 server(s) attached, registry now has 19 tools
```

Tool names are namespaced as `mcp__<server>__<tool>` so two servers can
expose tools with the same local name without collision (e.g.
`mcp__github__search` vs `mcp__brave-search__search`).

## Slash commands

Inside the REPL:

```text
/mcp              list configured servers
/mcp catalog      show built-in servers
/mcp add <id>     add from catalog (captures auth env)
/mcp test <id>    spawn + initialize + tools/list
/mcp remove <id>  delete profile
```

## Troubleshooting

**`initialize failed: ...`** — the server binary spawned but rejected the
handshake. Common cause: missing or wrong env var. Re-export the value
and re-run `evoclaw mcp add <id>` to overwrite the profile.

**`spawn ... failed: No such file or directory`** — install the server's
runtime first. `npx` servers need Node ≥18; `uvx` servers need
`pipx install uv`.

**`mcp__<server>__<tool>: tool result is empty`** — the server returned
`isError: false` but no text content. Run the same command directly via
the server's own CLI to inspect; some MCP servers stream content blocks
that EvoClaw renders as `[image: <mime>]` markers when not text.
