# 架构总览

简短、贴代码的导览。规格说明书见 `prd/prd.md`；可点击的 Mermaid 图见
**[`prd/architecture.html`](../../prd/architecture.html)** 与
**[`prd/design.html`](../../prd/design.html)**。

---

## 分层视图

```
   ┌──────────────────────────────────────────────────────────────┐
   │ L7 前端        evoclaw / evo（REPL 与子命令）                 │
   │                evo-gateway HTTP daemon（可选）                │
   ├──────────────────────────────────────────────────────────────┤
   │ L6 向导 / CLI  evo-cli::onboard, evo-cli::mcp_tools          │
   ├──────────────────────────────────────────────────────────────┤
   │ L5 Agent Loop  evo-core::ConversationRuntime                 │
   │                反思 · 蒸馏 · Skill 写盘                      │
   ├──────────────────────────────────────────────────────────────┤
   │ L4 能力面      evo-tools（Tool trait, ToolRegistry）          │
   │                evo-core::Memory / Skill / SkillTree          │
   ├──────────────────────────────────────────────────────────────┤
   │ L3 路由        evo-providers（OpenAI-compat / Anthropic /     │
   │                Copilot OAuth / ACP）                         │
   │                evo-acp-client · evo-mcp-client               │
   │                evo-stdio-rpc（共享 JSON-RPC over stdio）      │
   ├──────────────────────────────────────────────────────────────┤
   │ L2 策略        evo-policy::Permission / CostEngine /         │
   │                Redactor + Vault（PRD §13.4）                 │
   ├──────────────────────────────────────────────────────────────┤
   │ L1 持久化      JSONL（日志、记忆、成本）                      │
   │                YAML（skills）· TOML（config, agents, mcp）   │
   │                JSON（vault）· 文件系统布局                   │
   └──────────────────────────────────────────────────────────────┘
```

依赖**只能向下**。反向通信通过 audit-event bus（PRD §37，Phase 6+）。

---

## 各 crate 速览

| Crate | 角色 | 公开 API |
|-------|------|---------|
| `evo-policy`（~470 LOC） | 权限、预算、**隔离屏障** | `Permission`、`Decision`、`CostEngine`、`BudgetCfg`、`BudgetCheck`、`CostEvent`、`estimate_usd`、`Redactor`、`Vault`、`VaultEntry`、`SecretKind`、`classify_secret`、`fingerprint_of`、`default_vault_path` |
| `evo-providers`（~1200 LOC） | 模型适配器 | `Provider` trait、`Message`、`ChatRequest`、`StreamEvent`、`Usage`、`ToolFingerprint`、`ToolPayload`、`OpenAiCompatProvider`、`AnthropicProvider`、`CopilotProvider`、`AcpProvider` |
| `evo-tools`（~580 LOC） | 内置工具清单 | `Tool` trait、`ToolRegistry`、`ToolContext`、`ToolError`、`smart_format`、7 个内置工具 |
| `evo-core`（~2100 LOC） | Agent 循环与学习 | `ConversationRuntime<P>`、`Session`、`Skill`、`SkillTree`、`Memory`、`ReflectionRecord`、`SummaryParser`、`compress_if_due`… |
| `evo-cli`（~1500 LOC） | 两个 binary + REPL | `entry()`、`onboard::*`、`mcp_tools::*` |
| `evo-acp-client`（~220 LOC） | Zed ACP 客户端 + 4 个 agent 目录 | `AcpClient`、`AgentProfile`、`AgentConfig`、`CATALOG`、`find_agent`、`save_agent`、`load_agent`、`list_agents`、`remove_agent` |
| `evo-mcp-client`（~270 LOC） | Anthropic MCP 客户端 + 7 个服务器目录 | `McpClient`、`ServerProfile`、`ServerConfig`、`CATALOG`、`find_server`、`save_server`、`load_server`、`list_servers`、`remove_server` |
| `evo-stdio-rpc`（~165 LOC） | 共享 JSON-RPC 2.0 over child stdio | `StdioRpcClient`、`SpawnConfig`、`RpcError`、JSON-RPC 信封 |
| `evo-gateway` | 本地 HTTP daemon | binary `evo-gateway`、lib `serve()` + `GatewayConfig` |
| `evo-mock-provider` | 仅 dev 用的确定性 mock | `MockProvider`、`Turn` |

