# 使用参考

完整的 CLI 命令、配置项、环境变量。

## 两个 binary，同一份 CLI

EvoClaw 同时安装两个**完全等价**的 binary：

- **`evoclaw`** — 长格式，项目名（脚本与文档建议用）
- **`evo`** — 3 字母短别名（更短；行为 100% 相同）

两者都 `evo_cli::entry()`，挑你喜欢的。

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

### `evoclaw secret` — 本地密钥保险柜（PRD §13.4）

| 子命令 | 作用 |
|--------|------|
| `add <name> [value]` | 写入 / 覆盖一条记录。不带 value 时 CLI 会提示输入；`--stdin` 从 stdin 一行读入。 |
| `list` | 列出条目 — 仅显示 `name`、`kind`、指纹、`created_at`，**永远不打印原值**。 |
| `remove <name>` | 按 name 删除。 |
| `test <text>` | 用 redactor 跑一遍样本字符串并打印替换后的输出。 |

REPL 内同样可用：`/secret list`、`/secret add NAME VALUE`、`/secret remove NAME`、`/secret test TEXT…`。

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

即使**没有**保险柜条目，下列形状也会被模式兜底自动改写为 `[REDACTED:<kind>:<8 位指纹>]`：`sk-ant-*` / `sk-*`（≥20 字符）/ `ghp_*` / `gho_*` / `ghu_*` / `ghs_*` / `ghr_*` / `AKIA*`（20 位字母数字）/ `eyJ*.*.*` JWT / 任意 32 位以上、Shannon 熵 ≥ 4 bits/char 的高熵串。

### 交互式 REPL 斜线命令

进入 `evoclaw>` 后可以输入：

| 斜线命令 | 等价子命令 |
|---------|-----------|
| `/help` 或 `/?` | （无）打印斜线命令清单 |
| `/skill list` | `evoclaw skill list` |
| `/skill tree` | `evoclaw skill tree` |
| `/skill show <id>` | `evoclaw skill show <id>` |
| `/memory <query>` | `evoclaw memory search <query>` |
| `/tokens` | `evoclaw doctor-of tokens` |
| `/closure` | `evoclaw doctor-of closure` |
| `/replay [path]` | `evoclaw replay [path]` |
| `/doctor` | `evoclaw doctor` |
| `/clear` | 清屏（ANSI `\x1b[2J\x1b[H`） |
| `/exit`、`/quit`、`/q` | 退出（Ctrl-D / EOF 也行） |

不以 `/` 开头的输入会被当作任务执行，等同于 `evoclaw run "..."`。

### `evoclaw run` 完整说明

```bash
evo run "用自然语言描述要干的事"
```

Runtime 内部：

1. 构 system prompt（PROMPTS §1，**严格 6 行**，cache:persistent）
2. 拼接 user 消息：`<history>` 块（最近 40 条 `<summary>`，PRD §42.4）+ `<user_input>`
3. Loop：
   - 每 5 轮做 tag-level 压缩（PRD §42.5）
   - 预算前置检查（PRD §35）
   - 用 `ToolFingerprint` 决定 tools 段是 Full 还是 Reuse（PRD §42.1）
   - stream 模型事件，收集 tool calls
   - 通过 `ToolRegistry` 派发工具
   - JSONL append `TurnRecord`
   - 直到模型不再请求工具
4. 反思回合（PROMPTS §4 → `ReflectionRecord`）
5. 蒸馏（PROMPTS §5 → `Skill` DRAFT，写到 `~/.evoclaw/skills/`）
6. 写 Memory L3（PRD §33）
7. 落 End record

硬上限：

- `max_turns = 25`（通过 `RuntimeConfig` 调）
- `max_tokens = 1024` 每次响应
- `temperature = 0.2`

### `evo replay [path]`

不带 path 时挑 `~/.evoclaw/logs/` 中最新 `*.jsonl`。输出三段：

- `[TASK] <id>` — 初始任务记录
- `[TURN N]` — 每轮的 summary、工具调用、usage（带 cache_hit %）
- `[END]` — 终态（`COMPLETED` / `FAILED`）

### `evo skill <子命令>`

