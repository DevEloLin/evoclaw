# 通道适配器

EvoClaw 暴露了一个 **通道适配器（channel adapter）** 插件接口，让同一个 Agent
循环可以同时被 Telegram、Slack、Discord、Line、Messenger 或任意自定义传输层
驱动。本页介绍 v0.5 中已经落地的脚手架与 v0.6 路线图。

## 整体结构

```
                +-------------------+        +-----------------------+
   入站 ----->  | ChannelAdapter::  |  mpsc  | ChannelRouter         |
   (Telegram、  |   run() 推送      | -----> |   should_handle(msg)  |
    Slack 等)   |   InboundMessage  |        |   -> ConversationRun  |
                +-------------------+        +-----------+-----------+
                                                         |
                ^------- OutboundMessage <----------------+
                         ChannelAdapter::send()
```

承载这套接口的两个模块：

- `evo_core::channel` —— `ChannelAdapter` trait、`ChannelKind`、
  `InboundMessage`、`OutboundMessage`、`OutboundKind`。
- `evo_core::channel_router` —— `ChannelRouter`（多入多出汇聚）
  以及 `should_handle()` 提及策略过滤器。

参考实现 `evo_core::local_pipe::LocalPipe` 通过 stdin 接收入站 JSON、向
stdout 写出站 JSON。它既是整条链路的冒烟测试，也是插件作者的样板代码。

## 命令行

```sh
evo channel list                    # 列出内置适配器 + ~/.evoclaw/channels/*.toml
evo channel run --kind local-pipe   # 端到端跑通参考适配器
```

交互式 REPL 也支持 `/channel list` 和 `/channel run local-pipe`。

## 快速演示：local-pipe

`local-pipe` 以行分隔的 JSON 接收 `InboundMessage`，再以行分隔的 JSON 输出
`OutboundMessage`，例如：

```json
{"channel":"LocalPipe","conversation_id":"c1","sender_id":"u1",
 "sender_name":"alice","mentions_self":true,
 "text":"总结一下 README",
 "received_at_ms":1700000000000}
```

EvoClaw 回复：

```json
{"conversation_id":"c1","text":"<Agent 最终回答>","kind":"Reply"}
```

## 实现自定义适配器

实现 trait 即可。完整参考请见 `crates/evo-core/src/local_pipe.rs`。

```rust
use async_trait::async_trait;
use evo_core::channel::{
    ChannelAdapter, ChannelKind, InboundMessage, OutboundMessage,
};
use std::sync::Arc;

pub struct MyAdapter { /* 传输句柄、配置等 */ }

#[async_trait]
impl ChannelAdapter for MyAdapter {
    fn kind(&self) -> ChannelKind { ChannelKind::Custom("matrix".into()) }
    fn name(&self) -> &str { "matrix" }

    async fn run(
        self: Arc<Self>,
        tx: tokio::sync::mpsc::Sender<InboundMessage>,
    ) -> eyre::Result<()> {
        // 轮询/流式拉取传输层、解析事件，再推送 InboundMessage。
        // 仅在私聊或群聊中明确 @ 到机器人时把 `mentions_self` 设为 true，
        // 否则路由器会按 P4 提及策略丢弃这条消息。
        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> eyre::Result<()> {
        // 把回复发回传输层。如果你的通道区分
        // Reply / Notice / Error，可以根据 `msg.kind` 处理。
        Ok(())
    }
}
```

适配器必须 `Send + Sync`，并且尽量便宜地克隆（持有 `Arc<inner>`）。
`ChannelRouter::run_all` 会为每个适配器单独 spawn 一个 Tokio 任务。

### 提及策略

通道入口在 EvoClaw 的权限阶梯中被硬性约束为 **P4**。路由器在分发前会调用
`channel_router::should_handle(&msg)`；默认实现返回 `msg.mentions_self`。
请只在私聊或显式 `@` 时把它置 true，群聊水军不要打开。

## 通过 `~/.evoclaw/channels/*.toml` 接入

未来的外部适配器将沿用 MCP / ACP 的目录布局：每个 `*.toml` 描述一个适配器。

```toml
# ~/.evoclaw/channels/telegram.toml
kind   = "telegram"
token  = "${SECRET:telegram_bot_token}"
[mention]
self_handle = "@evoclaw_bot"
```

`evo channel list` 会扫描该目录下的 `*.toml` 并打印一行摘要。真正的加载器
将在 v0.6 随 transport 一起落地——目前 `list` 仅仅是把它们枚举出来。

## 路线图（v0.6）

| 适配器     | 状态     | 备注                                           |
| ---------- | -------- | ---------------------------------------------- |
| local-pipe | 已发布   | stdin/stdout JSON；参考实现 + 冒烟测试         |
| telegram   | 计划中   | 长轮询 Bot API；使用 `${SECRET:tg_token}`     |
| slack      | 计划中   | events API + socket mode                       |
| discord    | 计划中   | gateway WS + 斜杠命令                          |
| line       | 计划中   | webhook，由 `evo-gateway` 托管                 |
| messenger  | 计划中   | webhook + verify token                         |

trait 的接口刻意收窄，方便后续每种 transport 都能不动调用方就接入。

## 权限与密钥

- 所有通道输入都会经过与 CLI 相同的 redactor / vault 层——入站文本里的
  密钥会在模型看到之前被擦除。
- 通道驱动的运行默认 `allow_user_prompt = false`；需要 tty 交互的工具
  会直接拒绝，不会卡住。
- 按 PRD 的权限阶梯，通道运行级别永远不超过 P4。需要 P5+ 的能力必须
  在配置里显式 allow-list。

## 相关文档

- `crates/evo-core/src/channel.rs` —— trait 与类型
- `crates/evo-core/src/channel_router.rs` —— 路由器 + 提及策略
- `crates/evo-core/src/local_pipe.rs` —— 参考适配器
- `docs/zh/agents.md` —— ACP 外部 Agent（另一条插件轴）
- `docs/zh/mcp.md` —— MCP 服务器插件（再一条插件轴）
