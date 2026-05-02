//! Deterministic mock provider for tests. Phase 1 dev-dep only.

use async_trait::async_trait;
use evo_providers::{ChatRequest, Provider, ProviderError, StreamEvent, ToolCall, Usage};
use futures::stream::{self, BoxStream, StreamExt};
use serde_json::Value;
use std::sync::Mutex;

pub enum Turn {
    FinalText(String),
    ToolCall { name: String, args: Value },
}

impl Turn {
    pub fn final_text(s: impl Into<String>) -> Self {
        Turn::FinalText(s.into())
    }
    pub fn tool_call(name: impl Into<String>, args: Value) -> Self {
        Turn::ToolCall {
            name: name.into(),
            args,
        }
    }
}

pub struct MockProvider {
    mode: Mode,
}

enum Mode {
    Scripted(Mutex<std::collections::VecDeque<Turn>>),
    Looping { name: String, args: Value },
}

impl MockProvider {
    pub fn scripted(turns: Vec<Turn>) -> Self {
        Self {
            mode: Mode::Scripted(Mutex::new(turns.into())),
        }
    }
    pub fn looping_tool_call(name: impl Into<String>, args: Value) -> Self {
        Self {
            mode: Mode::Looping {
                name: name.into(),
                args,
            },
        }
    }

    fn next_turn(&self) -> Turn {
        match &self.mode {
            Mode::Scripted(q) => q
                .lock()
                .unwrap()
                .pop_front()
                .expect("MockProvider scripted: ran past end of turns"),
            Mode::Looping { name, args } => Turn::ToolCall {
                name: name.clone(),
                args: args.clone(),
            },
        }
    }
}

#[async_trait]
impl Provider for MockProvider {
    async fn stream(
        &self,
        _req: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError> {
        let turn = self.next_turn();
        let events: Vec<StreamEvent> = match turn {
            Turn::FinalText(s) => vec![
                StreamEvent::Delta(s),
                StreamEvent::ToolCallFinish,
                StreamEvent::Usage(Usage {
                    input_tokens: 100,
                    cached_tokens: 60,
                    output_tokens: 10,
                }),
                StreamEvent::Done,
            ],
            Turn::ToolCall { name, args } => vec![
                StreamEvent::Delta(format!("<summary>calling {name}</summary>")),
                StreamEvent::ToolCallStart(ToolCall {
                    id: format!("call_{}", uuid_like()),
                    name,
                    arguments: args,
                }),
                StreamEvent::ToolCallFinish,
                StreamEvent::Usage(Usage {
                    input_tokens: 100,
                    cached_tokens: 60,
                    output_tokens: 10,
                }),
                StreamEvent::Done,
            ],
        };
        Ok(stream::iter(events.into_iter().map(Ok)).boxed())
    }
}

fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{nanos:x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use evo_providers::{Message, ToolPayload};
    use futures::StreamExt;

    fn req() -> ChatRequest {
        ChatRequest {
            model: "mock".into(),
            messages: vec![Message::user("ping")],
            tools: ToolPayload::Full(Vec::new()),
            max_tokens: 100,
            temperature: 0.0,
        }
    }

    #[tokio::test]
    async fn scripted_emits_final_text_in_order() {
        let p = MockProvider::scripted(vec![Turn::final_text("hello")]);
        let mut s = p.stream(req()).await.unwrap();
        let mut text = String::new();
        while let Some(e) = s.next().await {
            if let StreamEvent::Delta(t) = e.unwrap() {
                text.push_str(&t);
            }
        }
        assert_eq!(text, "hello");
    }

    #[tokio::test]
    async fn looping_tool_call_never_terminates_externally() {
        let p = MockProvider::looping_tool_call("read_file", serde_json::json!({"path": "x"}));
        for _ in 0..3 {
            let mut s = p.stream(req()).await.unwrap();
            let mut got = false;
            while let Some(e) = s.next().await {
                if let StreamEvent::ToolCallStart(_) = e.unwrap() {
                    got = true;
                }
            }
            assert!(got);
        }
    }
}
