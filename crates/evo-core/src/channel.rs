//! Channel adapter trait & types — plug-in surface for Telegram / Slack /
//! Discord / IM bots etc. EvoClaw routes inbound channel messages through
//! the same `ConversationRuntime` and posts the result back via the adapter.

use async_trait::async_trait;
use std::sync::Arc;

/// Identifies which channel an inbound/outbound message belongs to.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ChannelKind {
    Telegram,
    Slack,
    Discord,
    Line,
    Messenger,
    /// Local pipe (stdin/stdout) — used for testing and as a default adapter.
    LocalPipe,
    /// Custom name; lets users wire arbitrary adapters.
    Custom(String),
}

/// One inbound message from a channel.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct InboundMessage {
    pub channel: ChannelKind,
    /// Channel-native conversation/chat id.
    pub conversation_id: String,
    /// Channel-native sender id (user id, @handle, etc.).
    pub sender_id: String,
    /// Channel-native sender display name (best-effort).
    pub sender_name: Option<String>,
    /// Whether the message @-mentions EvoClaw. Channels that don't have
    /// mentions (DMs, LocalPipe) should set true.
    pub mentions_self: bool,
    pub text: String,
    /// Unix millis.
    pub received_at_ms: i64,
}

/// Reply EvoClaw posts back to the channel.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct OutboundMessage {
    pub conversation_id: String,
    pub text: String,
    pub kind: OutboundKind,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum OutboundKind {
    Reply,
    Notice,
    Error,
}

/// Adapter trait — implementors do all channel-specific I/O.
/// Adapters must be `Send + Sync` and cheap to clone (typically `Arc<inner>`).
#[async_trait]
pub trait ChannelAdapter: Send + Sync {
    fn kind(&self) -> ChannelKind;
    /// Adapter human name shown in `evo channel list`.
    fn name(&self) -> &str;
    /// Start polling/streaming inbound messages. Adapter should push each
    /// inbound message into the supplied sender. Loops forever until the
    /// underlying transport closes; should return `Ok(())` on graceful close.
    async fn run(
        self: Arc<Self>,
        tx: tokio::sync::mpsc::Sender<InboundMessage>,
    ) -> eyre::Result<()>;
    /// Send a reply back to the channel.
    async fn send(&self, msg: OutboundMessage) -> eyre::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-time check that `ChannelAdapter` is object-safe.
    #[allow(dead_code)]
    fn _f(_: Box<dyn ChannelAdapter>) {}

    #[test]
    fn channel_kind_roundtrips_json() {
        let k = ChannelKind::Custom("matrix".into());
        let s = serde_json::to_string(&k).unwrap();
        let back: ChannelKind = serde_json::from_str(&s).unwrap();
        assert_eq!(k, back);
    }

    #[test]
    fn inbound_outbound_roundtrip() {
        let m = InboundMessage {
            channel: ChannelKind::LocalPipe,
            conversation_id: "c1".into(),
            sender_id: "u1".into(),
            sender_name: Some("alice".into()),
            mentions_self: true,
            text: "hi".into(),
            received_at_ms: 1_700_000_000_000,
        };
        let s = serde_json::to_string(&m).unwrap();
        let back: InboundMessage = serde_json::from_str(&s).unwrap();
        assert_eq!(back.text, "hi");

        let o = OutboundMessage {
            conversation_id: "c1".into(),
            text: "ok".into(),
            kind: OutboundKind::Reply,
        };
        let s = serde_json::to_string(&o).unwrap();
        let back: OutboundMessage = serde_json::from_str(&s).unwrap();
        assert_eq!(back.text, "ok");
    }
}