| 子 | 效果 |
|----|------|
| `list` | 列 `~/.evoclaw/skills/` 下所有 `*.yaml` |
| `show <id>` | dump 单个 skill 的 YAML |
| `tree` | 重新扫描所有 skill，重建 `index.json`，按 `SkillKind` 分组渲染 |

### `evo memory search`

```bash
evo memory search "ssh timeout" --limit 10
```

按子串（不区分大小写）搜 L1（偏好）/ L2（环境事实）/ L3（任务记录）的 `content` 与 `tags`。新→旧排序。Memory 是 **append-only**，删除走 Phase 6 GC。

### `evo gateway`

```bash
evo gateway --bind 127.0.0.1:7878 --token mychat
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

设计上仅本地。无 TLS。只在受信网络绑 `0.0.0.0`。

---

## 配置 `~/.evoclaw/config.toml`

```toml
[model]
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
| `model.default` | string | `deepseek-chat` | 发给厂商的 `model` |
| `model.base_url` | URL | `https://api.deepseek.com/v1` | 任意 OpenAI 兼容端点 |
| `model.fallback` | array | `["qwen-plus", "kimi-k2"]` | （Phase 3+）主 fail 时使用 |
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

## 环境变量

| 变量 | 谁需要 | 用途 |
|------|--------|------|
| `EVO_API_KEY` | `evo run` / `evo gateway` / `evo-gateway` | 厂商 bearer |
| `EVO_GATEWAY_BIND` | `evo-gateway` | 覆盖 `127.0.0.1:7878` |
| `EVO_GATEWAY_ALLOWLIST` | `evo-gateway` | 逗号分隔 bearer 列表 |
| `RUST_LOG` | 全部 | `tracing` filter，例如 `RUST_LOG=evo_core=debug` |

Key **永不**落盘。需要持久化的密钥放 `~/.evoclaw/secrets/`（Phase 4 范畴）。

---

## 工具清单（Phase 1+2 已交付 7 个）

| # | 名称 | 权限 | 用途 |
|---|------|------|------|
| 1 | `read_file` | P0 | 带行号读 |
| 2 | `write_file` | P1 | 工作区内写 |
| 3 | `patch_file` | P1 | 唯一替换（必须严格匹配一次） |
| 4 | `list_dir` | P0 | 列目录，跳过 `node_modules`/`.git`/`target` |
| 5 | `run_shell` | P2 | 沙箱 `sh -c`，默认 30s 超时 |
| 6 | `web_fetch` | P3 | 仅 HTTPS，cookie 不进 LLM |
| 7 | `ask_user` | P0 | 高危 / 歧义 / 缺参 时确认 |
| 8 | `browser_action` | P4 | Phase 4.5 — Node Playwright Worker |
| 9 | `memory`（工具） | P1 | Phase 4.5 — 模型可调 memory ops |
| 10 | `skill`（工具） | P1 | Phase 4.5 — 模型可调 skill ops |

新增 8 / 9 / 10 必须先改 PRD §43。

---

## 文件系统约定

```
~/.evoclaw/
├── config.toml                          # 本配置
├── workspace/                           # 工具沙箱
├── logs/{task-id}.jsonl                 # 每任务一份
├── skills/{skill-id}.yaml               # 每 skill 一份
├── skills/index.json                    # skill tree 索引
├── memory/{L0,L1,L2,L3,L4,L5}.jsonl     # 每层一份
├── secrets/                             # chmod 600
├── browser_profiles/                    # Phase 4.5 预留
├── plugins/                             # 预留
├── cache/                               # 临时
└── cost.jsonl                           # 每轮一行 cost event
```

JSONL 通过 `kind: "task" | "turn" | "end"` 区分类型（`evo_core::session::SessionRecord`）。Schema 自 v0.4 起稳定。

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
evo run "..."                          # 干活
evo replay                             # 看刚刚发生了什么
evo doctor-of tokens                   # 花了多少

# 大脑
evo skill list                         # 学到了什么
evo skill tree                         # 按领域看
evo memory search "..."                # 查一条事实

# 入口
evo gateway --bind 127.0.0.1:7878      # 浏览器端 WebChat
```
