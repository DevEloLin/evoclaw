# MCP 服务器

EvoClaw 是一个标准的
[Model Context Protocol](https://modelcontextprotocol.io)
客户端。给本地实例添加任何 MCP 服务器，它暴露的工具就会出现在代理
工具表里，命名格式 `mcp__<server_id>__<tool>`，和内置工具混排。

## 内置目录

| ID            | 说明                              | 鉴权环境变量                            |
|---------------|-----------------------------------|-----------------------------------------|
| `filesystem`  | 在配置好的根目录里读写文件         | —                                       |
| `github`      | 通过 GitHub API 操作 issue/PR/repo | `GITHUB_PERSONAL_ACCESS_TOKEN`          |
| `fetch`       | 抓取网页并转 markdown              | —                                       |
| `time`        | 时区与当前时间查询                 | —                                       |
| `brave-search`| Brave API 网页搜索                 | `BRAVE_API_KEY`                         |
| `postgres`    | 只读 SQL 查询                      | `DATABASE_URL`                          |
| `slack`       | 读取消息、向频道发帖                | `SLACK_BOT_TOKEN`, `SLACK_TEAM_ID`      |

```bash
evoclaw mcp catalog
```

## 添加一个服务器

```bash
# 先在外部 export 鉴权环境变量，`add` 会把它写进配置
export GITHUB_PERSONAL_ACCESS_TOKEN=ghp_xxx

evoclaw mcp add github
evoclaw mcp test github      # spawn + initialize + tools/list + shutdown
evoclaw mcp list
evoclaw mcp remove github
```

`add` 写入 `~/.evoclaw/mcp/<id>.toml`：

```toml
id           = "github"
name         = "GitHub"
command      = "npx"
args         = ["-y", "@modelcontextprotocol/server-github"]
env          = [["GITHUB_PERSONAL_ACCESS_TOKEN", "ghp_xxx"]]
installed_at = "2026-05-02T17:52:00Z"
```

捕获到的鉴权 env 是 spawn 配置的一部分，模型本身看不到 token 原文。

## 工具如何呈现给模型

每次 `evoclaw run`（包括 REPL 里每次提交）启动时，运行时都会扫
`~/.evoclaw/mcp/`、spawn 每个服务器、跑一遍 MCP `initialize` +
`tools/list` 握手，然后通过 `McpToolWrapper` 把每个工具登记进表里。
某个服务器失败只会被记日志并跳过，不会阻止代理启动。

会看到一行启动横幅：

```text
→ MCP: 2 server(s) attached, registry now has 19 tools
```

工具名前缀 `mcp__<server>__<tool>` 可以避免两个服务器同名冲突
（例如 `mcp__github__search` 与 `mcp__brave-search__search`）。

## 斜杠命令

REPL 内：

```text
/mcp              列出已配置服务器
/mcp catalog      显示内置目录
/mcp add <id>     从目录添加（捕获鉴权 env）
/mcp test <id>    spawn + initialize + tools/list
/mcp remove <id>  删除配置
```

## 常见问题

**`initialize failed: ...`** —— 服务器 spawn 成功但握手失败。常见原因：
环境变量缺失或值错。重新 export 后再 `evoclaw mcp add <id>` 覆盖配置。

**`spawn ... failed: No such file or directory`** —— 先装服务器对应的
运行时。`npx` 服务器需 Node ≥18；`uvx` 服务器需 `pipx install uv`。

**`mcp__<server>__<tool>: tool result is empty`** —— 服务器返回
`isError: false` 但没有文本内容。先用该服务器自己的 CLI 直接测一下；
某些 MCP 服务器输出非文本块（图片/二进制），EvoClaw 会渲染成
`[image: <mime>]` 占位符。
