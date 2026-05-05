# 🕷 EvoClaw — 自主进化的个人 AI 智能体

<p align="center">
  <img src="../image/EvoClaw.png" alt="EvoClaw — 自主进化的个人 AI 智能体" width="700">
</p>



<p align="center">
  <b>学习。记忆。进化。</b>
</p>

<p align="center">
  <a href="../../LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue?style=for-the-badge" alt="MIT 开源"></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-1.80%2B-orange?style=for-the-badge&logo=rust" alt="Rust 1.80+"></a>
  <img src="https://img.shields.io/badge/tests-153%20passing-4c1?style=for-the-badge" alt="153 测试全绿">
  <img src="https://img.shields.io/badge/data-stays_local-green?style=for-the-badge" alt="本地优先">
  <img src="https://img.shields.io/badge/secrets-never_leave_machine-red?style=for-the-badge" alt="密钥不出本机">
</p>

<p align="center">
  <b>🌐 官网</b>：<a href="https://develolin.github.io/EvoClawSite/zh.html">develolin.github.io/EvoClawSite</a> ·
  <b>📦 版本</b>：<a href="../../version"><code>v1.0.0-beta.1</code></a> ·
  <b>🇬🇧 English README</b>：<a href="../../README.md">README.md</a>
</p>

---

**EvoClaw** 是一个能在你自己的机器上跑的 *自主进化个人 AI 助手*。它会规划、调用工具、观察结果，循环直到任务完成 —— 然后安静地把刚学到的东西**蒸馏成一份 YAML Skill** 写到磁盘上，下次类似任务就更短、更便宜、更准。Rust 写的**单一二进制**（`evoclaw`，短别名 `evo`），加一个可选的本地 HTTP daemon。核心约 8K LOC。两个二进制。零遥测。

核心循环很短：*输入任务 → 规划、调用工具、观察、再规划、收尾 → 反思 → 写 Skill。* 其他的一切 —— 缓存指纹、密钥隔离、EWMA 评分、JSONL 复盘、ACP 桥、MCP 桥、本地保险柜 —— 都是在让这个循环**值得你信任，敢让它在你真实的机器上无人值守地跑**。

---

## EvoClaw 的差异化优势

我们觉得有意义的几个设计选择：

