# Architecture overview

A code-grounded tour of how EvoClaw fits together. For the canonical
spec see `prd/prd.md`; for clickable Mermaid diagrams open
**[`prd/architecture.html`](../prd/architecture.html)** and
**[`prd/design.html`](../prd/design.html)**.

---

## Layered view

```
   ┌──────────────────────────────────────────────────────────────┐
   │ L7 Frontends      evoclaw / evo (REPL & subcmds)             │
   │                   evo-gateway HTTP daemon (optional)         │
   ├──────────────────────────────────────────────────────────────┤
   │ L6 Wizard / CLI   evo-cli::onboard, evo-cli::mcp_tools       │
   ├──────────────────────────────────────────────────────────────┤
   │ L5 Agent Loop     evo-core::ConversationRuntime              │
   │                   reflection · distillation · skill upsert   │
   ├──────────────────────────────────────────────────────────────┤
   │ L4 Capability     evo-tools (Tool trait, ToolRegistry)       │
   │                   evo-core::Memory / Skill / SkillTree       │
   ├──────────────────────────────────────────────────────────────┤
   │ L3 Routing        evo-providers (OpenAI-compat / Anthropic / │
   │                   Copilot OAuth / ACP)                       │
   │                   evo-acp-client · evo-mcp-client            │
   │                   evo-stdio-rpc (shared JSON-RPC over stdio) │
   ├──────────────────────────────────────────────────────────────┤
   │ L2 Policy         evo-policy::Permission / CostEngine /      │
   │                   Redactor + Vault (PRD §13.4)               │
   ├──────────────────────────────────────────────────────────────┤
   │ L1 Persistence    JSONL (logs, memory, cost)                 │
   │                   YAML (skills) · TOML (config, agents, mcp) │
   │                   JSON (vault) · Filesystem layout           │
   └──────────────────────────────────────────────────────────────┘
```

Dependencies flow only **downward**. Reverse signalling happens through the audit-event bus (PRD §37, Phase 6+).

---

## Crates at a glance

| Crate | Role | Public API surface |
|-------|------|--------------------|
| `evo-policy` (~470 LOC) | Permissions, budget, **redaction barrier** | `Permission`, `Decision`, `CostEngine`, `BudgetCfg`, `BudgetCheck`, `CostEvent`, `estimate_usd`, `Redactor`, `Vault`, `VaultEntry`, `SecretKind`, `classify_secret`, `fingerprint_of`, `default_vault_path` |
| `evo-providers` (~1200 LOC) | Model adapters | `Provider` trait, `Message`, `ChatRequest`, `StreamEvent`, `Usage`, `ToolFingerprint`, `ToolPayload`, `OpenAiCompatProvider`, `AnthropicProvider`, `CopilotProvider`, `AcpProvider` |
| `evo-tools` (~580 LOC) | Built-in tool inventory | `Tool` trait, `ToolRegistry`, `ToolContext`, `ToolError`, `smart_format`, 7 built-in tools |
| `evo-core` (~2100 LOC) | Agent loop & learning | `ConversationRuntime<P>`, `Session`, `Skill`, `SkillTree`, `Memory`, `ReflectionRecord`, `SummaryParser`, `compress_if_due`, … |
| `evo-cli` (~1500 LOC) | Two binaries + REPL | `entry()`, `onboard::*`, `mcp_tools::*` |
| `evo-acp-client` (~220 LOC) | Zed ACP client + 4-agent catalog | `AcpClient`, `AgentProfile`, `AgentConfig`, `CATALOG`, `find_agent`, `save_agent`, `load_agent`, `list_agents`, `remove_agent` |
| `evo-mcp-client` (~270 LOC) | Anthropic MCP client + 7-server catalog | `McpClient`, `ServerProfile`, `ServerConfig`, `CATALOG`, `find_server`, `save_server`, `load_server`, `list_servers`, `remove_server` |
| `evo-stdio-rpc` (~165 LOC) | Shared JSON-RPC 2.0 over child stdio | `StdioRpcClient`, `SpawnConfig`, `RpcError`, JSON-RPC envelopes |
| `evo-gateway` | Local HTTP daemon | binary `evo-gateway`, lib `serve()` + `GatewayConfig` |
| `evo-mock-provider` | Dev-only deterministic mock | `MockProvider`, `Turn` |

Total core ≤ 5500 LOC (PRD §45.2). Run `./scripts/check.sh` to verify per-crate caps and totals.

---

## Data flow on a single task

