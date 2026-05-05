//! Discord channel adapter — Gateway WebSocket (v10).
//!
//! ## Quick start
//!
//! 1. Create an application at <https://discord.com/developers/applications>.
//! 2. Add a Bot under the **Bot** tab; copy the token.
//!    Store it: `evoclaw secret add discord_bot_token`.
//! 3. Under **Bot → Privileged Gateway Intents**, enable
//!    **Message Content Intent** (required to read message text).
//! 4. Invite the bot to your server with scopes `bot` + permissions
//!    `Send Messages`, `Read Message History`.
//! 5. Run: `evo channel run --kind discord`
//!
//! ## Protocol
//!
//! Connects to `wss://gateway.discord.gg` — no public URL required.
//! Handles Hello / Identify / Heartbeat lifecycle and dispatches
//! `MESSAGE_CREATE` events.  Replies via `POST /channels/{id}/messages`.
//!
//! ## Intents bitmask
//!
//! | Intent          | Bit    | Value  |
//! |-----------------|--------|--------|
//! | GUILD_MESSAGES  | 1 << 9 |    512 |
//! | DIRECT_MESSAGES |1 << 12 |   4096 |
//! | MESSAGE_CONTENT |1 << 15 |  32768 |

use crate::channel::{ChannelAdapter, ChannelKind, InboundMessage, OutboundMessage};
use async_trait::async_trait;
use eyre::{Result, WrapErr};
use futures::{SinkExt, StreamExt};
use reqwest::Client;
use serde::Deserialize;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_tungstenite::{connect_async, tungstenite::Message};

const DISCORD_API: &str = "https://discord.com/api/v10";
const DISCORD_GATEWAY: &str = "wss://gateway.discord.gg/?v=10&encoding=json";
/// GUILD_MESSAGES | DIRECT_MESSAGES | MESSAGE_CONTENT
const INTENTS: u32 = (1 << 9) | (1 << 12) | (1 << 15);

/// Discord Gateway adapter. Wrap in `Arc` and pass to `ChannelRouter::register`.
pub struct DiscordAdapter {
    token: String,
    client: Client,
}

