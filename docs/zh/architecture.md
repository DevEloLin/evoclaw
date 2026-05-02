# 架构总览

简短、贴代码的导览。规格说明书见 `prd/prd.md`；可点击的 Mermaid 图见
**[`prd/architecture.html`](../../prd/architecture.html)** 与
**[`prd/design.html`](../../prd/design.html)**。

---

## 分层

```
                 +----------------------------+
   L7 前端       | evo-cli   evo-gateway      |
                 +----------------------------+
   L6 Gateway    | evo-gateway HTTP daemon    |
                 +----------------------------+
   L5 Loop       | evo-core::ConversationRuntime
                 +----------------------------+
   L4 能力面     | Tool / Skill / Memory / Reflection / Distillation
                 +----------------------------+
   L3 路由 / Auth| evo-providers / (Auth Hub Phase 4.5)
                 +----------------------------+
   L2 策略       | evo-policy（Permission / Cost / Redact）
                 +----------------------------+
   L1 持久化     | JSONL / YAML / TOML / 文件系统
                 +----------------------------+
```

依赖**只能向下**。反向通信通过 audit-event bus（PRD §37，Phase 6+）。

---

## 各 crate 速览

| Crate | LOC | 主要 API |
|-------|-----|---------|
| `evo-policy` | 253 | `Permission`、`Decision`、`CostEngine`、`BudgetCfg`、`BudgetCheck`、`CostEvent`、`estimate_usd` |
| `evo-providers` | 507 | `Provider` trait、`Message`、`ChatRequest`、`StreamEvent`、`Usage`、`ToolFingerprint`、`ToolPayload`、`OpenAiCompatProvider` |
| `evo-tools` | 561 | `Tool` trait、`ToolRegistry`、`ToolContext`、`ToolError`、`smart_format`、7 工具 |
| `evo-core` | 2088 | `ConversationRuntime<P>`、`Session`、`Skill`、`SkillTree`、`Memory`、`ReflectionRecord`、`SummaryParser`、`compress_if_due` ... |
| `evo-cli` | 469 | binary `evo` |
| `evo-gateway` | 328 | binary `evo-gateway`、`serve()` + `GatewayConfig` |
| `evo-mock-provider` | 122 | `MockProvider`、`Turn`（仅 dev） |

总核心 ≤ 5000 LOC（PRD §45.2）。`./scripts/check.sh` 验证。

---

## Agent loop 60 行精简版

```rust
// crates/evo-core/src/runtime.rs (摘要)
loop {
    history.push(next_user_payload);

    compress_if_due(&mut history, turn, cfg);          // PRD §42.5
    cost.check_for_task(&task_id).await?;              // PRD §35

    let payload = fingerprint.payload_for_turn(turn,   // PRD §42.1
                                               registry.specs());
    let req = ChatRequest { /* messages, tools=payload, ... */ };

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

    summaries.ingest(&assistant_text);                 // PRD §42.4
    for call in &tool_calls {
        let result = registry.invoke(&ctx, &call.name, call.args).await?;
        tool_results.push(ToolResult { content: result, ... });
    }

    cost.record(&CostEvent { ... }).await;             // PRD §35
    session.append(&Turn(...)).await;                  // PRD §17.1

    if tool_calls.is_empty() { completed = true; break; }
    next_user_payload = compose_next(tool_results);
    turn += 1;
}

if completed && (memory_set || skills_set) {
    reflection_round(...).await;                      // PRD §11
}
session.append(&End { state, finished_at }).await;
```

完整实现 ~250 行；见 `crates/evo-core/src/runtime.rs`。

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

token_budget regression 断言：

- 30 轮中工具指纹命中 27 次，强制 full 仅 3 次（0/10/20 轮）
- 200 次 ingest 后 history block ≤ 1500 字符
- 30 条 message 压缩至少省 50%
- 合成负载下 cache_hit_rate ≥ 60%

---

## 自主进化闭环

```
任务进 -> 规划 -> 工具调用 -> 观察 -> 反思（JSON）
                                          |
                                          v
                                Memory L3（已脱敏）
                                          |
                                          v
                              蒸馏（PROMPTS §5）
                                          |
                                          v
                                Skill DRAFT YAML
                                          |
                                          v
                                Skill Tree 重建索引
                                          |
              +---------------------------+
              v
          下次类似任务 -> Plan 通过 trigger 命中 skill -> 步数减少
```

Skill 老化遵循 `evo-core::Skill` 中的 EWMA 状态机（PRD §32）：

```
Draft -- 沙箱通过 --> Candidate -- 真实成功 ≥3 --> Active
                                                      |
                                                      v
                                                   Degraded -- 5 次失败 --> Deprecated
```

---

## 存储 schema

| 文件 | 格式 | 写者 |
|------|------|------|
| `~/.evoclaw/config.toml` | TOML | 用户 |
| `~/.evoclaw/logs/{id}.jsonl` | JSONL | `evo_core::Session` |
| `~/.evoclaw/skills/{id}.yaml` | JSON-as-YAML | `evo_core::Skill` |
| `~/.evoclaw/skills/index.json` | JSON | `evo_core::SkillTree` |
| `~/.evoclaw/memory/{L*}.jsonl` | JSONL | `evo_core::Memory` |
| `~/.evoclaw/cost.jsonl` | JSONL | `evo_policy::CostEngine` |

所有 schema 在 `prd/prd.md §17`，自 v0.4 起稳定。

---

## 设计决策的理由

| 决策 | 理由 |
|------|------|
| Rust workspace、≤5K LOC core | 单一静态 binary，无运行时依赖；PRD §45 |
| `Provider` trait + 前缀路由 | 一个 mod 同时支持 DeepSeek / Kimi / Qwen / Ollama ... |
| Memory = JSONL + grep，不上向量库 | 向量在检索时要花 prompt token；针对类型化记忆层的子串匹配不会 |
| 6 行 system prompt | 长 system prompt 每轮都要付费；6 行装得下行动原则 |
| 10 工具上限 | 40+ 工具的注册表既是维护负担也是 prompt 成本负担 —— 想扩展请走 MCP |
| 一任务一文件（JSONL） | append-only、可回放、无 schema 迁移 |
| Gateway 仅本地 | Cookie / profile / 凭证永不出本机（PRD §13） |

---

## 深度阅读入口

- **规格**：`prd/prd.md` — 2,300 行、47 节、每条约束都编号
- **图**：`prd/architecture.html`、`prd/design.html` — Mermaid，dark mode
- **计划**：`prd/plan/development-plan.md` — 阶段任务清单
- **提示词**：`prd/plan/prompts.md` — 全部模型侧模板
- **代码**：从 `crates/evo-core/src/runtime.rs` 起，沿 `use` 链跟下去
