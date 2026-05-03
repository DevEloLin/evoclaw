# Channels

EvoClaw exposes a **channel adapter** plug-in surface so the same agent
loop can be driven from Telegram, Slack, Discord, Line, Messenger, or any
custom transport. This page documents the v0.5 scaffolding that ships
in-tree and the v0.6 roadmap.

## At a glance

```
                +-------------------+        +-----------------------+
   inbound ---> | ChannelAdapter::  |  mpsc  | ChannelRouter         |
   (Telegram,   |   run() pushes    | -----> |   should_handle(msg)  |
    Slack, ...) |   InboundMessage  |        |   -> ConversationRun  |
                +-------------------+        +-----------+-----------+
                                                         |
                ^------- OutboundMessage <----------------+
                         ChannelAdapter::send()
```

Two crates carry the surface:

- `evo_core::channel` — `ChannelAdapter` trait, `ChannelKind`,
  `InboundMessage`, `OutboundMessage`, `OutboundKind`.
- `evo_core::channel_router` — `ChannelRouter` (fan-in/fan-out) and the
  `should_handle()` mention-policy filter.

The reference adapter `evo_core::local_pipe::LocalPipe` reads inbound
JSON from stdin and writes outbound JSON to stdout. It is the smoke-test
for the whole pipeline and the worked example for plugin authors.

## CLI

```sh
evo channel list                    # built-in adapters + ~/.evoclaw/channels/*.toml
evo channel run --kind local-pipe   # drive the in-tree adapter end-to-end
```

The same is reachable from the interactive REPL via `/channel list` and
`/channel run local-pipe`.

## Quick demo: local-pipe

`local-pipe` reads one JSON `InboundMessage` per line on stdin and
writes one JSON `OutboundMessage` per line on stdout, e.g.:

```json
{"channel":"LocalPipe","conversation_id":"c1","sender_id":"u1",
 "sender_name":"alice","mentions_self":true,
 "text":"summarise the README",
 "received_at_ms":1700000000000}
```

EvoClaw replies with:

```json
{"conversation_id":"c1","text":"<final agent answer>","kind":"Reply"}
```

## Authoring a custom adapter

Implement the trait. The full reference is `crates/evo-core/src/local_pipe.rs`.

```rust
use async_trait::async_trait;
use evo_core::channel::{
    ChannelAdapter, ChannelKind, InboundMessage, OutboundMessage,
};
use std::sync::Arc;

pub struct MyAdapter { /* transport handle, config, ... */ }

#[async_trait]
impl ChannelAdapter for MyAdapter {
    fn kind(&self) -> ChannelKind { ChannelKind::Custom("matrix".into()) }
    fn name(&self) -> &str { "matrix" }

    async fn run(
        self: Arc<Self>,
        tx: tokio::sync::mpsc::Sender<InboundMessage>,
    ) -> eyre::Result<()> {
        // Poll/stream your transport, decode events, push InboundMessage.
        // Set `mentions_self = true` for DMs and for group messages where
        // the bot was @-mentioned. Otherwise the router will drop the
        // message per the P4 mention policy.
        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> eyre::Result<()> {
        // Post the reply back to your transport. Honor `msg.kind` if your
        // channel distinguishes Reply / Notice / Error.
        Ok(())
    }
}
```

Adapters must be `Send + Sync` and ideally cheap to clone (hold an
`Arc<inner>`). They are invoked by `ChannelRouter::run_all`, which
spawns each adapter on its own Tokio task.

### Mention policy

Channel senders are hard-capped at permission tier **P4** in the
EvoClaw permission ladder. The router calls
`channel_router::should_handle(&msg)` before dispatching; the default
implementation returns `msg.mentions_self`. Set `mentions_self = true`
for DMs and explicit @-mentions only — never for group chatter.

## Wiring via `~/.evoclaw/channels/*.toml`

Future external adapters will be configured per-file under
`~/.evoclaw/channels/`, similar to the existing MCP / ACP layout. The
file naming convention is one TOML per adapter:

```toml
# ~/.evoclaw/channels/telegram.toml
kind   = "telegram"
token  = "${SECRET:telegram_bot_token}"
[mention]
self_handle = "@evoclaw_bot"
```

`evo channel list` discovers any `*.toml` under that directory and
prints a one-line summary. The actual loader for these files lands with
the v0.6 transports — for now `list` only enumerates them.

## Roadmap (v0.6)

| Adapter   | Status   | Notes                                          |
| --------- | -------- | ---------------------------------------------- |
| local-pipe| shipped  | stdin/stdout JSON; reference + smoke test      |
| telegram  | planned  | long-poll Bot API; uses `${SECRET:tg_token}`   |
| slack     | planned  | events API + socket mode                       |
| discord   | planned  | gateway WS + slash commands                    |
| line      | planned  | webhook receiver hosted by `evo-gateway`       |
| messenger | planned  | webhook + verify token                         |

The trait surface above is intentionally narrow so each transport can
be added without churning consumers.

## Permissions and secrets

- All channel inputs are subject to the same redactor / vault layer as
  CLI input — secrets in inbound text are scrubbed before the model
  sees them.
- Channel-driven runs default to `allow_user_prompt = false`; tools
  that require a tty prompt are denied without asking.
- Per the PRD permission ladder, channels never run at higher than P4.
  Anything that would need P5+ must be explicitly allow-listed in
  config.

## See also

- `crates/evo-core/src/channel.rs` — trait + types
- `crates/evo-core/src/channel_router.rs` — router + mention filter
- `crates/evo-core/src/local_pipe.rs` — reference adapter
- `docs/agents.md` — ACP external agents (different plug-in axis)
- `docs/mcp.md` — MCP server plug-ins (yet another axis)