```
            ┌────────────────────────────────────────────────────────┐
 user input │ "diagnose why my SSH hangs intermittently"             │
            └───────────────────┬────────────────────────────────────┘
                                ▼
   ┌────────────────────────── Redactor ────────────────────────────┐
   │  Vault lookup → ${SECRET:NAME}                                 │
   │  Pattern catch-all → [REDACTED:<kind>:<fp>]                    │
   └───────────────────┬───────────────────────────────────────────┘
                       ▼
   ┌─── ConversationRuntime::run ───────────────────────────────────┐
   │  Session.append(Task)                                          │
   │  loop:                                                         │
   │    compress_if_due()                                           │
   │    cost.check_for_task()                                       │
   │    payload = fingerprint.payload_for_turn(turn, registry.specs)│
   │    provider.stream(req) → assistant_text + tool_calls + usage  │
   │    Redactor.scrub(assistant_text)                              │
   │    for call in tool_calls:                                     │
   │       Redactor.scrub_value(call.arguments)                     │
   │       result = registry.invoke(ctx, name, args)                │
   │       Redactor.scrub(result)                                   │
   │    cost.record(); session.append(Turn)                         │
   │    if no tool_calls: break                                     │
   │  reflection_round() → memory L3 + skill draft                  │
   │  session.append(End)                                           │
   └────────────────────────────────────────────────────────────────┘
```

Every text payload that enters the LLM call or the on-disk JSONL has been scrubbed first.

---

## The agent loop in 60 lines

```rust
// crates/evo-core/src/runtime.rs (sketch)
let user_input_safe = self.scrub(user_input);          // PRD §13.4
session.append(&Task { user_input: user_input_safe.clone(), ... }).await?;

let mut history = vec![system_msg];
let mut next_user_payload = compose_initial(user_input_safe);

while turn < max_turns {
    history.push(next_user_payload);
    compress_if_due(&mut history, turn, cfg);          // PRD §42.5
    cost.check_for_task(&task_id).await?;              // PRD §35

    let payload = fingerprint.payload_for_turn(turn, registry.specs());  // §42.1
    let req = ChatRequest { messages: history.clone(), tools: payload, .. };
    let mut stream = provider.stream(req).await?;

    while let Some(ev) = stream.next().await {
        match ev? {
            Delta(t)         => assistant_text.push_str(&t),
            ToolCallStart(c) => tool_calls.push(c),
            Usage(u)         => usage = Some(u),
            Done             => break,
            _ => {}
        }
    }

    let assistant_text_safe = self.scrub(&assistant_text);       // §13.4
    history.push(Message::assistant(assistant_text_safe));
    summaries.ingest(&assistant_text_safe);                      // §42.4

    for call in &tool_calls {
        let safe_args = self.scrub_value(&call.arguments);
        let result = registry.invoke(ctx, &call.name, call.arguments.clone()).await?;
        let safe_result = self.scrub(&result);                   // §13.4
        tool_results.push(ToolResult { content: safe_result, ... });
    }

    cost.record(...); session.append(Turn(...));
    if tool_calls.is_empty() { completed = true; break; }
    next_user_payload = compose_next(tool_results);
    turn += 1;
}

if completed && (memory_set || skills_set) {
    reflection_round(&task_id, &final_text_safe, &user_input_safe).await;
}
session.append(End { state, finished_at }).await?;
```

The full implementation is ~290 lines; see `crates/evo-core/src/runtime.rs`.

---

## Five token-economy techniques (PRD §42)

All five are wired and unit-tested in `crates/evo-core/tests/token_budget.rs`.

| # | Technique | Module | Token saving |
|---|-----------|--------|--------------|
| 1 | Tool-schema fingerprint + 10-turn reset | `evo-providers::ToolFingerprint` | 25–40% prompt |
| 2 | Incremental messages + ephemeral cache | `evo-providers::Message::cache_control` | 60–70% effective spend |
| 3 | `smart_format` head+tail truncation | `evo-tools::smart_format` | 50–70% observation |
| 4 | `<summary>` working memory | `evo-core::SummaryParser` | 80–90% history |
| 5 | Tag-level periodic compression (every 5 turns) | `evo-core::compress_if_due` | 50–60% long-task |

The token_budget regression test asserts:

- fingerprint reuse fires 27/30 turns (3 forced full sends at turns 0, 10, 20)
- summary block ≤ 1500 chars after 200 ingests
- 30-msg compression saves ≥ 50%
- cache_hit_rate ≥ 60% on synthetic load

---

## Self-evolving loop

