//! In-band notices that providers attach to a response.
//!
//! The normalized [`StreamEvent`] set has no event for "reasoning happened
//! but is not visible", "the provider reported a cost" or "this is how big
//! the request body was", so providers emit these facts as [`StreamEvent::Opaque`] blocks
//! whose provider is [`NOTICE_PROVIDER`]. They land in the assistant
//! message like any other block, so both live consumers (from the stream)
//! and later consumers (from a [`Response`] or the event log) can read them
//! with [`Notice::from_event`] and [`Notice::from_block`].
//!
//! No provider sends a `harness` block back to a model: every provider only
//! round-trips opaque blocks carrying its own name.

use serde::{Deserialize, Serialize};

use crate::http::BodyStats;
use crate::model::{ContentBlock, Message};
use crate::provider::{Response, StreamEvent};

/// Provider name used on notice blocks.
pub const NOTICE_PROVIDER: &str = "harness";

/// Block index reserved for the [`Notice::RequestBody`] block, far above
/// any index a provider assigns, so it never collides and sorts last.
pub const REQUEST_BODY_INDEX: usize = u32::MAX as usize;

/// Why reasoning was not shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HiddenReason {
    /// The model reasoned, but only an encrypted blob (no summary or text)
    /// was returned. It is round-tripped but cannot be displayed.
    EncryptedOnly,
    /// Reasoning was requested or billed, but the upstream model sent no
    /// reasoning content at all.
    NotProvided,
}

/// A fact about a response that has no dedicated stream event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Notice {
    /// The model's reasoning is not exposed for this turn.
    ReasoningHidden {
        /// Provider that served the call, e.g. `openai`.
        provider: String,
        /// Why it is hidden.
        reason: HiddenReason,
    },
    /// The provider reported the exact cost of the call.
    Cost {
        /// Provider that reported it, e.g. `openrouter`.
        provider: String,
        /// Cost in US dollars.
        usd: f64,
    },
    /// Size of the request body that was sent for the call.
    RequestBody {
        /// Provider that sent it, e.g. `anthropic`.
        provider: String,
        /// Serialized JSON size in bytes.
        raw_bytes: u64,
        /// Bytes on the wire after `Content-Encoding` (equal to `raw_bytes`
        /// when sent uncompressed).
        wire_bytes: u64,
        /// `Content-Encoding` used, e.g. `gzip`; absent when sent raw.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encoding: Option<String>,
    },
}

impl Notice {
    /// The notice as an opaque content block.
    pub fn to_block(&self) -> ContentBlock {
        ContentBlock::Opaque {
            provider: NOTICE_PROVIDER.to_owned(),
            data: serde_json::to_value(self).unwrap_or_default(),
        }
    }

    /// The notice as a stream event at block `index`.
    pub fn to_event(&self, index: usize) -> StreamEvent {
        StreamEvent::Opaque {
            index,
            provider: NOTICE_PROVIDER.to_owned(),
            data: serde_json::to_value(self).unwrap_or_default(),
        }
    }

    /// Reads a notice from a content block, if it is one.
    pub fn from_block(block: &ContentBlock) -> Option<Self> {
        match block {
            ContentBlock::Opaque { provider, data } if provider == NOTICE_PROVIDER => {
                serde_json::from_value(data.clone()).ok()
            }
            _ => None,
        }
    }

    /// Reads a notice from a stream event, if it is one.
    pub fn from_event(event: &StreamEvent) -> Option<Self> {
        match event {
            StreamEvent::Opaque { provider, data, .. } if provider == NOTICE_PROVIDER => {
                serde_json::from_value(data.clone()).ok()
            }
            _ => None,
        }
    }
}

/// All notices in `message`, in block order.
pub fn notices(message: &Message) -> impl Iterator<Item = Notice> + '_ {
    message.content.iter().filter_map(Notice::from_block)
}

/// The provider-reported cost of `response` in USD, if one was reported.
pub fn reported_cost(response: &Response) -> Option<f64> {
    notices(&response.message).find_map(|n| match n {
        Notice::Cost { usd, .. } => Some(usd),
        _ => None,
    })
}

/// The raw and on-the-wire request body size recorded in `message`, if the
/// provider recorded one.
pub fn request_body(message: &Message) -> Option<BodyStats> {
    notices(message).find_map(|n| match n {
        Notice::RequestBody {
            raw_bytes,
            wire_bytes,
            encoding,
            ..
        } => Some(BodyStats {
            raw_bytes,
            wire_bytes,
            encoding,
        }),
        _ => None,
    })
}

/// Why reasoning was hidden in `response`, if it was.
pub fn reasoning_hidden(response: &Response) -> Option<HiddenReason> {
    notices(&response.message).find_map(|n| match n {
        Notice::ReasoningHidden { reason, .. } => Some(reason),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn notices_round_trip_through_blocks_and_events() {
        let n = Notice::Cost {
            provider: "openrouter".into(),
            usd: 0.25,
        };
        assert_eq!(Notice::from_block(&n.to_block()), Some(n.clone()));
        assert_eq!(Notice::from_event(&n.to_event(3)), Some(n));
        let h = Notice::ReasoningHidden {
            provider: "openai".into(),
            reason: HiddenReason::EncryptedOnly,
        };
        assert_eq!(
            serde_json::to_value(&h).unwrap(),
            json!({"type": "reasoning_hidden", "provider": "openai", "reason": "encrypted_only"})
        );
    }

    #[test]
    fn request_body_notice_round_trips_and_is_found() {
        let n = Notice::RequestBody {
            provider: "anthropic".into(),
            raw_bytes: 10_000,
            wire_bytes: 1_200,
            encoding: Some("gzip".into()),
        };
        assert_eq!(
            serde_json::to_value(&n).unwrap(),
            json!({"type": "request_body", "provider": "anthropic", "raw_bytes": 10000, "wire_bytes": 1200, "encoding": "gzip"})
        );
        let m = Message {
            role: crate::model::Role::Assistant,
            content: vec![ContentBlock::Text { text: "hi".into() }, n.to_block()],
        };
        assert_eq!(
            request_body(&m),
            Some(BodyStats {
                raw_bytes: 10_000,
                wire_bytes: 1_200,
                encoding: Some("gzip".into()),
            })
        );
        let raw = Notice::RequestBody {
            provider: "anthropic".into(),
            raw_bytes: 5,
            wire_bytes: 5,
            encoding: None,
        };
        assert!(!serde_json::to_string(&raw).unwrap().contains("encoding"));
    }

    #[test]
    fn foreign_opaque_blocks_are_not_notices() {
        let b = ContentBlock::Opaque {
            provider: "openai".into(),
            data: json!({"type": "cost", "provider": "x", "usd": 1.0}),
        };
        assert_eq!(Notice::from_block(&b), None);
    }
}