总核心 ≤ 5500 LOC（PRD §45.2）。运行 `./scripts/check.sh` 验证各 crate 上限和总量。

---

## 单次任务的数据流

```
            ┌────────────────────────────────────────────────────────┐
 用户输入    │ "排查为什么我的 SSH 偶尔卡住"                          │
            └───────────────────┬────────────────────────────────────┘
                                ▼
   ┌────────────────────────── Redactor ────────────────────────────┐
   │  Vault 查找 → ${SECRET:NAME}                                   │
   │  模式兜底   → [REDACTED:<kind>:<fp>]                           │
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

每一条进入 LLM 调用或写入磁盘 JSONL 的文本都已事先脱敏。

---

## Agent loop 60 行精简版

```rust
// crates/evo-core/src/runtime.rs（摘要）
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

完整实现约 290 行；见 `crates/evo-core/src/runtime.rs`。

---

## 五大 Token 经济招式（PRD §42）

全部已接入并被 `crates/evo-core/tests/token_budget.rs` 回归测试覆盖。

| # | 招式 | 模块 | 节省 token |
|---|------|------|-----------|
| 1 | 工具 schema 哈希指纹 + 10 轮重置 | `evo-providers::ToolFingerprint` | 25–40% prompt |
| 2 | 增量 messages + ephemeral cache | `evo-providers::Message::cache_control` | 实际花费 60–70% |
| 3 | `smart_format` 头+尾截断 | `evo-tools::smart_format` | observation 50–70% |
| 4 | `<summary>` 工作记忆 | `evo-core::SummaryParser` | history 80–90% |
| 5 | 每 5 轮 tag 压缩 | `evo-core::compress_if_due` | 长任务 50–60% |

token_budget 回归断言：

- 30 轮中指纹命中 27 次，强制 full send 仅 3 次（0/10/20 轮）
- 200 次 ingest 后 summary block ≤ 1500 字符
- 30 条 message 压缩至少省 50%
- 合成负载下 cache_hit_rate ≥ 60%

---

## 自主进化闭环

```
任务进 → 规划 → 工具调用 → 观察 → 反思（JSON）
                                          │
                                          ▼
                                Memory L3（已脱敏）
                                          │
                                          ▼
                              蒸馏（PROMPTS §5）
                                          │
                                          ▼
                                Skill DRAFT YAML
                                          │
                                          ▼
                                Skill Tree 重建索引
                                          │
              ┌───────────────────────────┘
              ▼
          下次类似任务 → Plan 通过 trigger 命中 skill → 步数减少
```

Skill 老化遵循 `evo-core::Skill` 中的 EWMA 状态机（PRD §32）：

```
Draft -- 沙箱通过 --> Candidate -- ≥3 次真实成功, score≥0.7 --> Active
                                                                  │
                                                                  ▼
                                                              Degraded -- ≥5 次失败 or score<0.3 --> Deprecated
```

---

## 密钥隔离屏障（PRD §13.4）

`evo-policy::redact` 模块负责整个屏障边界。