```
Task IN → Plan → Tool calls → Observe → Reflect (JSON)
                                          │
                                          ▼
                                Memory L3 (redacted)
                                          │
                                          ▼
                              Distill (PROMPTS §5)
                                          │
                                          ▼
                                Skill DRAFT YAML
                                          │
                                          ▼
                                Skill Tree reindex
                                          │
              ┌───────────────────────────┘
              ▼
          Next task → Plan finds skill via triggers → fewer turns
```

Skills age through the EWMA state machine in `evo-core::Skill` (PRD §32):

```
Draft -- sandbox pass --> Candidate -- ≥3 wins, score≥0.7 --> Active
                                                                │
                                                                ▼
                                                            Degraded -- ≥5 fails or score<0.3 --> Deprecated
```

---

## Secret-redaction barrier (PRD §13.4)

The `evo-policy::redact` module owns this entire boundary.

```
┌────────── Vault (~/.evoclaw/secrets/vault.json, chmod 600) ─────────┐
│ {"version":1,"entries":[                                            │
│    {"name":"github_pat","value":"ghp_…","kind":"github_pat",        │
│     "fingerprint":"b4824fbd","created_at":"2026-05-02T17:52:00Z"}]} │
└──────────────────┬──────────────────────────────────────────────────┘
                   │  load
                   ▼
            ┌──────────────────┐  scrub(text) →  ┌─────────────────────┐
            │   Redactor       │ ──────────────▶ │  (scrubbed, hits)   │
            │   (immutable     │ scrub_value(v)  └─────────────────────┘
            │    snapshot)     │
            └──────┬───────────┘
                   │
                   │ wired into ConversationRuntime via with_redactor()
                   ▼
            ┌────────────────────────────────────────────────────────┐
            │  Inputs scrubbed:                                      │
            │    • user_input on entry                               │
            │    • assistant_text on stream end                      │
            │    • each ToolCall.arguments                           │
            │    • each tool result                                  │
            │  Outputs are stable: re-scrubbing is idempotent.       │
            └────────────────────────────────────────────────────────┘
```

Pattern fallback (no vault entry needed):

```
classify_secret("ghp_…")        → SecretKind::GitHubPat
classify_secret("sk-ant-api03…")→ SecretKind::Anthropic
classify_secret("eyJ….….….…")  → SecretKind::Jwt
classify_secret("AKIAIOSFOD…")  → SecretKind::AwsKeyId
classify_secret("K9f4Lq2pZ…")   → SecretKind::GenericHighEntropy   (Shannon ≥ 4 b/c, len≥32)
classify_secret("the quick fox") → SecretKind::Unknown              (passes through)
```

`fingerprint_of(secret)` returns the first 8 hex chars of `sha256(secret)`.

---

## ACP integration (Zed Agent Client Protocol)

```
   ┌─────────────────────┐
   │ ConversationRuntime │
   └──────────┬──────────┘
              │ Provider::stream(req)
              ▼
   ┌─────────────────────┐    JSON-RPC 2.0 / stdio    ┌───────────────────┐
   │  AcpProvider        │ ────────────────────────▶ │ external CLI      │
   │  (evo-providers/acp)│                           │ claude --acp      │
   │                     │ ◀──────────────────────── │ codex --acp       │
   └─────────────────────┘    session/prompt result  │ cursor-agent --acp│
                                                     │ gh copilot --acp  │
                                                     └───────────────────┘
```

The CLI handles its own auth (no key inside `~/.evoclaw/secrets/`) and runs its own tool-use loop. EvoClaw is a black-box turn responder for it.

Catalog and config files live in `crates-ext/evo-acp-client/src/lib.rs`. Each user-added agent persists to `~/.evoclaw/agents/<id>.toml`.

---

## MCP integration (Anthropic Model Context Protocol)

```
   ┌─────────────────────┐
   │ ToolRegistry        │
   │  + 7 built-ins      │
   │  + N MCP wrappers   │ ◀── install_all() walks ~/.evoclaw/mcp/*.toml
   └──────────┬──────────┘     spawns each server, lists tools, registers
              │ Tool::run
              ▼
   ┌─────────────────────┐    JSON-RPC 2.0 / stdio    ┌───────────────────┐
   │ McpToolWrapper      │ ────────────────────────▶ │ MCP server child  │
   │ name = mcp__<srv>__ │   tools/call               │ (npx / uvx /…)    │
   │  <tool>             │ ◀──────────────────────── │ filesystem/github/│
   └─────────────────────┘                            │ fetch/time/...    │
                                                      └───────────────────┘
```

