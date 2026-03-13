//! Partial event assembly for streaming responses.
//!
//! When `--include-partial-messages` is enabled, the CLI emits raw API
//! streaming events (`message_start`, `content_block_start`,
//! `content_block_delta`, `content_block_stop`, `message_stop`).
//!
//! [`StreamAssembler`] accumulates these deltas and produces fully assembled
//! content blocks as they complete, making it easy to process streaming
//! output without manually tracking partial state.

use crate::types::{ContentBlock, ContentBlockInfo, Delta, StreamEventPayload};

/// Tracks the partial state of an in-progress content block.
#[derive(Debug, Clone)]
enum PartialBlock {
    /// Accumulating text deltas.
    Text { text: String },
    /// Accumulating thinking deltas.
    Thinking { thinking: String },
    /// Accumulating tool-use input JSON deltas.
    ToolUse {
        id: String,
        name: String,
        partial_json: String,
    },
}

/// Events emitted by the [`StreamAssembler`].
#[derive(Debug, Clone)]
pub enum AssembledEvent {
    /// The model has started a new message.
    MessageStart {
        /// The raw message metadata (id, model, role, etc.).
        metadata: serde_json::Value,
    },

    /// A content block has been fully assembled.
    ContentBlockComplete {
        /// Zero-based index of this block within the message.
        index: u64,
        /// The fully assembled content block.
        block: ContentBlock,
    },

    /// Incremental text — emitted for every `text_delta` so callers can
    /// stream text to the user in real time.
    TextDelta {
        /// Zero-based index of the content block.
        index: u64,
        /// The new text fragment.
        text: String,
    },

    /// Incremental thinking — emitted for every `thinking_delta`.
    ThinkingDelta {
        /// Zero-based index of the content block.
        index: u64,
        /// The new thinking fragment.
        thinking: String,
    },

    /// The entire message is complete.
    MessageComplete {
        /// The stop reason, if any (e.g. `"end_turn"`, `"tool_use"`).
        stop_reason: Option<String>,
    },
}

/// Assembles streaming events into complete content blocks.
///
/// Feed it [`StreamEventPayload`] values via [`process`](Self::process) and
/// collect the resulting [`AssembledEvent`]s.
///
/// # Example
///
/// ```rust,no_run
/// # use apiari_claude_sdk::streaming::{StreamAssembler, AssembledEvent};
/// # use apiari_claude_sdk::types::StreamEventPayload;
/// let mut assembler = StreamAssembler::new();
/// // ... for each stream event payload:
/// // let events = assembler.process(&payload);
/// // for event in events { /* handle */ }
/// ```
#[derive(Debug, Default)]
pub struct StreamAssembler {
    /// In-flight content blocks, keyed by index.
    blocks: Vec<Option<PartialBlock>>,
}

impl StreamAssembler {
    /// Create a new assembler with no state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset all internal state (e.g. between messages).
    pub fn reset(&mut self) {
        self.blocks.clear();
    }

