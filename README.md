# 🕷 EvoClaw — Self-evolving Personal AI Agent

<p align="center">
  <img src="./docs/image/EvoClaw.png" alt="EvoClaw — self-evolving personal AI agent" width="700">
</p>



<p align="center">
  <b>LEARN. REMEMBER. EVOLVE.</b>
</p>

<p align="center">
  <a href="./LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue?style=for-the-badge" alt="MIT license"></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-1.80%2B-orange?style=for-the-badge&logo=rust" alt="Rust 1.80+"></a>
  <img src="https://img.shields.io/badge/tests-153%20passing-4c1?style=for-the-badge" alt="153 tests passing">
  <img src="https://img.shields.io/badge/data-stays_local-green?style=for-the-badge" alt="local-first">
  <img src="https://img.shields.io/badge/secrets-never_leave_machine-red?style=for-the-badge" alt="secret-redacted">
</p>

<p align="center">
  <b>🌐 Website</b>: <a href="https://develolin.github.io/EvoClawSite/">develolin.github.io/EvoClawSite</a> ·
  <b>📦 Version</b>: <a href="./version"><code>v0.3.3</code></a> ·
  <b>🇨🇳 中文文档</b>: <a href="./docs/zh/README.md">docs/zh/README.md</a>
</p>

---

**EvoClaw** is a *self-evolving personal AI assistant* you run on your own machine. It plans, runs tools, observes the results, and keeps going until the job is done — then quietly distills what it just learned into a **YAML Skill** on disk so the next similar task is shorter, cheaper, and sharper. Written in Rust as **one binary** (`evoclaw`, with the `evo` alias) plus an optional local HTTP daemon. ~8 K LOC core. Two binaries. Zero telemetry.

The loop is short: *type a task → it plans, calls tools, observes, replans, finishes → it reflects → it writes a Skill.* Everything else — the cache fingerprints, the redaction barrier, the EWMA score, the JSONL replay, the ACP bridge, the MCP bridge, the secret vault — exists to make that loop **trustworthy enough that you'll let it run unsupervised on your real machine**.

---

## What sets EvoClaw apart

A handful of design choices we made that we think matter:

| Capability | EvoClaw delivers |
|------------|------------------|
| **Single static binary** | Rust 1.80+, ~8 K LOC core, zero runtime dependencies, sub-100 ms cold start |
| **Self-evolving Skill Tree** | five-state EWMA lifecycle (Draft → Candidate → Active → Degraded → Deprecated); **Active** Skills auto-load into the next planner round |
| **Secret-redaction barrier** | named **Vault** + pattern catch-all (`sk-*`, `ghp_*`, `AKIA…`, JWT, 32-char-plus high-entropy); idempotent scrub at **6 boundary points** in the runtime |
| **Token economy** | five layered tricks (schema fingerprint, ephemeral cache, summary working memory, head+tail truncation, periodic compression); **long-task spend ≈ ⅓** of the naive cost; each trick unit-tested |
| **ACP standard interop** | **7 upstream agents** ship in the catalog: Claude Code, Codex, Cursor, GitHub Copilot, Gemini CLI, Aider, Qwen Code (阿里通义灵码) |
| **MCP standard interop** | **7 servers** in the built-in catalog: filesystem, GitHub, fetch, time, Brave Search, Postgres, Slack — plus bring-your-own |
| **Provider catalog** | **17 model vendors** in one wizard: 9 国内 (DeepSeek, Kimi, Qwen, Doubao, Zhipu GLM, Baidu Qianfan, MiniMax, StepFun, Tencent Hunyuan) + 8 国际 (OpenAI, Anthropic, Gemini, Copilot, Mistral, Groq, Together, Fireworks, OpenRouter) + 3 local (Ollama, vLLM, llama.cpp) + custom endpoint |
| **Login → pick model** | wizard fetches `/v1/models` from your provider and lets you pick — or accept the catalog default with one keystroke |
| **Append-only JSONL audit** | one log per task; `evoclaw replay <log>` rehydrates any session; `evoclaw doctor closure` walks every log and asserts the 13-row closure matrix |
| **6-line system prompt cap** | enforced in CI by `scripts/check.sh`; every tool description capped at 80 chars |
| **Permission ladder P0..P8** | totally ordered; default ceiling **P1**; channel senders hard-capped at **P4** regardless of config |
| **Three-tier budget engine** | per-task hard stop, per-day soft warn + hard cap (4×), per-month hard cap; `doctor-of tokens` reports cache-hit rate |
| **Standard CLI ergonomics** | run `evoclaw` with no subcommand → REPL with Tab auto-completion, history navigation (↑↓ / Ctrl-P/N), reverse search (Ctrl-R), vim-style line-editing (Ctrl+A/E/K/U/W), and slash commands (`/agent /mcp /secret /skill /memory /tokens /closure /replay /doctor /logout /config /status /model /profile /usage /clear /exit`) |
| **Zero telemetry** | no analytics SDK, no remote pings, no "anonymous usage statistics" toggle hiding the real one |
| **Local-first by default** | every byte of state lives under `~/.evoclaw/` on your machine — vault, agents/*.toml, mcp/*.toml, JSONL logs, learned Skills |

---

## What is this?

EvoClaw is **one binary** — `evoclaw` (or its three-letter alias `evo`) — plus an optional local HTTP daemon (`evo-gateway`).

You type a task in plain English (or any language). EvoClaw plans, calls tools, observes the results, plans again, and keeps going until the job is done. Then it runs a quiet little post-mortem on itself, distills what it just learned into a **YAML Skill**, and saves it to disk. The next time you ask for something similar, that Skill kicks in and the run is shorter, cheaper, and sharper.

That's the loop. Everything else — the cache fingerprints, the redaction barrier, the EWMA score, the JSONL replay, the ACP bridge, the MCP bridge, the secret vault — exists to make that loop **trustworthy enough that you'll let it run unsupervised on your real machine**.

---

## Architecture at a glance

![EvoClaw architecture overview](./docs/image/architecture-en1.png)

> Seven layers, top-to-bottom: Frontends → Gateway/Wizard → Agent Loop → Capabilities → Routing & Adapters → Policy/Cost/Redact → Persistence. Each layer fans out left-to-right; cross-layer dependencies always flow downward. Live diagrams: <https://develolin.github.io/EvoClawSite/architecture-en.html>.

The agent loop closes itself with a reflection + distillation round at the end of every task:

![EvoClaw design — task lifecycle and skill EWMA](./docs/image/design.png)

> Task FSM (RECEIVED → PLANNING → TOOL_EXECUTING → OBSERVING → REFLECTING → DISTILLING → COMPLETED → ARCHIVED) and Skill FSM (Draft → Candidate → Active → Degraded → Deprecated) drive the self-evolution. Live design diagrams: <https://develolin.github.io/EvoClawSite/design-en.html>.

---

## The three things that make EvoClaw different

### 1. **It learns.** Not in a vector-DB-and-pray way. In a structured, auditable, deletable way.

After every task, EvoClaw runs a **reflection round** (one extra model call, deliberately) that asks: *what was the user actually trying to do, what worked, what didn't, what's reusable?* The answer becomes a YAML Skill on disk. Skills move through five states — **Draft → Candidate → Active → Degraded → Deprecated** — driven by an EWMA score. Active skills feed the next planner round; Deprecated skills get archived but never deleted, because audit trails matter more than disk space.

You can inspect any skill (`evoclaw skill show <id>`), grep for one (`evoclaw memory search "..."`), or rebuild the whole tree (`evoclaw skill tree`). Nothing is opaque. Nothing is sent anywhere.

### 2. **It scrubs secrets before they ever leave the machine.**

There's a **two-layer redaction barrier** between you and the model:

- **Vault layer.** Run `evoclaw secret add github_pat ghp_…`, the raw value lands in `~/.evoclaw/secrets/vault.json` (chmod 600). From that moment forward any string containing it that goes anywhere near the model becomes `${SECRET:github_pat}`.
- **Pattern layer.** Even if you never registered a secret, common credential shapes — `sk-*`, `sk-ant-*`, `ghp_*` family, `AKIA…`, JWTs, plus any 32-character-plus high-entropy blob — get caught and rewritten as `[REDACTED:<kind>:<8-char-fingerprint>]` before they touch the prompt, the JSONL session log, or the memory layers.

Both layers are **idempotent**. The fingerprint is a stable SHA-256 prefix, so the same secret always gets the same fingerprint and you can correlate across logs without ever exposing the value. There's no "we'll redact it later in a GC pass" window — scrubbing happens at the runtime boundary, on the way in.

### 3. **It's frugal — measurably so.**

A long agent task on a naive harness can easily cost 10× what it should. EvoClaw's runtime applies five layered tricks, each measured and asserted in unit tests:

| Trick | What it does | Token saving |
|-------|--------------|--------------|
| Tool-schema fingerprint | Sends the full tool list once every 10 turns; in between it sends a 16-byte hash and a "still active" marker | 25–40% prompt tokens |
| Ephemeral cache markers | Marks system prompt as persistent, recent assistant turns as ephemeral; the model API caches both | 60–70% effective spend |
| `<summary>` working memory | Every assistant turn ends with a 30-char `<summary>…</summary>`; the next turn replaces the full assistant message with that summary | 80–90% history bytes |
| Head+tail observation truncation | Tool output stays human-readable but the middle gets `…`-replaced before going back to the model | 50–70% observation tokens |
| Periodic tag-level compression | Every 5 turns, older `<observation>` blocks are squashed into single-line digests | 50–60% on long tasks |

All five are wired and unit-tested. You can `evoclaw doctor-of tokens` to see the cache-hit rate from your last seven days.

---

## Install

```bash
git clone https://github.com/DevEloLin/evoclaw && cd evoclaw
cargo build --workspace --release
./target/release/evoclaw            # interactive setup wizard runs on first launch
```

Full instructions: [`docs/installation.md`](./docs/installation.md) · 中文: [`docs/zh/installation.md`](./docs/zh/installation.md)

---

## Quick start

Type **`evoclaw`** — no subcommand — to enter the interactive shell:

```
$ evoclaw

──────────────────────────────────────────────────────────────────────────────────
  \\  ▄   ▄  //        Quick start
    ▄███████▄           ──────────────────────────
    █       █           /help    list all commands
    █ ▀▀ ▀▀ █           /login   configure auth
    ▀█▄▄▄▄▄█▀           /doctor  health check
      ▄▄ ▄▄             /skill   browse skills
  //  ██ ██  \\

  EvoClaw  v0.3.3       Status
  self-evolving agent   ──────────────────────────
  runtime               auth    ✓ ready
                        model   deepseek-chat
  deepseek  ·  deepseek-chat
  ~/.evoclaw            Ctrl-D to exit  ·  /help for commands
──────────────────────────────────────────────────────────────────────────────────

─ input ─────────────────────────────────────────────────────────────────────────
  ▷ Type your message and press Enter to send  ·  /help for commands
─────────────────────────────────────────────────────────────────────────────────
shortcuts: Tab /cmd  ·  ↑↓/Ctrl-P/N history  ·  Ctrl-R search  ·  Ctrl-C quit
```

Type a question and press Enter — it appears in the conversation area above while the
assistant streams in-place on a single status line:

```
─ You · 12:00:05 ────────────────────────────────────────────────────────────────
diagnose why my SSH hangs intermittently
─────────────────────────────────────────────────────────────────────────────────

────────────────────────────────────────────────────── (streaming)
EvoClaw · streaming · 3.1s
The most common cause is TCP keepalive not configured...
─────────────────────────────────────────────────────────────────────────────────

─ input ─────────────────────────────────────────────────────────────────────────
  ▷ Type your message and press Enter to send  ·  /help for commands
─────────────────────────────────────────────────────────────────────────────────
shortcuts: Tab /cmd  ·  ↑↓/Ctrl-P/N history  ·  Ctrl-R search  ·  Ctrl-C quit
```

When done the full answer scrolls into the history above the fixed input box:

```
─ EvoClaw · deepseek · 12:00:05 ─────────────────────────────────────────────────
The most common cause is TCP keepalive not being configured.

## Fix
Add to ~/.ssh/config:

  ServerAliveInterval 60
  ServerAliveCountMax 3

─────────────────────────────────────────────────────────────────────────────────

─ input ─────────────────────────────────────────────────────────────────────────
  ▷ Type your message and press Enter to send  ·  /help for commands
─────────────────────────────────────────────────────────────────────────────────
shortcuts: Tab /cmd  ·  ↑↓/Ctrl-P/N history  ·  Ctrl-R search  ·  Ctrl-C quit
```

5-minute walkthrough: [`docs/getting-started.md`](./docs/getting-started.md) · 中文: [`docs/zh/getting-started.md`](./docs/zh/getting-started.md)

---

## Secret redaction

```bash
# Register a secret — the raw value never leaves the machine
$ evoclaw secret add github_pat ghp_abc123…
✓ stored 'github_pat' (kind=github_pat, fingerprint=b4824fbd) at …/vault.json
  the model will never see the raw value — only ${SECRET:github_pat}

# List entries — only metadata, never raw values
$ evoclaw secret list
NAME                     KIND           FINGER     CREATED
github_pat               github_pat     b4824fbd   2026-05-02 17:52

# Verify on a sample string
$ evoclaw secret test "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjMifQ.fakesigvalue"
output : Authorization: Bearer [REDACTED:jwt:9d25a2da]
hits   : 1 substitution(s)
```

Vault file layout (`~/.evoclaw/secrets/vault.json`, chmod 600):

```json
{
  "version": 1,
  "entries": [
    {
      "name": "github_pat",
      "value": "ghp_actual_value_only_on_disk",
      "kind": "github_pat",
      "fingerprint": "b4824fbd",
      "created_at": "2026-05-02T17:52:00Z"
    }
  ]
}
```

---

## Multi-Profile Configuration

Switch between different model providers and configurations seamlessly with profiles:

```bash
# List available profiles
$ evoclaw
evoclaw> /profile list
Available profiles:
  * default        Default configuration
    deepseek       DeepSeek Chat (https://api.deepseek.com/v1)
    claude         Claude 3.5 Sonnet (Anthropic)
    gemini         Google Gemini Flash

# Create a new profile from template
evoclaw> /profile add myopenai --template openai
✓ created profile 'myopenai' from template 'openai'

# Switch to a different profile
evoclaw> /profile switch deepseek
✓ switched to profile 'deepseek'
✓ provider: deepseek
✓ model: deepseek-chat

# View current profile in /status
evoclaw> /status
╭─ Provider & Model ─────────────────────────────────╮
  Active Profile...... deepseek
  Vendor.............. DeepSeek (Cloud)
  Model............... deepseek-chat
╰────────────────────────────────────────────────────╯
```

**Built-in templates**: `deepseek`, `openai`, `claude`, `gemini`, `ollama`

Each profile is a separate `~/.evoclaw/profiles/<name>.toml` file with its own provider, model, budget, and security settings. Switch profiles instantly without editing config files manually.

---

## Built-in tools (ship 12)

| # | Name | Permission | What it does |
|---|------|------------|--------------|
| 1 | `read_file` | P0 | Read a file with line numbers; the agent must read before edit. |
| 2 | `write_file` | P1 | Workspace-bounded write (won't escape `~/.evoclaw/workspace/`). |
| 3 | `patch_file` | P1 | Replace a unique substring with a new one. Refuses ambiguous matches. |
| 4 | `list_dir` | P0 | List a directory; quietly skips `node_modules / .git / target / .venv / dist / build / __pycache__`. |
| 5 | `run_shell` | P2 | `sh -c` with a 30-second default timeout, output capped at 8K. |
| 6 | `web_fetch` | P3 | HTTPS only. Cookies are stripped before the body reaches the model. |
| 7 | `ask_user` | P0 | Mandatory call when params are ambiguous or the action is high-risk. |
| 8 | `browser_navigate` | P3 | Navigate headless browser to URL; returns page title and body text. |
| 9 | `browser_screenshot` | P3 | Screenshot the current page to a PNG; returns the saved path. |
| 10 | `browser_click` | P3 | Click an element matching a CSS selector. |
| 11 | `browser_type` | P3 | Type text into a form field matching a CSS selector. |
| 12 | `browser_eval` | P3 | Evaluate JavaScript in the browser page; returns the result. |

The permission ladder runs **P0** (read-only) → **P8** (production ops). The default ceiling is P1; messages arriving over a remote channel are hard-capped at P4 regardless of config. Browser tools (8–12) require Chrome/Chromium on the host and P3 permission.

**Need more tools?** Prefer attaching an MCP server over adding new built-ins.

---

## Standard interop: ACP + MCP

### ACP — Agent Client Protocol (delegate the loop)

Want the agent loop run by an upstream coding CLI you already trust? Set `provider = "acp:claude"` (or `acp:codex`, `acp:cursor`, `acp:copilot`) in `~/.evoclaw/config.toml`. EvoClaw spawns the upstream binary as a subprocess, hands it your prompt over JSON-RPC, and surfaces the response. The upstream CLI handles its own auth — EvoClaw never touches their credentials.

```bash
evoclaw agent catalog          # see the seven built-in agent profiles
evoclaw agent add claude       # write ~/.evoclaw/agents/claude.toml
evoclaw agent test claude      # spawn `claude --acp` + ACP initialize handshake
```

Full guide: [`docs/agents.md`](./docs/agents.md) · 中文: [`docs/zh/agents.md`](./docs/zh/agents.md)

### MCP — Model Context Protocol (bring your own tools)

Run any standard Anthropic MCP server and its tools appear in EvoClaw's registry namespaced as `mcp__<server>__<tool>`. Auth env vars are captured at `add` time and passed to the spawned child; the model never sees them.

```bash
export GITHUB_PERSONAL_ACCESS_TOKEN=ghp_xxx
evoclaw mcp add github
evoclaw mcp test github        # spawn + initialize + tools/list
```

Full guide: [`docs/mcp.md`](./docs/mcp.md) · 中文: [`docs/zh/mcp.md`](./docs/zh/mcp.md)

---

## Filesystem contract

```
~/.evoclaw/
├── config.toml                        # active provider, model, budget, security (symlink to active profile)
├── profiles/                          # multi-profile configuration system
│   ├── default.toml                   # default profile
│   ├── deepseek.toml                  # example: DeepSeek profile
│   ├── claude.toml                    # example: Claude profile
│   └── active-profile.txt             # tracks which profile is active
├── workspace/                         # tools sandboxed here; default cwd
├── logs/{task_id}.jsonl               # one append-only log per task
├── skills/{skill_id}.yaml             # learned skills (Draft → Active → Deprecated)
├── skills/index.json                  # skill-tree summary
├── memory/{L0,L1,L2,L3,L4,L5}.jsonl   # six layered memory streams
├── secrets/<provider>.key             # per-provider API key, chmod 600
├── secrets/vault.json                 # named secret vault, chmod 600
├── agents/<id>.toml                   # ACP agent profiles
├── mcp/<id>.toml                      # MCP server profiles
├── plugins/                           # reserved
├── cache/                             # transient
└── cost.jsonl                         # per-turn cost events (input / cached / output / usd)
```

JSONL records are typed via `kind: "task" | "turn" | "end"`. The schema is stable; you can write parsers against it without worrying about breaking changes.

---

## Documentation map

| Topic | English | 中文 |
|-------|---------|------|
| Installation | [`docs/installation.md`](./docs/installation.md) | [`docs/zh/installation.md`](./docs/zh/installation.md) |
| Getting started | [`docs/getting-started.md`](./docs/getting-started.md) | [`docs/zh/getting-started.md`](./docs/zh/getting-started.md) |
| Usage reference | [`docs/usage.md`](./docs/usage.md) | [`docs/zh/usage.md`](./docs/zh/usage.md) |
| External ACP agents | [`docs/agents.md`](./docs/agents.md) | [`docs/zh/agents.md`](./docs/zh/agents.md) |
| MCP servers | [`docs/mcp.md`](./docs/mcp.md) | [`docs/zh/mcp.md`](./docs/zh/mcp.md) |
| Architecture overview | [`docs/architecture.md`](./docs/architecture.md) | [`docs/zh/architecture.md`](./docs/zh/architecture.md) |
| Contributing | [`docs/contributing.md`](./docs/contributing.md) | [`docs/zh/contributing.md`](./docs/zh/contributing.md) |

Live diagrams: [Architecture (EN)](https://develolin.github.io/EvoClawSite/architecture-en.html) · [中文](https://develolin.github.io/EvoClawSite/architecture-zh.html) · [Design (EN)](https://develolin.github.io/EvoClawSite/design-en.html) · [中文](https://develolin.github.io/EvoClawSite/design-zh.html)

---

## Roadmap

| Phase | Status     | Window     | Target |
|-------|------------|------------|--------|
| 1   — Skeleton             | ✓ shipped   | Week 1–2   | onboard + run + 4 tools + JSONL session |
| 2   — Loop closure         | ✓ shipped   | Week 3–4   | reflection + distillation + 7 tools |
| 3   — Token economy        | ✓ shipped   | Week 5     | 5 cache/compress techniques + budget engine |
| 4   — Skill tree           | ✓ shipped   | Week 6     | tree index + ACTIVE state + trigger search |
| 4.5 — ACP + MCP catalogs   | ✓ shipped   | Week 7     | Zed-style ACP + Anthropic MCP; 4 + 7 built-in profiles |
| 4.6 — Secret-redaction     | ✓ shipped   | Week 8     | Vault + Redactor + `secret` subcommand |
| 5   — Local Gateway core   | ✓ shipped   | Week 8–9   | HTTP daemon + WebChat + bearer-token allowlist + session isolation |
| 6   — Hardening (CI gates) | ✓ shipped   | Week 9     | doctor closure + mock-provider tests + LOC enforcer + `evo replay` + doc-sync check + GitHub Actions CI |
| 7   — Multi-channel        | ⏳ v0.6 plan | future     | Telegram / Slack / Discord plugins, Local Dashboard, trust-FSM auto-promote, group-mention enforcement |
| 8   — Deep hardening       | ⏳ v0.7 plan | future     | unshare-based sandbox + capability drop, OWASP scan in CI, 100-concurrent load test, performance baseline |

Phase 7 (Multi-channel) and Phase 8 (Deep hardening) are explicit future work — the Telegram / Slack plugins depend on external service tokens and a Tauri-based dashboard, and the kernel-level sandbox + load testing aren't blockers for solo-developer use of the runtime today. Everything in Phases 1–6 ships in v0.3.3.

---

## Quality gates

```bash
cargo build  --workspace
cargo test   --workspace --all-targets        # 153+ tests, all green
cargo clippy --workspace --all-targets -- -D warnings
./scripts/check.sh                            # LOC budget, prompt budget, tool count
```

If any of those four fail, the repo is broken — fix or revert before opening a PR.

---

## Versioning

The version of this code is recorded in [`./version`](./version). The site repo and the code repo keep their version files **in sync**; the value is identical in:

- `EvoClaw/version` (this repo)
- `EvoClawSite/version`

A version bump in one **must** be accompanied by the same bump in the other. Both currently read **`v0.3.3`**.

---

## What it is not

- **Not a SaaS platform.** Every byte of state stays on your machine. There is no telemetry endpoint.
- **Not a replacement for upstream coding CLIs.** When you want one of those, route the loop through ACP — they keep their full feature set, you keep the redaction barrier and the local logs.
- **Not a chat aggregator.** Channels are on the v0.6 horizon, but the heart of EvoClaw is the **self-evolving runtime**, not message routing.
- **Not a vector-DB-powered "second brain."** Memory is plain text + grep on purpose. We measured.

---

## Contributing

PRs welcome. Read [`docs/contributing.md`](./docs/contributing.md). The seven golden rules:

1. Keep the LOC budgets (`./scripts/check.sh`).
2. Keep the system prompt at exactly 6 lines.
3. Keep the **built-in** tool count ≤ 12. MCP-bridged tools don't count.
4. Tests stay green.
5. Clippy stays green with `-D warnings`.
6. No new dependencies without justification.
7. **No silent failures, and no path can leak a raw secret to the model.**

---

## License

MIT. See [`LICENSE`](./LICENSE).

---

> **The brief, in one sentence:** *Local-first agent runtime that learns from every task, proves its work in append-only logs, and never lets a secret you typed reach the model.*
