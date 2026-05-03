//! Channel router — fan-in from any number of `ChannelAdapter`s into a
//! single `ConversationRuntime` and fan-out replies via the originating
//! adapter. Designed for an in-process plugin model; cross-process IPC
//! (sandboxed plugins) can be layered on later.

use crate::channel::{ChannelAdapter, ChannelKind, InboundMessage, OutboundMessage};
use std::collections::HashMap;
use std::sync::Arc;

pub struct ChannelRouter {
    adapters: HashMap<ChannelKind, Arc<dyn ChannelAdapter>>,
}

impl ChannelRouter {
    pub fn new() -> Self {
        Self {
            adapters: HashMap::new(),
        }
    }

    pub fn register(&mut self, adapter: Arc<dyn ChannelAdapter>) {
        self.adapters.insert(adapter.kind(), adapter);
    }

    pub fn list(&self) -> Vec<(ChannelKind, String)> {
        self.adapters
            .iter()
            .map(|(k, a)| (k.clone(), a.name().into()))
            .collect()
    }

    /// Run all registered adapters concurrently. Inbound messages are pushed
    /// into `inbound_tx`. Caller is responsible for consuming and dispatching.
    pub async fn run_all(
        self,
        inbound_tx: tokio::sync::mpsc::Sender<InboundMessage>,
    ) -> eyre::Result<()> {
        let mut handles = Vec::new();
        for (_kind, adapter) in self.adapters {
            let tx = inbound_tx.clone();
            handles.push(tokio::spawn(async move { adapter.run(tx).await }));
        }
        for h in handles {
            match h.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::warn!(error=?e, "channel adapter exited with error"),
                Err(e) => tracing::warn!(error=?e, "channel adapter task panicked"),
            }
        }
        Ok(())
    }

    pub async fn send_via(&self, kind: &ChannelKind, msg: OutboundMessage) -> eyre::Result<()> {
        match self.adapters.get(kind) {
            Some(a) => a.send(msg).await,
            None => Err(eyre::eyre!("no adapter for {:?}", kind)),
        }
    }
}

impl Default for ChannelRouter {
    fn default() -> Self {
        Self::new()
    }
}

/// Mention-policy enforcement: channel senders are hard-capped at P4 per the
/// permission ladder. Use this to filter inbound messages from group chats
/// where EvoClaw is not @-mentioned.
pub fn should_handle(msg: &InboundMessage) -> bool {
    msg.mentions_self
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::{ChannelKind, OutboundKind};
    use async_trait::async_trait;
    use std::sync::Mutex;

    struct DummyAdapter {
        kind: ChannelKind,
        name: &'static str,
        sent: Mutex<Vec<OutboundMessage>>,
    }

    #[async_trait]
    impl ChannelAdapter for DummyAdapter {
        fn kind(&self) -> ChannelKind {
            self.kind.clone()
        }
        fn name(&self) -> &str {
            self.name
        }
        async fn run(
            self: Arc<Self>,
            _tx: tokio::sync::mpsc::Sender<InboundMessage>,
        ) -> eyre::Result<()> {
            Ok(())
        }
        async fn send(&self, msg: OutboundMessage) -> eyre::Result<()> {
            self.sent.lock().unwrap().push(msg);
            Ok(())
        }
    }

    #[test]
    fn should_handle_respects_mention() {
        let m = InboundMessage {
            channel: ChannelKind::LocalPipe,
            conversation_id: "c".into(),
            sender_id: "u".into(),
            sender_name: None,
            mentions_self: false,
            text: "hi".into(),
            received_at_ms: 0,
        };
        assert!(!should_handle(&m));
        let m2 = InboundMessage {
            mentions_self: true,
            ..m
        };
        assert!(should_handle(&m2));
    }

    #[tokio::test]
    async fn register_and_send_via() {
        let mut router = ChannelRouter::new();
        let dummy = Arc::new(DummyAdapter {
            kind: ChannelKind::LocalPipe,
            name: "dummy",
            sent: Mutex::new(Vec::new()),
        });
        router.register(dummy.clone());
        assert_eq!(router.list().len(), 1);
        router
            .send_via(
                &ChannelKind::LocalPipe,
                OutboundMessage {
                    conversation_id: "c".into(),
                    text: "ok".into(),
                    kind: OutboundKind::Reply,
                },
            )
            .await
            .unwrap();
        assert_eq!(dummy.sent.lock().unwrap().len(), 1);
        assert!(router
            .send_via(
                &ChannelKind::Telegram,
                OutboundMessage {
                    conversation_id: "c".into(),
                    text: "x".into(),
                    kind: OutboundKind::Reply,
                },
            )
            .await
            .is_err());
    }
}