    /// Process a single streaming event and return zero or more assembled events.
    pub fn process(&mut self, event: &StreamEventPayload) -> Vec<AssembledEvent> {
        match event {
            StreamEventPayload::MessageStart { message } => {
                self.reset();
                vec![AssembledEvent::MessageStart {
                    metadata: message.clone(),
                }]
            }

            StreamEventPayload::ContentBlockStart {
                index,
                content_block,
            } => {
                let idx = *index as usize;
                // Grow the blocks vec if needed.
                if self.blocks.len() <= idx {
                    self.blocks.resize_with(idx + 1, || None);
                }
                self.blocks[idx] = Some(match content_block {
                    ContentBlockInfo::Text { text } => PartialBlock::Text { text: text.clone() },
                    ContentBlockInfo::Thinking { thinking } => PartialBlock::Thinking {
                        thinking: thinking.clone(),
                    },
                    ContentBlockInfo::ToolUse { id, name, .. } => PartialBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        partial_json: String::new(),
                    },
                });
                vec![]
            }

            StreamEventPayload::ContentBlockDelta { index, delta } => {
                let idx = *index as usize;
                let mut events = Vec::new();

                if let Some(Some(partial)) = self.blocks.get_mut(idx) {
                    match (partial, delta) {
                        (PartialBlock::Text { text }, Delta::TextDelta { text: fragment }) => {
                            text.push_str(fragment);
                            events.push(AssembledEvent::TextDelta {
                                index: *index,
                                text: fragment.clone(),
                            });
                        }
                        (
                            PartialBlock::Thinking { thinking },
                            Delta::ThinkingDelta { thinking: fragment },
                        ) => {
                            thinking.push_str(fragment);
                            events.push(AssembledEvent::ThinkingDelta {
                                index: *index,
                                thinking: fragment.clone(),
                            });
                        }
                        (
                            PartialBlock::ToolUse { partial_json, .. },
                            Delta::InputJsonDelta {
                                partial_json: fragment,
                            },
                        ) => {
                            partial_json.push_str(fragment);
                        }
                        _ => {
                            // Mismatched delta/block type — ignore gracefully.
                        }
                    }
                }

                events
            }

            StreamEventPayload::ContentBlockStop { index } => {
                let idx = *index as usize;
                let mut events = Vec::new();

                if let Some(partial) = self.blocks.get_mut(idx).and_then(Option::take) {
                    let block = match partial {
                        PartialBlock::Text { text } => ContentBlock::Text { text },
                        PartialBlock::Thinking { thinking } => {
                            // The signature is typically in the final event; we
                            // don't receive it via deltas, so we leave it empty.
                            ContentBlock::Thinking {
                                thinking,
                                signature: String::new(),
                            }
                        }
                        PartialBlock::ToolUse {
                            id,
                            name,
                            partial_json,
                        } => {
                            let input = serde_json::from_str(&partial_json)
                                .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                            ContentBlock::ToolUse { id, name, input }
                        }
                    };
                    events.push(AssembledEvent::ContentBlockComplete {
                        index: *index,
                        block,
                    });
                }

                events
            }

            StreamEventPayload::MessageDelta { delta, .. } => {
                // We extract stop_reason here but don't emit MessageComplete
                // until MessageStop.
                let _stop_reason = delta
                    .get("stop_reason")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                vec![]
            }

            StreamEventPayload::MessageStop => {
                vec![AssembledEvent::MessageComplete { stop_reason: None }]
            }

            StreamEventPayload::Unknown => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assemble_text_block() {
        let mut asm = StreamAssembler::new();

        // message_start
        let events = asm.process(&StreamEventPayload::MessageStart {
            message: serde_json::json!({"id": "msg_1", "role": "assistant"}),
        });
        assert!(matches!(events[0], AssembledEvent::MessageStart { .. }));

        // content_block_start
        let events = asm.process(&StreamEventPayload::ContentBlockStart {
            index: 0,
            content_block: ContentBlockInfo::Text {
                text: String::new(),
            },
        });
        assert!(events.is_empty());

        // content_block_delta (text)
        let events = asm.process(&StreamEventPayload::ContentBlockDelta {
            index: 0,
            delta: Delta::TextDelta {
                text: "Hello".to_owned(),
            },
        });
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], AssembledEvent::TextDelta { text, .. } if text == "Hello"));

        let events = asm.process(&StreamEventPayload::ContentBlockDelta {
            index: 0,
            delta: Delta::TextDelta {
                text: " world".to_owned(),
            },
        });
        assert_eq!(events.len(), 1);

        // content_block_stop
        let events = asm.process(&StreamEventPayload::ContentBlockStop { index: 0 });
        assert_eq!(events.len(), 1);
        match &events[0] {
            AssembledEvent::ContentBlockComplete { block, .. } => match block {
                ContentBlock::Text { text } => assert_eq!(text, "Hello world"),
                other => panic!("expected Text, got {other:?}"),
            },
            other => panic!("expected ContentBlockComplete, got {other:?}"),
        }

        // message_stop
        let events = asm.process(&StreamEventPayload::MessageStop);
        assert!(matches!(events[0], AssembledEvent::MessageComplete { .. }));
    }

    #[test]
    fn assemble_tool_use_block() {
        let mut asm = StreamAssembler::new();

        asm.process(&StreamEventPayload::MessageStart {
            message: serde_json::json!({}),
        });

        asm.process(&StreamEventPayload::ContentBlockStart {
            index: 0,
            content_block: ContentBlockInfo::ToolUse {
                id: "tu_1".to_owned(),
                name: "Bash".to_owned(),
                input: serde_json::Value::Object(serde_json::Map::new()),
            },
        });

        asm.process(&StreamEventPayload::ContentBlockDelta {
            index: 0,
            delta: Delta::InputJsonDelta {
                partial_json: r#"{"command":"#.to_owned(),
            },
        });
        asm.process(&StreamEventPayload::ContentBlockDelta {
            index: 0,
            delta: Delta::InputJsonDelta {
                partial_json: r#""ls -la"}"#.to_owned(),
            },
        });

        let events = asm.process(&StreamEventPayload::ContentBlockStop { index: 0 });
        assert_eq!(events.len(), 1);
        match &events[0] {
            AssembledEvent::ContentBlockComplete { block, .. } => match block {
                ContentBlock::ToolUse { id, name, input } => {
                    assert_eq!(id, "tu_1");
                    assert_eq!(name, "Bash");
                    assert_eq!(input["command"], "ls -la");
                }
                other => panic!("expected ToolUse, got {other:?}"),
            },
            other => panic!("expected ContentBlockComplete, got {other:?}"),
        }
    }
}