```
┌────────── Vault（~/.evoclaw/secrets/vault.json，chmod 600）─────────┐
│ {"version":1,"entries":[                                            │
│    {"name":"github_pat","value":"ghp_…","kind":"github_pat",        │
│     "fingerprint":"b4824fbd","created_at":"2026-05-02T17:52:00Z"}]} │
└──────────────────┬──────────────────────────────────────────────────┘
                   │  加载
                   ▼
            ┌──────────────────┐  scrub(text) →  ┌─────────────────────┐
            │   Redactor       │ ──────────────▶ │  (已脱敏, 命中数)   │
            │  （不可变快照）   │ scrub_value(v)  └─────────────────────┘
            └──────┬───────────┘
                   │
                   │ 通过 with_redactor() 接入 ConversationRuntime
                   ▼
            ┌────────────────────────────────────────────────────────┐
            │  以下输入均经脱敏：                                    │
            │    • 进入时的 user_input                               │
            │    • stream 结束时的 assistant_text                    │
            │    • 每个 ToolCall.arguments                           │
            │    • 每条工具结果                                      │
            │  输出是稳定的：重复脱敏是幂等的。                      │
            └────────────────────────────────────────────────────────┘
```

模式兜底（无需 vault 条目）：

```
classify_secret("ghp_…")        → SecretKind::GitHubPat
classify_secret("sk-ant-api03…")→ SecretKind::Anthropic
classify_secret("eyJ….….….…")  → SecretKind::Jwt
classify_secret("AKIAIOSFOD…")  → SecretKind::AwsKeyId
classify_secret("K9f4Lq2pZ…")   → SecretKind::GenericHighEntropy   (Shannon ≥ 4 b/c，len≥32)
classify_secret("the quick fox") → SecretKind::Unknown              （原样通过）
```

`fingerprint_of(secret)` 返回 `sha256(secret)` 的前 8 位十六进制字符。

---

## ACP 集成（Zed Agent Client Protocol）

```
   ┌─────────────────────┐
   │ ConversationRuntime │
   └──────────┬──────────┘
              │ Provider::stream(req)
              ▼
   ┌─────────────────────┐    JSON-RPC 2.0 / stdio    ┌───────────────────┐
   │  AcpProvider        │ ────────────────────────▶ │ 外部 CLI          │
   │  (evo-providers/acp)│                           │ claude --acp      │
   │                     │ ◀──────────────────────── │ codex --acp       │
   └─────────────────────┘    session/prompt 结果    │ cursor-agent --acp│
                                                     │ gh copilot --acp  │
                                                     └───────────────────┘
```

CLI 自行处理认证（`~/.evoclaw/secrets/` 中没有其密钥），并运行自己的工具调用循环。EvoClaw 对它而言是一个黑盒的回合响应器。

目录和配置文件位于 `crates-ext/evo-acp-client/src/lib.rs`。每个用户添加的 agent 持久化到 `~/.evoclaw/agents/<id>.toml`。

---

## MCP 集成（Anthropic Model Context Protocol）

```
   ┌─────────────────────┐
   │ ToolRegistry        │
   │  + 7 个内置工具     │
   │  + N 个 MCP 包装器  │ ◀── install_all() 遍历 ~/.evoclaw/mcp/*.toml
   └──────────┬──────────┘     为每个服务器 spawn 进程，列出工具，注册
              │ Tool::run
              ▼
   ┌─────────────────────┐    JSON-RPC 2.0 / stdio    ┌───────────────────┐
   │ McpToolWrapper      │ ────────────────────────▶ │ MCP server 子进程 │
   │ name = mcp__<srv>__ │   tools/call               │ (npx / uvx /…)    │
   │  <tool>             │ ◀──────────────────────── │ filesystem/github/│
   └─────────────────────┘                            │ fetch/time/...    │
                                                      └───────────────────┘
```

认证 env 变量（`GITHUB_PERSONAL_ACCESS_TOKEN`、`BRAVE_API_KEY` 等）在 `mcp add` 时捕获，通过 `SpawnConfig` 的 `env` 字段传给子进程。模型永远看不到这些 env 值。

---

## 存储 schema

