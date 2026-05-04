# 使用参考

完整的 CLI 命令、配置项、环境变量、斜线命令。

---

## 两个 binary，同一份 CLI

EvoClaw 同时安装两个**完全等价**的 binary：

- **`evoclaw`** — 长格式，项目名（脚本与文档建议用）
- **`evo`** — 3 字母短别名（更短；行为 100% 相同）

两者都调用 `evo_cli::entry()`，挑你喜欢的。

---

## 交互式 shell vs 子命令模式

**不带子命令**直接运行 → 进入交互式 REPL，跟 `claude` / `codex` 一样：

```bash
evoclaw                    # 进交互
# 等价于：
evoclaw shell
```

**带子命令** → 一次性、非交互执行：

```bash
evoclaw run "..."          # 跑一次任务后退出
evoclaw doctor             # 健康检查后退出
evoclaw skill tree         # 渲染 skill tree 后退出
```

第一次运行 `evoclaw` 且无配置时，自动进入 onboarding 向导。之后直接显示 banner。

---

## CLI 子命令

```
evoclaw [subcommand] [args]
```

| 子命令 | 用途 |
|--------|------|
| *（无）* / `shell` | 进入交互式 REPL |
| `onboard` | 首次安装向导，写 `~/.evoclaw/config.toml` |
| `login` | 切换 provider 或重新输入 API key（重跑向导） |
| `run <input>` | 单次任务；完整 agent loop + reflection + skill 落盘 |
| `doctor` | 健康检查：config / model / fs / api_key |
| `doctor-of tokens` | 7 天 / 30 天 cost 与 cache 统计 |
| `doctor-of closure` | 审计最近 session JSONL（对照 PRD §39 闭环矩阵） |
| `replay [path]` | pretty-print session JSONL（默认最近一份） |
| `skill list` | 列出所有 skill 的 state / score / version |
| `skill show <id>` | 打印某个 skill 的完整 YAML |
| `skill tree` | 重建并按类型分组渲染 skill tree |
| `memory search <query> [--limit N]` | grep memory L1/L2/L3 |
| `agent ...` | 外部 ACP 代理（Claude / Codex / Cursor / Copilot）— 见 [agents.md](agents.md) |
| `mcp ...` | MCP 服务器（filesystem / github / fetch / time / brave / postgres / slack）— 见 [mcp.md](mcp.md) |
| `secret ...` | 本地密钥保险柜，原值永不送给模型（PRD §13.4） |
| `gateway [--bind X --token Y]` | 启动 `evo-gateway` HTTP daemon |

### 交互式 REPL 斜线命令

进入 `evoclaw>` 后可以输入：

| 斜线命令 | 等价子命令 | 备注 |
|---------|-----------|------|
| `/help` 或 `/?` | （无） | 打印斜线命令清单 |
| `/login` | `evoclaw login` | 切换 provider |
| `/agent [list/catalog/add/remove/test <id>]` | `evoclaw agent ...` | ACP agent 管理 |
| `/mcp [list/catalog/add/remove/test <id>]` | `evoclaw mcp ...` | MCP 服务器管理 |
| `/secret [list/add/remove/test ...]` | `evoclaw secret ...` | 密钥保险柜管理 |
| `/skill [list/tree/show <id>]` | `evoclaw skill ...` | Skill 管理 |
| `/memory <query>` | `evoclaw memory search <query>` | 记忆 grep |
| `/tokens` | `evoclaw doctor-of tokens` | 成本统计 |
| `/closure` | `evoclaw doctor-of closure` | 闭环审计 |
| `/replay [path]` | `evoclaw replay [path]` | 会话回放 |
| `/doctor` | `evoclaw doctor` | 健康检查 |
| `/clear` | （无） | 清屏 |
| `/exit`、`/quit`、`/q` | （无） | 退出（Ctrl-D / EOF 也行） |

不以 `/` 开头的输入会被当作任务执行，等同于 `evoclaw run "..."`。

---

## `evoclaw run` — 完整说明

```bash
evoclaw run "用自然语言描述要干的事"
```

Runtime 内部按序执行：

