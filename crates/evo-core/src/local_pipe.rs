//! Reference `ChannelAdapter` that speaks line-delimited JSON over
//! stdin/stdout. Used as the smoke-test adapter for `evo channel run
//! --kind local-pipe` and as a worked example for plugin authors.

use crate::channel::{ChannelAdapter, ChannelKind, InboundMessage, OutboundMessage};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub struct LocalPipe;

#[async_trait]
impl ChannelAdapter for LocalPipe {
    fn kind(&self) -> ChannelKind {
        ChannelKind::LocalPipe
    }
    fn name(&self) -> &str {
        "local-pipe"
    }

    async fn run(
        self: Arc<Self>,
        tx: tokio::sync::mpsc::Sender<InboundMessage>,
    ) -> eyre::Result<()> {
        let stdin = tokio::io::stdin();
        let mut lines = BufReader::new(stdin).lines();
        while let Some(line) = lines.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<InboundMessage>(&line) {
                Ok(m) => {
                    if tx.send(m).await.is_err() {
                        // receiver dropped — graceful shutdown
                        break;
                    }
                }
                Err(e) => {
                    tracing::warn!(error=?e, line=%line, "local-pipe: malformed inbound JSON")
                }
            }
        }
        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> eyre::Result<()> {
        let line = serde_json::to_string(&msg)?;
        let mut out = tokio::io::stdout();
        out.write_all(line.as_bytes()).await?;
        out.write_all(b"\n").await?;
        out.flush().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::OutboundKind;

    #[test]
    fn metadata_matches_kind() {
        let lp = LocalPipe;
        assert_eq!(lp.kind(), ChannelKind::LocalPipe);
        assert_eq!(lp.name(), "local-pipe");
    }

    #[tokio::test]
    async fn outbound_serializes_to_one_line() {
        // We don't easily redirect stdout here, but we can at least exercise
        // the JSON encoding path that `send()` uses.
        let msg = OutboundMessage {
            conversation_id: "c1".into(),
            text: "hello\nworld".into(),
            kind: OutboundKind::Reply,
        };
        let line = serde_json::to_string(&msg).unwrap();
        assert!(!line.contains('\n'), "JSON line must be single-line");
        let back: OutboundMessage = serde_json::from_str(&line).unwrap();
        assert_eq!(back.text, "hello\nworld");
    }
}