| 文件 | 格式 | 负责方 | Schema 示例 |
|------|------|--------|------------|
| `~/.evoclaw/config.toml` | TOML | 用户编辑 | `[model]`、`[budget]`、`[security]` |
| `~/.evoclaw/logs/{id}.jsonl` | JSONL | `evo_core::Session` | `{kind:"task" \| "turn" \| "end", …}` |
| `~/.evoclaw/skills/{id}.yaml` | JSON-as-YAML | `evo_core::Skill` | `{id, kind, state, score, version, …}` |
| `~/.evoclaw/skills/index.json` | JSON | `evo_core::SkillTree` | `{nodes:[…]}` |
| `~/.evoclaw/memory/{L*}.jsonl` | JSONL | `evo_core::Memory` | `{layer, content, source, confidence, tags, ts}` |
| `~/.evoclaw/cost.jsonl` | JSONL | `evo_policy::CostEngine` | `{ts, task_id, model, input_tokens, cached_tokens, output_tokens, usd}` |
| `~/.evoclaw/secrets/<provider>.key` | 文本 | 向导 | 原始 API key，chmod 600 |
| `~/.evoclaw/secrets/vault.json` | JSON | `evo_policy::Vault` | `{version, entries:[{name, value, kind, fingerprint, created_at}]}`，chmod 600 |
| `~/.evoclaw/agents/<id>.toml` | TOML | `evo_acp_client` | `{id, name, command, args, env, installed_at}` |
| `~/.evoclaw/mcp/<id>.toml` | TOML | `evo_mcp_client` | `{id, name, command, args, env, installed_at}` |

所有 schema 均在 `prd/prd.md §17` 中有版本记录，自 v0.4 起稳定。

---

## 子进程回收的生命周期

ACP 和 MCP 子进程由 `StdioRpcClient::spawn` 以 `kill_on_drop(true)` 方式启动。当 `ConversationRuntime` 完成一个任务且 registry 被 drop 时：

1. `Arc<McpClient>` 引用计数归零
2. `StdioRpcClient::Inner::child` 被 drop
3. tokio 的 `Child::drop` 响应 `kill_on_drop` → 发送 SIGKILL
4. OS 回收僵尸进程

双重保险：`McpClient::shutdown()` 和 `AcpClient::shutdown()` 在调用方需要确定性清理时会显式执行 `start_kill` + `wait`。`agent test` 和 `mcp test` 子命令走的就是这条路径。

---

## 设计决策的理由

| 决策 | 理由 |
|------|------|
| Rust workspace，≤5500 LOC core | 单一静态 binary，无运行时依赖；PRD §45 |
| `Provider` trait + 前缀路由 | 一个 mod 同时支持 DeepSeek / Kimi / Qwen / Ollama…；ACP 作为另一个 impl 接入 |
| Memory = JSONL + grep，不上向量库 | 向量在检索时要花 prompt token；针对类型化记忆层的子串匹配不会 |
| 6 行 system prompt | 长 system prompt 每轮都要付费；6 行装得下行动原则 |
| 内置工具上限 10 | 40 工具的注册表既是维护负担也是 prompt 成本负担 —— 想扩展请走 MCP |
| 一任务一文件（JSONL） | append-only、可回放、无 schema 迁移 |
| Gateway 仅本地 | Cookie / profile / 凭证永不出本机（PRD §13） |
| **Vault + 模式 redactor** | 双层。命名密钥变为确定性占位符；未命名的仍被熵值/前缀启发式捕获。幂等，同一文本多次通过也安全。 |
| stdio 子进程 `kill_on_drop(true)` | 防止 registry drop 时子进程泄漏 |

---

## 深度阅读入口

- **规格**：`prd/prd.md`
- **图**：`prd/architecture.html`、`prd/design.html` — Mermaid，dark mode
- **计划**：`prd/plan/development-plan.md` — 阶段任务清单
- **提示词**：`prd/plan/prompts.md` — 全部模型侧模板
- **代码起点**：
  - `crates/evo-core/src/runtime.rs` — 主循环
  - `crates/evo-policy/src/redact.rs` — 隔离屏障
  - `crates/evo-cli/src/lib.rs` — REPL + 子命令 + banner
  - `crates/evo-cli/src/mcp_tools.rs` — MCP→Tool 桥接
  - `crates/evo-providers/src/acp.rs` — ACP→Provider 桥接
