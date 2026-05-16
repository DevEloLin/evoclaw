# 快速上手 — 从零跑通你的第一个 Agent

如果还没装好，先看 **[安装](installation.md)** 再回来。

本教程 ~5 分钟，覆盖：

1. 跑第一个任务
2. 看 EvoClaw 学习（一个 Skill 诞生）
3. 跑类似任务，看 Skill 起作用
4. 检查发生了什么（replay / doctor / memory）
5. （可选）打开 WebChat

---

## 0. 体检

```bash
evoclaw --help     # 列出子命令，无子命令时进交互
evoclaw doctor     # 应该看到 api_key: set (len=...)
```

`evo` 是 `evoclaw` 的 3 字母短别名——两个 binary 完全等价。

---

## 1. 进入交互式 Shell

直接输入项目名（**不带任何子命令**），跟 `claude` / `codex` 一样：

```bash
evoclaw
```

进入：

```
───────────────────────────────────────────────────────────────────
  \\  ▄   ▄  //                  Quick start
    ▄███████▄                    ──────────────────────────
    █       █                    /help    list all commands
    █ ▀▀ ▀▀ █                    /login   configure auth
    ▀█▄▄▄▄▄█▀                    /doctor  health check
      ▄▄ ▄▄                      /skill   browse skills
  //  ██ ██  \\
                                  Status
  EvoClaw  v1.0.1-beta.2                 ──────────────────────────
  self-evolving agent runtime     auth    ✓ ready
                                  model   deepseek-chat

  deepseek  ·  deepseek-chat
  ~/.evoclaw                      Ctrl-D to exit  ·  /help

  ✓ ready  secrets file: ~/.evoclaw/secrets/deepseek.key

───────────────────────────────────────────────────────────────────

─ input ───────────────────────────────────────────────────────────
  ▷ Type your message and press Enter to send  ·  /help for commands
───────────────────────────────────────────────────────────────────
shortcuts: Tab /cmd  ·  ↑↓/Ctrl-P/N 历史  ·  Ctrl-R 搜索  ·  Ctrl-C 退出
```

直接用自然语言描述任务：

```
evoclaw> 列出 workspace 下所有 Cargo.toml，写一行总结到
         ~/.evoclaw/workspace/cargo-toml-summary.txt
```

简化输出：

```
→ running... session log: /Users/you/.evoclaw/logs/task-20260502T143012.481.jsonl

=== final (4 turns) ===
Wrote 7 paths to cargo-toml-summary.txt. Roots: evoclaw, my-other-repo, ...
```

Agent 自己决定用 `list_dir` + `read_file` + `write_file`。还没有 Skill 可用，所以它**自主探索**。

---

## 2. 看 Skill 诞生

任务结束时，EvoClaw 会跑**反思回合**（PRD §11）。它问模型"刚刚干了什么、有没有可复用的经验？"，把答案落到：

- **Memory L3** — 自由文本（`~/.evoclaw/memory/L3.jsonl`）
- **Skill DRAFT** — 结构化 YAML（`~/.evoclaw/skills/skill-*.yaml`）

```bash
evo skill list
```

期望：

```
ID                       STATE      SCORE VER      NAME
skill-20260502T1430      DRAFT       0.50 v1       enumerate cargo manifests
```

技能处于 **DRAFT** 状态，分数 0.50。它需要沙箱通过 + 真实成功 ≥3 次才会升到 **ACTIVE**（PRD §32 EWMA 规则）。

```bash
evo skill show skill-20260502T1430
```

---

## 3. 跑一个类似的任务

```bash
evo run "找出 workspace 中所有 Cargo manifest 并归纳一下"
```

预期：planner 通过关键词 / trigger 命中已有 skill（PRD §11.5）并复用它。第二次跑 token 应该降 ~30%，原因：

1. 工具 schema 指纹命中，发送 `Tools: still active` 短串（PRD §42.1）
2. 第二轮 prompt 段落命中缓存（PRD §42.2）
3. `<summary>` 协议把整段 assistant 历史替换成 30 字摘要（PRD §42.4）

