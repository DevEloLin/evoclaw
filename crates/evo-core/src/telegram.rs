//! Telegram channel adapter — Bot API long-polling.
//!
//! ## Quick start
//!
//! 1. Create a bot via @BotFather and copy the token.
//! 2. Store it in the EvoClaw vault: `evoclaw secret add telegram_bot_token`
//! 3. Run: `evo channel run --kind telegram`
//!
//! ## Message formatting
//!
//! Outbound messages use `parse_mode=Markdown` so the model's standard
//! Markdown output (*bold*, `code`, triple-backtick blocks) renders natively
//! in Telegram without extra conversion.
//!
//! ## Long-polling model
//!
//! The adapter calls `getUpdates` with a 30-second server-side timeout.
//! On error it backs off 5 seconds and retries indefinitely.
//! The `update_id` offset advances atomically to deduplicate messages.

use crate::channel::{ChannelAdapter, ChannelKind, InboundMessage, OutboundMessage};
use async_trait::async_trait;
use eyre::{Result, WrapErr};
use reqwest::Client;
use serde::Deserialize;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

const TG_API: &str = "https://api.telegram.org/bot";
/// Server-side long-poll timeout (seconds). Client timeout = this + 5.
const POLL_SECS: u64 = 30;

/// Telegram Bot adapter. Wrap in `Arc` and pass to `ChannelRouter::register`.
pub struct TelegramAdapter {
    token: String,
    client: Client,
    /// `getUpdates` offset: next expected `update_id`. Atomic so `Arc<Self>`
    /// can advance it inside the `run` loop without a Mutex.
    offset: AtomicI64,
}

impl TelegramAdapter {
    /// Construct with a resolved bot token string.
    pub fn new(token: impl Into<String>) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(POLL_SECS + 5))
            .build()
            .expect("reqwest TLS client");
        Self {
            token: token.into(),
            client,
            offset: AtomicI64::new(0),
        }
    }

    fn url(&self, method: &str) -> String {
        format!("{TG_API}{}/{method}", self.token)
    }

    async fn poll_once(&self) -> Result<Vec<TgUpdate>> {
        let offset = self.offset.load(Ordering::Relaxed);
        let resp = self
            .client
            .get(self.url("getUpdates"))
            .query(&[
                ("offset", offset.to_string()),
                ("timeout", POLL_SECS.to_string()),
                ("allowed_updates", r#"["message"]"#.to_string()),
            ])
            .send()
            .await
            .wrap_err("telegram getUpdates request")?
            .json::<TgResponse<Vec<TgUpdate>>>()
            .await
            .wrap_err("telegram getUpdates decode")?;

        if !resp.ok {
            return Err(eyre::eyre!(
                "Telegram API error: {}",
                resp.description.as_deref().unwrap_or("unknown")
            ));
        }
        Ok(resp.result.unwrap_or_default())
    }
}

#[async_trait]
impl ChannelAdapter for TelegramAdapter {
    fn kind(&self) -> ChannelKind {
        ChannelKind::Telegram
    }
    fn name(&self) -> &str {
        "telegram"
    }

    async fn run(
        self: Arc<Self>,
        tx: tokio::sync::mpsc::Sender<InboundMessage>,
    ) -> eyre::Result<()> {
        loop {
            let updates = match self.poll_once().await {
                Ok(u) => u,
                Err(e) => {
                    tracing::warn!(error=?e, "telegram: poll failed, retrying in 5s");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };

            for upd in updates {
                // Acknowledge the update by advancing the offset past it.
                self.offset.fetch_max(upd.update_id + 1, Ordering::Relaxed);

                let Some(msg) = upd.message else { continue };
                let Some(text) = msg.text.filter(|t| !t.trim().is_empty()) else {
                    continue;
                };

                let inbound = InboundMessage {
                    channel: ChannelKind::Telegram,
                    conversation_id: msg.chat.id.to_string(),
                    sender_id: msg
                        .from
                        .as_ref()
                        .map(|u| u.id.to_string())
                        .unwrap_or_default(),
                    sender_name: msg.from.as_ref().map(|u| {
                        u.username
                            .as_deref()
                            .map(|n| format!("@{n}"))
                            .unwrap_or_else(|| u.first_name.clone())
                    }),
                    // DMs always count as self-mentions. Group @-mention
                    // detection would inspect msg.entities; simplified here.
                    mentions_self: true,
                    text,
                    received_at_ms: msg.date * 1000,
                };

                if tx.send(inbound).await.is_err() {
                    return Ok(()); // router dropped receiver — graceful exit
                }
            }
        }
    }

    async fn send(&self, msg: OutboundMessage) -> eyre::Result<()> {
        #[derive(serde::Serialize)]
        struct Body<'a> {
            chat_id: &'a str,
            text: &'a str,
            parse_mode: &'static str,
        }
        let resp = self
            .client
            .post(self.url("sendMessage"))
            .json(&Body {
                chat_id: &msg.conversation_id,
                text: &msg.text,
                parse_mode: "Markdown",
            })
            .send()
            .await
            .wrap_err("telegram sendMessage")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!(status = %status, body = %body, "telegram: sendMessage failed");
        }
        Ok(())
    }
}

// ── Telegram Bot API response types ──────────────────────────────────────────

#[derive(Deserialize)]
struct TgResponse<T> {
    ok: bool,
    description: Option<String>,
    result: Option<T>,
}

#[derive(Deserialize)]
struct TgUpdate {
    update_id: i64,
    message: Option<TgMessage>,
}

#[derive(Deserialize)]
struct TgMessage {
    date: i64,
    chat: TgChat,
    from: Option<TgUser>,
    text: Option<String>,
}

#[derive(Deserialize)]
struct TgChat {
    id: i64,
}

#[derive(Deserialize)]
struct TgUser {
    id: i64,
    first_name: String,
    username: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_metadata() {
        let a = TelegramAdapter::new("token");
        assert_eq!(a.kind(), ChannelKind::Telegram);
        assert_eq!(a.name(), "telegram");
    }

    #[test]
    fn url_format() {
        let a = TelegramAdapter::new("123:ABC");
        assert_eq!(
            a.url("getUpdates"),
            "https://api.telegram.org/bot123:ABC/getUpdates"
        );
    }
}
