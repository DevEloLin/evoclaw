//! Slack channel adapter — Socket Mode WebSocket.
//!
//! ## Quick start
//!
//! 1. Create a Slack app at <https://api.slack.com/apps>.
//! 2. Enable **Socket Mode** (Settings → Socket Mode → Enable).
//! 3. Under **Basic Information → App-Level Tokens**, generate a token with
//!    `connections:write` scope.  Store it: `evoclaw secret add slack_app_token`.
//! 4. Under **OAuth & Permissions**, add Bot Token Scopes: `chat:write`,
//!    `channels:history`, `im:history`.  Install to your workspace and store
//!    the `xoxb-` token: `evoclaw secret add slack_bot_token`.
//! 5. Under **Event Subscriptions → Subscribe to bot events**, add
//!    `message.channels` and `message.im`.
//! 6. Run: `evo channel run --kind slack`
//!
//! ## Protocol
//!
//! Socket Mode opens a WebSocket to Slack's servers — no public URL required.
//! Each inbound envelope must be acknowledged by echoing its `envelope_id`
//! within 3 seconds.  Replies go through the `chat.postMessage` REST API.

use crate::channel::{ChannelAdapter, ChannelKind, InboundMessage, OutboundMessage};
use async_trait::async_trait;
use eyre::{Result, WrapErr};
use futures::{SinkExt, StreamExt};
use reqwest::Client;
use serde::Deserialize;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_tungstenite::{connect_async, tungstenite::Message};

const SLACK_API: &str = "https://slack.com/api";

/// Slack Socket Mode adapter. Wrap in `Arc` and pass to `ChannelRouter::register`.
pub struct SlackAdapter {
    /// Bot token (xoxb-) — used for `chat.postMessage`.
    bot_token: String,
    /// App-level token (xapp-) — used to open the Socket Mode WebSocket.
    app_token: String,
    client: Client,
}

impl SlackAdapter {
    pub fn new(bot_token: impl Into<String>, app_token: impl Into<String>) -> Self {
        Self {
            bot_token: bot_token.into(),
            app_token: app_token.into(),
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("reqwest TLS client"),
        }
    }

    /// Call `apps.connections.open` to get a one-time WebSocket URL.
    async fn open_connection_url(&self) -> Result<String> {
        let resp = self
            .client
            .post(format!("{SLACK_API}/apps.connections.open"))
            .bearer_auth(&self.app_token)
            .send()
            .await
            .wrap_err("slack: apps.connections.open request")?
            .json::<SlackConnectResp>()
            .await
            .wrap_err("slack: apps.connections.open decode")?;

        if !resp.ok {
            return Err(eyre::eyre!(
                "Slack API error: {}",
                resp.error.as_deref().unwrap_or("unknown")
            ));
        }
        resp.url
            .ok_or_else(|| eyre::eyre!("slack: no WebSocket URL in response"))
    }

    async fn post_message(&self, channel: &str, text: &str) -> Result<()> {
        #[derive(serde::Serialize)]
        struct Body<'a> {
            channel: &'a str,
            text: &'a str,
        }
        let resp = self
            .client
            .post(format!("{SLACK_API}/chat.postMessage"))
            .bearer_auth(&self.bot_token)
            .json(&Body { channel, text })
            .send()
            .await
            .wrap_err("slack: chat.postMessage")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!(status = %status, body = %body, "slack: postMessage failed");
        }
        Ok(())
    }
}

#[async_trait]
impl ChannelAdapter for SlackAdapter {
    fn kind(&self) -> ChannelKind {
        ChannelKind::Slack
    }
    fn name(&self) -> &str {
        "slack"
    }

    async fn run(
        self: Arc<Self>,
        tx: tokio::sync::mpsc::Sender<InboundMessage>,
    ) -> eyre::Result<()> {
        loop {
            let ws_url = match self.open_connection_url().await {
                Ok(u) => u,
                Err(e) => {
                    tracing::warn!(error = ?e, "slack: connection open failed, retrying in 10s");
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    continue;
                }
            };

            let (ws_stream, _) = match connect_async(&ws_url).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = ?e, "slack: WebSocket connect failed, retrying in 5s");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };

            let (mut ws_tx, mut ws_rx) = ws_stream.split();

            while let Some(frame) = ws_rx.next().await {
                let frame = match frame {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::warn!(error = ?e, "slack: WebSocket error, reconnecting");
                        break;
                    }
                };

                if frame.is_ping() {
                    let _ = ws_tx.send(Message::Pong(frame.into_data())).await;
                    continue;
                }

                let raw = match frame.into_text() {
                    Ok(t) => t,
                    Err(_) => continue,
                };

                let envelope: SlackEnvelope = match serde_json::from_str(&raw) {
                    Ok(e) => e,
                    Err(_) => continue,
                };

                // Acknowledge every envelope within Slack's 3-second window.
                if let Some(ref env_id) = envelope.envelope_id {
                    let ack = format!(r#"{{"envelope_id":"{env_id}"}}"#);
                    let _ = ws_tx.send(Message::Text(ack)).await;
                }

                let Some(payload) = envelope.payload else {
                    continue;
                };
                let Some(event) = payload.event else {
                    continue;
                };
                if event.r#type != "message" {
                    continue;
                }
                // Skip bot-originated messages to avoid reply loops.
                if event.bot_id.is_some() || event.subtype.as_deref() == Some("bot_message") {
                    continue;
                }

                let text = match event.text.filter(|t| !t.trim().is_empty()) {
                    Some(t) => t,
                    None => continue,
                };

                let now_ms = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i64;

                let inbound = InboundMessage {
                    channel: ChannelKind::Slack,
                    conversation_id: event.channel.unwrap_or_default(),
                    sender_id: event.user.unwrap_or_default(),
                    sender_name: None,
                    // Socket Mode only delivers subscribed events; treat all as relevant.
                    mentions_self: true,
                    text,
                    received_at_ms: now_ms,
                };

                if tx.send(inbound).await.is_err() {
                    return Ok(()); // router dropped receiver — graceful exit
                }
            }

            tracing::info!("slack: WebSocket closed, reconnecting in 3s");
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    }

    async fn send(&self, msg: OutboundMessage) -> eyre::Result<()> {
        self.post_message(&msg.conversation_id, &msg.text).await
    }
}

// ── Slack Socket Mode API types ───────────────────────────────────────────────

#[derive(Deserialize)]
struct SlackConnectResp {
    ok: bool,
    error: Option<String>,
    url: Option<String>,
}

#[derive(Deserialize)]
struct SlackEnvelope {
    envelope_id: Option<String>,
    payload: Option<SlackPayload>,
}

#[derive(Deserialize)]
struct SlackPayload {
    event: Option<SlackEvent>,
}

#[derive(Deserialize)]
struct SlackEvent {
    r#type: String,
    user: Option<String>,
    text: Option<String>,
    channel: Option<String>,
    bot_id: Option<String>,
    subtype: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_metadata() {
        let a = SlackAdapter::new("xoxb-token", "xapp-token");
        assert_eq!(a.kind(), ChannelKind::Slack);
        assert_eq!(a.name(), "slack");
    }
}
