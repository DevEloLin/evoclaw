# 外部 ACP 代理

EvoClaw 可以把"代理循环"完全交给一个会说
[Agent Client Protocol](https://github.com/zed-industries/agent-client-protocol)
（ACP）的外部 CLI —— 这正是 Zed 用来集成 Claude Code、Codex、Cursor、
GitHub Copilot 的同一个开放协议。当你选用 ACP 代理时，那个代理自己
处理认证和工具调用循环，EvoClaw 把它当作一个"黑盒回合应答器"。

## 为什么走 ACP

- **认证留在它该在的地方。** Claude Code 用 `claude login` 登录，
  Cursor 用 `cursor login`，GitHub Copilot 走 OAuth Device Flow。
  EvoClaw 既不会看到、也不会保存或转发它们的凭据。
- **不抓浏览器，不踩 ToS。** 没有 headless Chrome，没有偷 cookie，
  ACP CLI 是各厂家自己的官方客户端。
- **一种协议，多种代理。** 把任意一个支持 ACP 的 CLI 放进
  `~/.evoclaw/agents/`，EvoClaw 不改一行代码就能驱动它。

## 内置目录

| ID          | 可执行名         | 参数                       | 安装方式                                  |
|-------------|------------------|----------------------------|-------------------------------------------|
| `claude`    | `claude`         | `--acp`                    | `npm i -g @anthropic-ai/claude-code`      |
| `codex`     | `codex`          | `--acp`                    | `npm i -g @openai/codex`                  |
| `cursor`    | `cursor-agent`   | `--acp`                    | 随 Cursor 桌面端附带                       |
| `copilot`   | `gh`             | `copilot suggest --acp`    | `gh extension install github/gh-copilot`  |
| `gemini`    | `gemini`         | `--acp`                    | `npm i -g @google/gemini-cli`             |
| `aider`     | `aider`          | `--acp`                    | `pipx install aider-chat`                 |
| `qwen-code` | `qwen`           | `--acp`                    | `npm i -g @qwen-code/qwen-code`           |

随时查看：

```bash
evoclaw agent catalog
```

## 添加一个代理

```bash
# 从目录里挑一个
evoclaw agent add claude

# 验证：spawn + ACP initialize 握手
evoclaw agent test claude

# 列出已配置代理
evoclaw agent list

# 移除
evoclaw agent remove claude
```

`add` 会写入 `~/.evoclaw/agents/<id>.toml`：

```toml
id           = "claude"
name         = "Claude Agent"
command      = "claude"
args         = ["--acp"]
env          = []
installed_at = "2026-05-02T17:52:00Z"
```

## 把代理循环切到 ACP 代理

两种等价方式：

1. **向导** —— 运行 `evoclaw onboard`（或 `evoclaw login`），选最后一项
   "External ACP agent"，再选具体代理。向导会把
   `~/.evoclaw/config.toml` 改为 `provider = "acp:<id>"`。
2. **手改** —— 直接编辑 `~/.evoclaw/config.toml`：
   ```toml
   [model]
   provider = "acp:claude"
   default  = "acp:claude"
   base_url = ""
   fallback = []
   ```

切换后，无论你在交互式 REPL 还是 `evoclaw run "<task>"` 里输入什么，
都会通过 `AcpProvider` → ACP `session/prompt` → 代理的 final text。

## 斜杠命令

REPL 内：

```text
/agent              列出已配置代理
/agent catalog      显示内置目录
/agent add <id>     从目录添加
/agent test <id>    spawn + initialize 握手
/agent remove <id>  删除配置
```

## 常见问题

**`spawn <id> failed: No such file or directory`** —— 二进制不在 PATH。
按目录中提示安装，或者把 TOML 配置里的 `command` 改为绝对路径。

**`ACP initialize: ...`** —— spawn 成功但握手失败。先在外部运行该代理
自己的登录命令（如 `claude login`），再 `evoclaw agent test <id>` 验证。

**首轮慢** —— 很多 ACP 代理首次启动会拉取自己的远程配置，第一次提示
比后续慢几秒属于正常。