impl DiscordAdapter {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("reqwest TLS client"),
        }
    }

    fn auth_header(&self) -> String {
        format!("Bot {}", self.token)
    }

    async fn post_message(&self, channel_id: &str, text: &str) -> Result<()> {
        #[derive(serde::Serialize)]
        struct Body<'a> {
            content: &'a str,
        }
        let resp = self
            .client
            .post(format!("{DISCORD_API}/channels/{channel_id}/messages"))
            .header("Authorization", self.auth_header())
            .json(&Body { content: text })
            .send()
            .await
            .wrap_err("discord: post message")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!(status = %status, body = %body, "discord: post message failed");
        }
        Ok(())
    }

    /// Run one Gateway session until the connection closes or errors.
    async fn run_session(
        self: &Arc<Self>,
        tx: &tokio::sync::mpsc::Sender<InboundMessage>,
        bot_user_id: &mut Option<String>,
    ) -> Result<()> {
        let (ws, _) = connect_async(DISCORD_GATEWAY)
            .await
            .wrap_err("discord: Gateway connect")?;
        let (mut sink, mut stream) = ws.split();

        // First frame is always HELLO (op 10) carrying heartbeat_interval.
        let hello_raw = stream
            .next()
            .await
            .ok_or_else(|| eyre::eyre!("discord: stream closed before Hello"))??
            .into_text()
            .wrap_err("discord: Hello not text")?;
        let hello: GwPayload<HelloData> =
            serde_json::from_str(&hello_raw).wrap_err("discord: Hello decode")?;
        let hb_ms = hello
            .d
            .ok_or_else(|| eyre::eyre!("discord: Hello missing d"))?
            .heartbeat_interval;

        // Send IDENTIFY (op 2).
        let identify = serde_json::json!({
            "op": 2,
            "d": {
                "token": self.token,
                "intents": INTENTS,
                "properties": { "os": "linux", "browser": "evoclaw", "device": "evoclaw" }
            }
        });
        sink.send(Message::Text(identify.to_string().into()))
            .await
            .wrap_err("discord: send Identify")?;

        let mut hb_timer = tokio::time::interval(Duration::from_millis(hb_ms));
        hb_timer.tick().await; // consume the immediate first tick
        let mut last_seq: Option<u64> = None;

        loop {
            tokio::select! {
                _ = hb_timer.tick() => {
                    let hb = serde_json::json!({"op": 1, "d": last_seq});
                    if sink.send(Message::Text(hb.to_string().into())).await.is_err() {
                        break;
                    }
                }
                frame = stream.next() => {
                    let frame = match frame {
                        None => break,
                        Some(Err(e)) => {
                            tracing::warn!(error = ?e, "discord: WebSocket error");
                            break;
                        }
                        Some(Ok(f)) => f,
                    };

                    if frame.is_ping() {
                        let _ = sink.send(Message::Pong(frame.into_data())).await;
                        continue;
                    }

                    let raw = match frame.into_text() {
                        Ok(t) => t,
                        Err(_) => continue,
                    };

                    let event: GwEvent = match serde_json::from_str(&raw) {
                        Ok(e) => e,
                        Err(_) => continue,
                    };

                    if let Some(s) = event.s {
                        last_seq = Some(s);
                    }

                    match event.op {
                        11 => {} // Heartbeat ACK
                        7 => break, // Reconnect requested
                        0 => {
                            match event.t.as_deref() {
                                Some("READY") => {
                                    if let Some(d) = event.d {
                                        *bot_user_id = d
                                            .get("user")
                                            .and_then(|u| u.get("id"))
                                            .and_then(|v| v.as_str())
                                            .map(str::to_owned);
                                    }
                                }
                                Some("MESSAGE_CREATE") => {
                                    if let Some(d) = event.d {
                                        self.handle_message(d, bot_user_id, tx).await;
                                    }
                                }
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(())
    }

    async fn handle_message(
        &self,
        d: serde_json::Value,
        bot_user_id: &Option<String>,
        tx: &tokio::sync::mpsc::Sender<InboundMessage>,
    ) {
        let msg: DiscordMessage = match serde_json::from_value(d) {
            Ok(m) => m,
            Err(_) => return,
        };

        // Skip bot messages (including self) to avoid reply loops.
        if msg.author.bot.unwrap_or(false) {
            return;
        }

        let text = msg.content.trim().to_owned();
        if text.is_empty() {
            return;
        }

        // DM (channel_type 1) always counts as self-mention;
        // in guild channels detect <@BOT_ID> in the message text.
        let mentions_self = msg.channel_type == Some(1)
            || bot_user_id
                .as_deref()
                .map(|id| text.contains(&format!("<@{id}>")))
                .unwrap_or(false);

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        let inbound = InboundMessage {
            channel: ChannelKind::Discord,
            conversation_id: msg.channel_id,
            sender_id: msg.author.id,
            sender_name: Some(msg.author.username),
            mentions_self,
            text,
            received_at_ms: now_ms,
        };

        let _ = tx.send(inbound).await;
    }
}

#[async_trait]
impl ChannelAdapter for DiscordAdapter {
    fn kind(&self) -> ChannelKind {
        ChannelKind::Discord
    }
    fn name(&self) -> &str {
        "discord"
    }

    async fn run(
        self: Arc<Self>,
        tx: tokio::sync::mpsc::Sender<InboundMessage>,
    ) -> eyre::Result<()> {
        let mut bot_user_id: Option<String> = None;
        loop {
            match self.run_session(&tx, &mut bot_user_id).await {
                Ok(_) => tracing::info!("discord: session closed, reconnecting in 5s"),
                Err(e) => tracing::warn!(error = ?e, "discord: session error, reconnecting in 5s"),
            }
            if tx.is_closed() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }

    async fn send(&self, msg: OutboundMessage) -> eyre::Result<()> {
        self.post_message(&msg.conversation_id, &msg.text).await
    }
}

// ── Discord Gateway types ─────────────────────────────────────────────────────

#[derive(Deserialize)]
struct GwPayload<D> {
    d: Option<D>,
}

#[derive(Deserialize)]
struct HelloData {
    heartbeat_interval: u64,
}

#[derive(Deserialize)]
struct GwEvent {
    op: u8,
    s: Option<u64>,
    t: Option<String>,
    d: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct DiscordMessage {
    channel_id: String,
    content: String,
    author: DiscordUser,
    /// 1 = DM, 0 = guild text channel
    channel_type: Option<u8>,
}

#[derive(Deserialize)]
struct DiscordUser {
    id: String,
    username: String,
    bot: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_metadata() {
        let a = DiscordAdapter::new("token");
        assert_eq!(a.kind(), ChannelKind::Discord);
        assert_eq!(a.name(), "discord");
    }

    #[test]
    fn intents_value() {
        assert_eq!(INTENTS, 512 + 4096 + 32768);
    }
}