| 能力 | EvoClaw 提供 |
|------|--------------|
| **单一静态二进制** | Rust 1.80+，核心约 8K LOC，零运行时依赖，冷启动 < 100 ms |
| **自主进化的 Skill Tree** | 五态机 EWMA（Draft → Candidate → Active → Degraded → Deprecated）；**Active** Skill 自动加载到下一轮 planner |
| **密钥隔离屏障** | 命名 **Vault** + 模式兜底（`sk-*`、`ghp_*`、`AKIA…`、JWT、32 位以上高熵串）；在 runtime **6 个边界点**幂等脱敏 |
| **Token 经济** | 5 招叠加（schema 指纹、ephemeral cache、summary 工作记忆、头尾截断、周期压缩）；**长任务花费 ≈ 1/3** 朴素方案；每招都有单元测试 |
| **ACP 标准互通** | 内置 **7 个上游代理**：Claude Code、Codex、Cursor、GitHub Copilot、Gemini CLI、Aider、Qwen Code（阿里通义灵码） |
| **MCP 标准互通** | 内置 **7 个服务器**：filesystem、GitHub、fetch、time、Brave Search、Postgres、Slack —— 也支持自带 |
| **Provider 目录** | **17 个模型厂商** 一键接入：9 国内（DeepSeek、Kimi、Qwen、豆包、智谱、百度千帆、MiniMax、阶跃星辰、腾讯混元）+ 8 国际（OpenAI、Anthropic、Gemini、Copilot、Mistral、Groq、Together、Fireworks、OpenRouter）+ 3 本地（Ollama、vLLM、llama.cpp）+ 自定义 |
| **登录后选模型** | 向导自动拉 `/v1/models` 让你挑，或一键回车用目录默认 |
| **Append-only JSONL 审计** | 一任务一份日志；`evoclaw replay <log>` 任意会话都能回放；`evoclaw doctor closure` 走查每份日志、断言 13 行闭环矩阵 |
| **system prompt 6 行硬约束** | `scripts/check.sh` CI 卡住；每个工具描述上限 80 字符 |
| **P0..P8 权限阶梯** | 全序约束；默认上限 **P1**；远程渠道无视 config 硬封顶 **P4** |
| **三级预算引擎** | per-task hard stop、per-day soft warn + hard cap（4×）、per-month hard cap；`doctor-of tokens` 看缓存命中率 |
| **CLI 体验** | 无子命令直接进 REPL；Tab 自动补全、历史导航（↑↓ / Ctrl-P/N）、反向搜索（Ctrl-R）、vim 风格行编辑（Ctrl+A/E/K/U/W）；斜杠命令全套（`/agent /mcp /secret /skill /memory /tokens /closure /replay /doctor /logout /config /status /model /profile /usage /clear /exit`） |
| **零遥测** | 没有分析 SDK、没有远程 ping、没有"匿名使用统计"开关其实在偷偷上报 |
| **本地优先** | 所有状态在 `~/.evoclaw/`：vault、agents/*.toml、mcp/*.toml、JSONL 日志、学到的 Skill |

---

## 这是什么？

EvoClaw 是**一个二进制** —— `evoclaw`（短别名 `evo`）—— 加一个可选的本地 HTTP 守护进程（`evo-gateway`）。

你用自然语言描述要做的事。EvoClaw 规划、调用工具、观察结果、再规划，一直跑到任务完成。然后它会安静地对自己做一次复盘，把刚学到的东西**蒸馏成一份 YAML Skill** 写到磁盘。下次你再问类似的问题时，那份 Skill 就会被命中，整轮会更短、更便宜、更准。

这就是核心循环。其他的一切 —— 缓存指纹、密钥隔离、EWMA 评分、JSONL 复盘、ACP 桥、MCP 桥、本地保险柜 —— 都是在让这个循环**值得你信任，敢让它在你真实的机器上无人值守地跑**。

---

## 架构总览

![EvoClaw 架构](../image/architecture-zh1.png)

> 七层泳道，自上而下：前端 → Gateway/向导 → Agent Loop → 能力面 → 路由与适配器 → 策略/成本/脱敏 → 持久化。每层内部横向铺开；跨层依赖永远只朝下。在线图：<https://develolin.github.io/EvoClawSite/architecture-zh.html>。

每次任务结束都跑一次反思 + 蒸馏，让循环自动闭合：

![EvoClaw 设计 — 任务生命周期与 Skill EWMA](../image/design.png)

> Task FSM（RECEIVED → PLANNING → TOOL_EXECUTING → OBSERVING → REFLECTING → DISTILLING → COMPLETED → ARCHIVED）和 Skill FSM（Draft → Candidate → Active → Degraded → Deprecated）共同驱动自主进化。在线设计图：<https://develolin.github.io/EvoClawSite/design-zh.html>。

---

## EvoClaw 与众不同的三件事

### 1. **它会学。** 不是"丢进向量库然后保佑"那种学，是结构化、可审计、可删除的学。

每次任务结束后，EvoClaw 会主动多打一次模型 —— 这是**反思回合（reflection round）**，问的是：*用户当时到底想干什么？什么有效？什么没生效？哪些动作是可复用的？* 答案被写成一份 YAML Skill。Skill 走五态机：**Draft → Candidate → Active → Degraded → Deprecated**，由 EWMA 分数驱动。Active 状态的 skill 会被下次任务的 planner 自动加载；进入 Deprecated 的会归档但不会删 —— 审计轨迹比磁盘空间重要。

任何 skill 你都可以单独看（`evoclaw skill show <id>`），可以 grep（`evoclaw memory search "..."`），可以重建整棵树（`evoclaw skill tree`）。没有一处是黑盒。没有一字节会发往别处。

### 2. **密钥永远不会从本机泄漏到模型。**

你和模型之间夹着**两层 redaction 屏障**：

- **保险柜层。** 跑 `evoclaw secret add github_pat ghp_…`，原值落到 `~/.evoclaw/secrets/vault.json`（Unix chmod 600），从此但凡含有它的字符串在去往模型的路上都会被替换成 `${SECRET:github_pat}`。
- **模式兜底层。** 即使你没注册过，常见凭据形状 —— `sk-*`、`sk-ant-*`、`ghp_*` 系列、`AKIA…`、JWT、以及任意 32 位以上的高熵串 —— 都会被识别并改写为 `[REDACTED:<kind>:<8 位指纹>]`，**在文本进入 prompt、JSONL 会话日志、记忆层之前**完成替换。

两层都是**幂等**的。指纹用 SHA-256 前 8 位，同一密钥两次扫描得到同一指纹，便于跨日志串联但永远不暴露原值。**没有"先写明文，后续 GC 时再脱敏"的窗口** —— 替换发生在 runtime 边界，进门时就完成。

### 3. **它很省钱 —— 是可量化的省。**

朴素的 agent harness 跑长任务很容易花到 10 倍冤枉钱。EvoClaw 在 runtime 层叠了 5 个技巧，每一个都有单元测试压住：

| 技巧 | 干什么 | Token 节省 |
|------|--------|-----------|
| 工具 schema 哈希指纹 | 工具表每 10 轮发完整版一次；中间的 9 轮只发 16 字节哈希 + "still active" 标记 | 25–40% prompt |
| Ephemeral cache 标记 | system prompt 标 persistent，最近若干 assistant 轮标 ephemeral；模型 API 都缓存 | 60–70% 实际花费 |
| `<summary>` working memory | 每条 assistant 回复结尾必带 30 字符 `<summary>…</summary>`；下轮把整条 assistant 消息替换成这条 summary | 80–90% 历史字节 |
| 头+尾观测截断 | 工具输出对人仍可读，但中间被 `…` 替换后再回送给模型 | 50–70% 观测 token |
| 周期性 tag-level 压缩 | 每 5 轮，把更早的 `<observation>` 块压成一行摘要 | 长任务 50–60% |

5 个都已落地、有回归测试。`evoclaw doctor-of tokens` 可以看你过去 7 天的缓存命中率。

---

## 安装

```bash
git clone https://github.com/DevEloLin/evoclaw && cd evoclaw
cargo build --workspace --release
./target/release/evoclaw            # 首次启动会跑交互式向导
```

完整说明：[`installation.md`](./installation.md) · English: [`../installation.md`](../installation.md)

---

## 快速上手

直接敲 **`evoclaw`** —— 不带子命令 —— 进入交互式 shell：

```
$ evoclaw

──────────────────────────────────────────────────────────────────────────────────
  \\  ▄   ▄  //        快速开始
    ▄███████▄           ──────────────────────────
    █       █           /help    查看所有命令
    █ ▀▀ ▀▀ █           /login   配置认证
    ▀█▄▄▄▄▄█▀           /doctor  健康检查
      ▄▄ ▄▄             /skill   浏览技能
  //  ██ ██  \\

  EvoClaw  v1.0.0-beta.1       状态
  自主进化智能体         ──────────────────────────
  运行时                 auth    ✓ 就绪
                         model   deepseek-chat
  deepseek  ·  deepseek-chat
  ~/.evoclaw             Ctrl-D 退出  ·  /help 查看命令
──────────────────────────────────────────────────────────────────────────────────

─ input ─────────────────────────────────────────────────────────────────────────
  ▷ 输入消息并回车发送  ·  /help 查看命令
─────────────────────────────────────────────────────────────────────────────────
shortcuts: Tab /cmd  ·  ↑↓/Ctrl-P/N 历史  ·  Ctrl-R 搜索  ·  Ctrl-C 退出
```

输入问题并回车 —— 消息出现在上方对话区，助手以流式方式在单行状态栏输出：

```
─ You · 12:00:05 ────────────────────────────────────────────────────────────────
排查为什么我连生产机的 SSH 偶尔卡住
─────────────────────────────────────────────────────────────────────────────────

─────────────────────────────────────────────────────── (streaming)
EvoClaw · 流式输出 · 3.1s
最常见的原因是未配置 TCP keepalive...
─────────────────────────────────────────────────────────────────────────────────

─ input ─────────────────────────────────────────────────────────────────────────
  ▷ 输入消息并回车发送  ·  /help 查看命令
─────────────────────────────────────────────────────────────────────────────────
shortcuts: Tab /cmd  ·  ↑↓/Ctrl-P/N 历史  ·  Ctrl-R 搜索  ·  Ctrl-C 退出
```

完成后完整回复滚入固定输入框上方的历史区：

```
─ EvoClaw · deepseek · 12:00:05 ─────────────────────────────────────────────────
最常见的原因是未配置 TCP keepalive。

## 修复方法
在 ~/.ssh/config 中添加：

  ServerAliveInterval 60
  ServerAliveCountMax 3

─────────────────────────────────────────────────────────────────────────────────

─ input ─────────────────────────────────────────────────────────────────────────
  ▷ 输入消息并回车发送  ·  /help 查看命令
─────────────────────────────────────────────────────────────────────────────────
shortcuts: Tab /cmd  ·  ↑↓/Ctrl-P/N 历史  ·  Ctrl-R 搜索  ·  Ctrl-C 退出
```

完整教程：[`getting-started.md`](./getting-started.md) · English: [`../getting-started.md`](../getting-started.md)

---

## 密钥隔离屏障

```bash
# 注册一个密钥 —— 原值永不离开本机
$ evoclaw secret add github_pat ghp_abc123…
✓ stored 'github_pat' (kind=github_pat, fingerprint=b4824fbd) at …/vault.json
  the model will never see the raw value — only ${SECRET:github_pat}

# 列出条目 —— 只显示元信息，永远不打印原值
$ evoclaw secret list
NAME                     KIND           FINGER     CREATED
github_pat               github_pat     b4824fbd   2026-05-02 17:52

# 用样本字符串验证
$ evoclaw secret test "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjMifQ.fakesigvalue"
output : Authorization: Bearer [REDACTED:jwt:9d25a2da]
hits   : 1 substitution(s)
```

保险柜文件结构（`~/.evoclaw/secrets/vault.json`，Unix chmod 600）：

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

## 多配置文件

通过配置文件在不同模型 provider 和配置之间无缝切换：

```bash
# 列出可用的配置文件
$ evoclaw
evoclaw> /profile list
Available profiles:
  * default        Default configuration
    deepseek       DeepSeek Chat (https://api.deepseek.com/v1)
    claude         Claude 3.5 Sonnet (Anthropic)
    gemini         Google Gemini Flash

# 从模板创建新的配置文件
evoclaw> /profile add myopenai --template openai
✓ created profile 'myopenai' from template 'openai'

# 切换到其他配置文件
evoclaw> /profile switch deepseek
✓ switched to profile 'deepseek'
✓ provider: deepseek
✓ model: deepseek-chat

# 在 /status 中查看当前配置文件
evoclaw> /status
╭─ Provider & Model ─────────────────────────────────╮
  Active Profile...... deepseek
  Vendor.............. DeepSeek (Cloud)
  Model............... deepseek-chat
╰────────────────────────────────────────────────────╯
```

**内置模板**：`deepseek`、`openai`、`claude`、`gemini`、`ollama`

每个配置文件是独立的 `~/.evoclaw/profiles/<name>.toml`，拥有各自的 provider、model、budget 和 security 设置。无需手动编辑配置文件即可即时切换。

---

## 内置工具（已交付 12）

| # | 名字 | 权限 | 干什么 |
|---|------|------|--------|
| 1 | `read_file` | P0 | 按行号读文件；agent 必须先读后写。 |
| 2 | `write_file` | P1 | workspace 内写入（不会跑出 `~/.evoclaw/workspace/`）。 |
| 3 | `patch_file` | P1 | 唯一替换某段子串。匹配数 ≠ 1 直接拒绝。 |
| 4 | `list_dir` | P0 | 列目录；自动跳过 `node_modules / .git / target / .venv / dist / build / __pycache__`。 |
| 5 | `run_shell` | P2 | `sh -c`，默认 30 秒超时，输出截到 8K。 |
| 6 | `web_fetch` | P3 | 仅 HTTPS。Cookie 在响应进 LLM 上下文前剥除。 |
| 7 | `ask_user` | P0 | 参数歧义或动作高危时**必须**调用。 |
| 8 | `browser_navigate` | P3 | 无头浏览器跳转 URL；返回页面标题和正文。 |
| 9 | `browser_screenshot` | P3 | 对当前页面截图保存为 PNG；返回文件路径。 |
| 10 | `browser_click` | P3 | 点击 CSS selector 匹配的元素。 |
| 11 | `browser_type` | P3 | 向 CSS selector 匹配的表单字段输入文本。 |
| 12 | `browser_eval` | P3 | 在浏览器页面执行 JavaScript；返回结果。 |

权限阶梯 **P0**（只读）→ **P8**（生产环境）。默认上限 P1；通过远程渠道进入的消息硬封顶 P4，无论 config 怎么设。浏览器工具（8–12）需要宿主机安装 Chrome / Chromium，且要求 P3 权限。

**想要更多工具？** 优先考虑接入 MCP 服务器，而非继续增加内置工具。

---

## 标准互通：ACP + MCP

### ACP — Agent Client Protocol（把循环交出去）

想让上游 coding CLI 来跑 agent loop？在 `~/.evoclaw/config.toml` 设 `provider = "acp:claude"`（或 `acp:codex` / `acp:cursor` / `acp:copilot`）。EvoClaw 会把上游 CLI 拉成子进程、用 JSON-RPC 把你的 prompt 喂过去、回收最终文本。**上游 CLI 自己处理认证 —— EvoClaw 永远不接触它的凭据。**

```bash
evoclaw agent catalog          # 看七个内置 agent profile
evoclaw agent add claude       # 落盘 ~/.evoclaw/agents/claude.toml
evoclaw agent test claude      # spawn `claude --acp` + ACP initialize 握手
```

完整指南：[`agents.md`](./agents.md) · English: [`../agents.md`](../agents.md)

### MCP — Model Context Protocol（自带工具进来）

跑任意一个标准 Anthropic MCP 服务器，它的工具就会出现在 EvoClaw 的注册表里，命名 `mcp__<server>__<tool>`。鉴权 env 在 `add` 时落盘，模型完全看不见。

```bash
export GITHUB_PERSONAL_ACCESS_TOKEN=ghp_xxx
evoclaw mcp add github
evoclaw mcp test github        # spawn + initialize + tools/list
```

完整指南：[`mcp.md`](./mcp.md) · English: [`../mcp.md`](../mcp.md)

---

## 文件系统约定

```
~/.evoclaw/
├── config.toml                        # 当前激活的配置（链接到激活的 profile）
├── profiles/                          # 多配置文件系统
│   ├── default.toml                   # 默认 profile
│   ├── deepseek.toml                  # 示例：DeepSeek profile
│   ├── claude.toml                    # 示例：Claude profile
│   └── active-profile.txt             # 记录当前激活的 profile
├── workspace/                         # 工具沙箱；默认 cwd
├── logs/{task_id}.jsonl               # 一任务一份 append-only 日志
├── skills/{skill_id}.yaml             # 学会的 skill（Draft → Active → Deprecated）
├── skills/index.json                  # skill tree 索引
├── memory/{L0,L1,L2,L3,L4,L5}.jsonl   # 6 层记忆流
├── secrets/<provider>.key             # 每 provider 一个 API key, chmod 600
├── secrets/vault.json                 # 命名密钥保险柜，chmod 600
├── agents/<id>.toml                   # ACP agent profile
├── mcp/<id>.toml                      # MCP server profile
├── plugins/                           # 预留
├── cache/                             # 临时
└── cost.jsonl                         # 每轮 cost event（input / cached / output / usd）
```

JSONL 记录用 `kind: "task" | "turn" | "end"` 标类型。schema 稳定，可放心写解析器。

---

## 文档地图

| 主题 | 中文 | English |
|------|------|---------|
| 安装 | [`installation.md`](./installation.md) | [`../installation.md`](../installation.md) |
| 快速上手 | [`getting-started.md`](./getting-started.md) | [`../getting-started.md`](../getting-started.md) |
| 使用参考 | [`usage.md`](./usage.md) | [`../usage.md`](../usage.md) |
| 外部 ACP 代理 | [`agents.md`](./agents.md) | [`../agents.md`](../agents.md) |
| MCP 服务器 | [`mcp.md`](./mcp.md) | [`../mcp.md`](../mcp.md) |
| 架构总览 | [`architecture.md`](./architecture.md) | [`../architecture.md`](../architecture.md) |
| 贡献指南 | [`contributing.md`](./contributing.md) | [`../contributing.md`](../contributing.md) |

在线图：[架构（中文）](https://develolin.github.io/EvoClawSite/architecture-zh.html) · [English](https://develolin.github.io/EvoClawSite/architecture-en.html) · [设计（中文）](https://develolin.github.io/EvoClawSite/design-zh.html) · [English](https://develolin.github.io/EvoClawSite/design-en.html)

---

## Roadmap

| Phase | 状态 | 周期 | 目标 |
|-------|------|------|------|
| 1   — Skeleton              | ✓ 已交付      | Week 1–2   | onboard + run + 4 工具 + JSONL session |
| 2   — 学习闭环              | ✓ 已交付      | Week 3–4   | 反思 + 蒸馏 + 7 工具 |
| 3   — Token 经济学          | ✓ 已交付      | Week 5     | 5 招省 token + 预算引擎 |
| 4   — Skill tree            | ✓ 已交付      | Week 6     | 树索引 + ACTIVE 状态 + trigger 检索 |
| 4.5 — ACP + MCP catalog     | ✓ 已交付      | Week 7     | Zed-style ACP + Anthropic MCP；4 + 7 内置 profile |
| 4.6 — 密钥隔离屏障          | ✓ 已交付      | Week 8     | Vault + Redactor + `secret` 子命令 |
| 5   — 本地 Gateway 核心     | ✓ 已交付      | Week 8–9   | HTTP daemon + WebChat + bearer 鉴权 + session 隔离 |
| 6   — Hardening (CI 门)     | ✓ 已交付      | Week 9     | doctor closure + mock-provider 集测 + LOC 守门 + `evo replay` + 文档同步 + GitHub Actions CI |
| 7   — 多渠道接入            | ⏳ v0.6 计划 | 待定       | Telegram / Slack / Discord 插件、Local Dashboard、信任晋级 FSM、群聊 mention 强制 |
| 8   — 深度加固              | ⏳ v0.7 计划 | 待定       | unshare 沙箱 + capability drop、CI OWASP 扫描、100 并发负载测试、性能基线 |

Phase 7（多渠道）和 Phase 8（深度加固）显式标记为后续工作 —— Telegram / Slack 插件依赖外部服务 token，Local Dashboard 需要 Tauri 重壳，内核级 sandbox 与负载测试也不是个人开发者使用 runtime 的阻塞项。Phases 1–6 全部在 v1.0.0-beta.1 中交付。

---

## 质量门

```bash
cargo build  --workspace
cargo test   --workspace --all-targets        # 153+ 测试，全绿
cargo clippy --workspace --all-targets -- -D warnings
./scripts/check.sh                            # LOC 预算 + prompt 预算 + 工具数
```

任一不绿则视为代码"未交付状态"，请先修复或回滚。

---

## 版本

代码版本记录在 [`../../version`](../../version)。**站点 repo 与代码 repo 的 version 文件必须严格一致**：

- `EvoClaw/version` (本仓库)
- `EvoClawSite/version`

任一处升版必须同步升另一处。当前两边都读作 **`v1.0.0-beta.1`**。

---

## 它不是

- **不是 SaaS 平台。** 所有状态留在你机器上。没有遥测端点。
- **不是上游 coding CLI 的替代。** 想用它们时通过 ACP 接入即可 —— 它们保留全部功能，你保留 redaction 屏障与本地日志。
- **不是聊天聚合器。** 渠道在 v0.6 路线图上，但 EvoClaw 的核心是**自主进化 runtime**，不是消息路由。
- **不是向量数据库式的"第二大脑"。** 记忆是有意做的纯文本 + grep。我们量过。

---

## 贡献

欢迎 PR。先读 [`contributing.md`](./contributing.md)。七条金律：

1. 守住 LOC 预算（`./scripts/check.sh`）。
2. system prompt 永远 6 行。
3. **内置**工具数 ≤ 12。MCP 桥接的工具不计入。
4. 测试全绿。
5. clippy `-D warnings` 全绿。
6. 没有充分理由不加新依赖。
7. **没有 silent failure，没有任何路径能让原始密钥到达模型。**

---

## License

MIT. 见仓库根的 [`LICENSE`](../../LICENSE)。

---

> **一句话总结：** *本地优先的 agent runtime，每次任务都在学，工作过程在 append-only 日志里可以证明，你输入的密钥永远不会到模型。*