1. **通过 redactor 脱敏用户输入**（PRD §13.4）—— vault 中的值变为 `${SECRET:NAME}`，模式匹配到的变为 `[REDACTED:<kind>:<fp>]`。原始字符串永远不再进入处理管线。
2. **构建 system prompt**（PROMPTS §1，严格 6 行，以 `persistent` 缓存）。
3. **拼接 user 消息**：带 `<history>` 块（最近 40 条 `<summary>` 记录）+ `<user_input>`。
4. **追加 `TaskRecord`** 到 `~/.evoclaw/logs/` 下对应的任务 JSONL 日志。
5. **循环**直到模型不再发出工具调用或达到 `max_turns`：
   - 每 5 轮做 tag-level 压缩。
   - 预算前置检查。
   - 用 `ToolFingerprint` 短路构建 `ChatRequest`。
   - stream 模型事件，收集工具调用与 assistant 文本。
   - 通过 redactor 脱敏 assistant 文本和工具参数。
   - 通过 `ToolRegistry::invoke` 派发工具调用。内置工具内联执行；MCP 桥接的工具以 `mcp__<server>__<tool>` 形式调用对应子进程。
   - 脱敏每条工具结果，追加 `TurnRecord`，将工具结果回送给模型。
6. **反思回合**（PROMPTS §4 → `ReflectionRecord`）。
7. **蒸馏**（PROMPTS §5 → `Skill` DRAFT，写到 `~/.evoclaw/skills/`）。
8. **写 Memory L3**（每个任务一条记录）。
9. **EndRecord**（`COMPLETED` 或 `FAILED`）。

硬上限：

- `max_turns = 25`
- `max_tokens = 1024` 每次响应
- `temperature = 0.2`

通过 `RuntimeConfig` 可调；CLI 默认值是经过权衡的。

---

## `evoclaw secret` — 本地密钥保险柜（PRD §13.4）

| 子命令 | 作用 |
|--------|------|
| `add <name> [value]` | 写入 / 覆盖一条记录。不带 value 时 CLI 会提示输入；`--stdin` 从 stdin 一行读入。 |
| `list` | 列出条目 — 仅显示 `name`、`kind`、指纹、`created_at`，**永远不打印原值**。 |
| `remove <name>` | 按 name 删除。 |
| `test <text>` | 用 redactor 跑一遍样本字符串并打印替换后的输出。 |

REPL 内同样可用：`/secret list`、`/secret add NAME VALUE`（或 `/secret add NAME` 触发提示输入）、`/secret remove NAME`、`/secret test TEXT…`。

保险柜文件 `~/.evoclaw/secrets/vault.json`（Unix chmod 600）：

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

即使**没有**保险柜条目，下列形状也会被模式兜底自动替换为 `[REDACTED:<kind>:<fp>]`：

| 模式 | 示例 | 替换为 |
|------|------|--------|
| `sk-ant-*` | Anthropic API key | `[REDACTED:anthropic_key:<fp>]` |
| `sk-*`（≥20 字符） | OpenAI API key | `[REDACTED:openai_key:<fp>]` |
| `ghp_/gho_/ghu_/ghs_/ghr_*` | GitHub PAT | `[REDACTED:github_pat:<fp>]` |
| `AKIA*`（20 位字母数字） | AWS key id | `[REDACTED:aws_key_id:<fp>]` |
| `eyJ*.*.*` | JWT | `[REDACTED:jwt:<fp>]` |
| 任意 32 位以上、Shannon 熵 ≥ 4 bits/char 的字符串 | 未指定 | `[REDACTED:high_entropy:<fp>]` |

`<fp>` 是稳定的 SHA-256 前 8 位 —— 同一密钥始终得到同一指纹，便于跨日志串联而永不暴露原值。

---

## `evoclaw agent` — 外部 ACP 代理

完整指南见 **[docs/agents.md](agents.md)**。快速参考：

```bash
evoclaw agent catalog          # 查看内置 agent 目录
evoclaw agent add claude       # 写入 ~/.evoclaw/agents/claude.toml
evoclaw agent test claude      # spawn `claude --acp` 并执行初始化握手
evoclaw agent list
evoclaw agent remove claude
```

要让循环通过 ACP agent 运行，在 `~/.evoclaw/config.toml` 中设置 `provider = "acp:<id>"`（向导的"外部 ACP agent"选项会自动完成这一步）。

---

## `evoclaw mcp` — MCP 服务器

完整指南见 **[docs/mcp.md](mcp.md)**。快速参考：

```bash
export GITHUB_PERSONAL_ACCESS_TOKEN=ghp_xxx   # 由 `add` 时捕获写入 profile
evoclaw mcp add github
evoclaw mcp test github                       # spawn + initialize + tools/list
evoclaw mcp list
```