Auth env vars (`GITHUB_PERSONAL_ACCESS_TOKEN`, `BRAVE_API_KEY`, …) are captured at `mcp add` time and passed to the spawned child via the `env` field of the `SpawnConfig`. The model never sees the env values.

---

## Storage schemas

| File | Format | Owner | Schema example |
|------|--------|-------|----------------|
| `~/.evoclaw/config.toml` | TOML | user-edited | `[model]`, `[budget]`, `[security]` |
| `~/.evoclaw/logs/{id}.jsonl` | JSONL | `evo_core::Session` | `{kind:"task" \| "turn" \| "end", …}` |
| `~/.evoclaw/skills/{id}.yaml` | JSON-as-YAML | `evo_core::Skill` | `{id, kind, state, score, version, …}` |
| `~/.evoclaw/skills/index.json` | JSON | `evo_core::SkillTree` | `{nodes:[…]}` |
| `~/.evoclaw/memory/{L*}.jsonl` | JSONL | `evo_core::Memory` | `{layer, content, source, confidence, tags, ts}` |
| `~/.evoclaw/cost.jsonl` | JSONL | `evo_policy::CostEngine` | `{ts, task_id, model, input_tokens, cached_tokens, output_tokens, usd}` |
| `~/.evoclaw/secrets/<provider>.key` | text | wizard | raw API key, chmod 600 |
| `~/.evoclaw/secrets/vault.json` | JSON | `evo_policy::Vault` | `{version, entries:[{name, value, kind, fingerprint, created_at}]}`, chmod 600 |
| `~/.evoclaw/agents/<id>.toml` | TOML | `evo_acp_client` | `{id, name, command, args, env, installed_at}` |
| `~/.evoclaw/mcp/<id>.toml` | TOML | `evo_mcp_client` | `{id, name, command, args, env, installed_at}` |

All schemas are versioned in `prd/prd.md §17` and stable from v0.4.

---

## Lifecycle: how subprocesses are reaped

ACP and MCP children are spawned by `StdioRpcClient::spawn` with `kill_on_drop(true)`. When a `ConversationRuntime` finishes a task and the registry drops:

1. `Arc<McpClient>` reference count hits zero
2. `StdioRpcClient::Inner::child` is dropped
3. tokio's `Child::drop` honours `kill_on_drop` → SIGKILL is sent
4. The OS reaps the zombie

Belt-and-braces: `McpClient::shutdown()` and `AcpClient::shutdown()` issue `start_kill` + `wait` explicitly when the caller wants deterministic cleanup. The `agent test` and `mcp test` subcommands use this path.

---

## Why these design choices

| Choice | Rationale |
|--------|-----------|
| Rust workspace, ≤5500 LOC core | Single static binary, no runtime deps; PRD §45 |
| `Provider` trait + prefix routing | One mod covers DeepSeek, Kimi, Qwen, Ollama, …; ACP slots in as another impl |
| Memory = JSONL + grep, no vector DB | Vectors cost prompt tokens at retrieval time; substring matching against typed memory layers doesn't |
| 6-line system prompt | Long system prompts cost tokens every turn; principles fit in 6 lines |
| 10-tool cap on built-ins | A 40-tool registry is a maintenance burden and a prompt-cost burden — extend via MCP instead |
| One file per turn (JSONL) | Append-only, replayable, no migration needed |
| Local-only Gateway | Cookies, profiles, secrets never leave the machine (PRD §13) |
| **Vault + Pattern redactor** | Two layers. Named secrets become deterministic placeholders; unnamed ones still get caught by entropy/prefix heuristics. Idempotent so the same text can pass through multiple times safely. |
| `kill_on_drop(true)` on stdio children | Prevents subprocess leaks when registries drop |

---

## Where to dig deeper

- **Spec**: `prd/prd.md`
- **Diagrams**: `prd/architecture.html`, `prd/design.html` — Mermaid, dark-mode
- **Plan**: `prd/plan/development-plan.md` — phase-by-phase tasks
- **Prompts**: `prd/plan/prompts.md` — every model-facing template
- **Code starting points**:
  - `crates/evo-core/src/runtime.rs` — the loop
  - `crates/evo-policy/src/redact.rs` — the redaction barrier
  - `crates/evo-cli/src/lib.rs` — REPL + subcommands + banner
  - `crates/evo-cli/src/mcp_tools.rs` — MCP→Tool bridge
  - `crates/evo-providers/src/acp.rs` — ACP→Provider bridge