---

## 4. 检查发生了什么

### 4a. 回放 session

```bash
evo replay
```

挑选最新 JSONL，pretty-print：

```
== replay /Users/you/.evoclaw/logs/task-...jsonl (12 records) ==

[TASK] task-20260502T143012.481
  input : 列出 workspace 下所有 Cargo.toml ...
  start : 2026-05-02T14:30:12.481Z

[TURN 0] 1 tool_calls
  summary: scanning workspace
  ✓ list_dir args={"path":"."} → d crates ... | d crates-ext ...
  usage: in=620 cached=400 (65% hit) out=44

[TURN 1] 1 tool_calls
  summary: read 3 manifests
  ✓ read_file args={"path":"crates/evo-core/Cargo.toml"} → ...

[END] COMPLETED @ 2026-05-02T14:30:39.910Z
```

### 4b. 成本 / token 统计

```bash
evo doctor-of tokens
```

输出：

```
== evo doctor tokens ==
metric              7d         30d
events               4           4
input_tokens     2,480       2,480
cached_tokens    1,612       1,612
output_tokens      244         244
cache_hit         65.00%      65.00%
usd_total       0.0078$     0.0078$

budget: per_task ≤ $0.50, per_day ≤ $5.00 (soft) / $20.00 (hard), per_month ≤ $100
```

DeepSeek 上每任务几分钱很正常。如果超过 per-task 上限，EvoClaw 会停下并报 `Budget(...)` 错误（PRD §35）。

### 4c. 闭环审计

```bash
evo doctor-of closure
```

校验每份 session JSONL 都有 `TaskRecord` + ≥1 `TurnRecord` + `EndRecord`（PRD §39 第 1/4 行）。

### 4d. Memory 搜索

```bash
evo memory search "cargo manifest"
```

返回 L1 / L2 / L3 中匹配的记录。Memory 是**故意做的**纯文本 + grep — 向量在检索时要花 prompt token；针对类型化记忆层的子串匹配不会。

---

## 5. 看技能树成长

跑过 5–10 个不同任务后：

```bash
evo skill tree
```

输出：

```
== skill tree (8 nodes, 2 active) ==

[Diagnostic]
  skill-...     ACTIVE     score=0.86  diagnose ssh hang   (triggers: ssh, diagnose)
  skill-...     CANDIDATE  score=0.62  docker healthcheck  (triggers: docker)

[Sop]
  skill-...     DRAFT      score=0.50  enumerate cargo manifests (triggers: cargo, manifest)
  ...
```

状态自动转移（PRD §32）：

- **DRAFT** → **CANDIDATE**：沙箱通过
- **CANDIDATE** → **ACTIVE**：真实成功 ≥3 且 score ≥ 0.7
- **ACTIVE** → **DEGRADED**：score 跌到 0.7 以下
- **DEGRADED** → **DEPRECATED**：连续失败 5 次或 score < 0.3

只有 **ACTIVE** 会被 planner 默认加载，**CANDIDATE** 需要显式启用。

---

## 6. （可选）打开本地 Gateway WebChat

```bash
evo gateway --bind 127.0.0.1:7878 --token mychat
```

浏览器打开 <http://127.0.0.1:7878>，Bearer token 填 `mychat`（或你传的值）。发条消息，会走和 CLI 完全一样的 `ConversationRuntime`，含完整 reflection / cost / memory。

`Ctrl-C` 停止。Gateway 默认只监听 `127.0.0.1`，cookie / API key 永远不出本机。

---

## 7. 接下来

- **[使用参考](usage.md)** — 全部 CLI 命令、配置项、环境变量
- **[架构总览](architecture.md)** — 每个 crate 干什么，agent loop 怎么跑
- **[贡献指南](contributing.md)** — 改 bug / 加工具 / 改提示词
- `prd/prd.md` — 2,300 行规格说明书
- `prd/plan/development-plan.md` — 阶段化任务清单
- `prd/plan/prompts.md` — 全部提示词模板