每次执行 `evoclaw run`（以及 REPL 中每条 prompt），runtime 都会遍历 `~/.evoclaw/mcp/`，spawn 每个服务器，并将其广播的工具以 `mcp__<server>__<tool>` 形式注册到与内置工具共用的 `ToolRegistry` 中。

---

## `evoclaw replay [path]`

不带 path 时，EvoClaw 选取 `~/.evoclaw/logs/` 中最近修改的 `*.jsonl`。输出三段：

- `[TASK] <id>` — 任务记录（输入、来源、模型、started_at）
- `[TURN N]` — 每轮的摘要、工具调用、usage（含 cache-hit %）
- `[END]` — 终态（`COMPLETED` / `FAILED`）

由于每条文本在写入前都已脱敏，JSONL 日志可安全分享用于调试。

---

## `evoclaw skill <子命令>`

| 子命令 | 效果 |
|--------|------|
| `list` | 列出 `~/.evoclaw/skills/` 下所有 `*.yaml` 的表格视图 |
| `show <id>` | dump 单个 skill 的 YAML |
| `tree` | 重新扫描所有 skill，重建 `index.json`，按 `SkillKind` 分组渲染 |

Skill 状态机：

```
Draft → Candidate → Active → Degraded → Deprecated
        (沙箱通过)  (≥3 次成功)  (score<0.7)  (5 次失败 or score<0.3)
```

只有 `Active` 状态的 skill 会自动加载到 planner。`Candidate` 可手动启用。

---

## `evoclaw memory search`

```bash
evoclaw memory search "ssh timeout" --limit 10
```

按子串（不区分大小写）搜索 L1（偏好）/ L2（环境事实）/ L3（任务记录）的 `content` 与 `tags`。新→旧排序。Memory 是 **append-only**，删除走 Phase 6 GC。

---

## `evoclaw gateway`

```bash
evoclaw gateway --bind 127.0.0.1:7878 --token mychat
```

启动 `evo-gateway`，传入两个 env：

- `EVO_GATEWAY_BIND` — TCP 监听地址
- `EVO_GATEWAY_ALLOWLIST` — 逗号分隔的 bearer token 列表

路由：

| 方法 / 路径 | 鉴权 | 行为 |
|-------------|------|------|
| `GET /` | 无 | 静态 WebChat HTML |
| `GET /healthz` | 无 | `200 ok` |
| `POST /chat` | `Authorization: Bearer <token>` | body `{"input": "..."}`，返回 `{task_id, turns, final_text}` |

设计上仅本地。无 TLS。只在受信网络才绑 `0.0.0.0`。

---

## 配置：`~/.evoclaw/config.toml`

```toml
[model]
provider = "deepseek"                                 # 决定 API key 文件 + provider 适配器
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

| 字段 | 类型 | 默认 | 说明 |
|------|------|------|------|
| `model.provider` | string | `deepseek` | 同时决定 API key 解析和 provider 适配器（或 `acp:<id>` 走 ACP） |
| `model.default` | string | `deepseek-chat` | 发给 provider 的 `model` 字段 |
| `model.base_url` | URL | `https://api.deepseek.com/v1` | 任意 OpenAI 兼容端点；ACP provider 忽略此项 |
| `model.fallback` | array | `[]` | （Phase 3+）主 provider 失败时使用 |
| `budget.per_task_usd` | float | 0.50 | HardStop 阈值（PRD §35） |
| `budget.per_day_usd` | float | 5.0 | SoftWarn（三级；硬上限是 4×） |
| `budget.per_month_usd` | float | 100.0 | HardStop |
| `security.default_permission` | enum | `P1` | P0..P8 阶梯 |
| `security.high_risk_intercept` | bool | `true` | P5+ 操作强制 `ask_user` |

权限阶梯（PRD §13.1）：

```
P0 只读     P1 workspace 写       P2 安全 shell
P3 网络     P4 浏览器              P5 用户目录写
P6 系统     P7 凭证                P8 生产
```

渠道发送者**硬封顶在 P4**，无视 `default_permission`。

---

## Provider 字段速查

| `provider` 值 | 含义 | 认证位置 |
|---------------|------|---------|
| `deepseek` `kimi` `qwen` `openai` `openrouter` | OpenAI 兼容 HTTP | `~/.evoclaw/secrets/<provider>.key`（chmod 600） |
| `anthropic` | 原生 Anthropic Messages API | `~/.evoclaw/secrets/anthropic.key` |
| `copilot` | GitHub Copilot via OAuth Device Flow | `~/.evoclaw/secrets/copilot.key`（refresh token） |
| `ollama`（或其他 `local`） | OpenAI 兼容，地址 `http://localhost:11434/v1` | （无需 key） |
| `acp:claude` `acp:codex` `acp:cursor` `acp:copilot` | 通过 ACP 调外部 CLI | 由 CLI 自行登录（`claude login`、`gh auth login`…） |
| `custom` | 你自己设置的 OpenAI 兼容端点 | 你自己的 key |

---

## 环境变量

| 变量 | 谁需要 | 用途 |
|------|--------|------|
| `EVO_API_KEY` | `evoclaw run`、`evoclaw gateway`、`evo-gateway` | 模型 provider 的 bearer —— 覆盖磁盘上的 key 文件 |
| `EVO_GATEWAY_BIND` | `evo-gateway` | 覆盖 `127.0.0.1:7878` |
| `EVO_GATEWAY_ALLOWLIST` | `evo-gateway` | 逗号分隔的 bearer token 列表 |
| `RUST_LOG` | 全部 | `tracing` filter，例如 `RUST_LOG=evo_core=debug` |
| `NO_COLOR` / `EVO_NO_COLOR` | `evoclaw` | 禁用 welcome banner 的 ANSI 颜色 |

向导捕获的 API key 持久化到 `~/.evoclaw/secrets/`（chmod 600）。Vault 条目 —— 与 provider key 独立 —— 存储在 `~/.evoclaw/secrets/vault.json`，仅用于脱敏。

---

## 工具清单（内置，上限 10，当前 7 个）

| # | 名称 | 权限 | 用途 |
|---|------|------|------|
| 1 | `read_file` | P0 | 带行号读取；agent 必须先读后写。 |
| 2 | `write_file` | P1 | workspace 内写入（不会跑出 `~/.evoclaw/workspace/`）。 |
| 3 | `patch_file` | P1 | 唯一替换某段子串。匹配数 ≠ 1 直接拒绝。 |
| 4 | `list_dir` | P0 | 列目录；自动跳过 `node_modules`/`.git`/`target`。 |
| 5 | `run_shell` | P2 | 沙箱 `sh -c`，默认 30s 超时，输出截到 8K。 |
| 6 | `web_fetch` | P3 | 仅 HTTPS。Cookie 在响应进 LLM 上下文前剥除。 |
| 7 | `ask_user` | P0 | 参数歧义或动作高危时**必须**调用。 |

MCP 桥接的工具（如 `mcp__github__list_issues`）也在同一 registry 中，但**不计入** 10 工具上限。它们以 `McpToolWrapper` 形式包装，权限 `Permission::P3`（网络）。

---

## 文件系统约定

```
~/.evoclaw/
├── config.toml                          # 当前配置
├── workspace/                           # 工具沙箱；默认 cwd
├── logs/{task-id}.jsonl                 # 每任务一份
├── skills/{skill-id}.yaml               # 每 skill 一份
├── skills/index.json                    # skill tree 索引
├── memory/{L0..L5}.jsonl                # 每层一份
├── secrets/<provider>.key               # 每 provider 一个 API key，chmod 600
├── secrets/vault.json                   # 命名密钥保险柜，chmod 600
├── agents/<id>.toml                     # ACP agent profile
├── mcp/<id>.toml                        # MCP server profile
├── plugins/                             # 预留
├── cache/                               # 临时
└── cost.jsonl                           # 每轮一行 cost event
```

JSONL 记录通过 `kind: "task" | "turn" | "end"` 区分类型（`evo_core::session::SessionRecord`）。Schema 自 v0.4 起稳定。

---

## 退出码

| 码 | 原因 |
|----|------|
| 0 | 成功 |
| 1 | 运行时错（model 4xx/5xx、tool 错、IO、缺 key） |
| 2 | 预算 hard-stop（错误链中含 `Budget(...)`） |
| 130 | Ctrl-C |

---

## 速查

```bash
# 主循环
evoclaw run "..."                       # 干活
evoclaw replay                          # 看刚刚发生了什么
evoclaw doctor-of tokens                # 花了多少
evoclaw doctor-of closure               # 会话完整性审计

# 大脑
evoclaw skill list                      # 学到了什么
evoclaw skill tree                      # 按领域看
evoclaw memory search "..."             # 查一条事实

# 集成
evoclaw agent catalog                   # ACP agent 目录
evoclaw mcp catalog                     # MCP 服务器目录
evoclaw secret add github_pat           # 本地密钥保险柜

# 入口
evoclaw gateway --bind 127.0.0.1:7878   # 浏览器用户的 WebChat
```
