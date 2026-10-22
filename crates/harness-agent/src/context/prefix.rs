//! The session's frozen, cache-friendly prompt prefix.
//!
//! Requests are laid out so that everything that does not change during a
//! session comes first, in this order:
//!
//! 1. the rendered system prompt,
//! 2. the tool definitions (the request's `tools` field; the tool set is
//!    fixed for the session),
//! 3. the memory index,
//! 4. the skill listing,
//! 5. the history.
//!
//! Layers 1, 3 and 4 form the system text built once by
//! [`SessionPrefix::freeze`]. They are never recomputed mid-session: memory
//! writes and skill hot reloads take effect in the next session, so the
//! prefix bytes (and the provider's prompt cache) stay valid for every
//! turn. Providers put their cache breakpoints at the layer boundaries they
//! support: the end of the tools and the end of this system text (which
//! covers all frozen layers, since they only ever change together), then the
//! history.

use sha2::{Digest, Sha256};

/// Separator between layers of the system text.
pub const LAYER_SEPARATOR: &str = "\n\n";

/// A layer of the frozen prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LayerKind {
    /// The rendered system prompt.
    System,
    /// The memory index (`MEMORY.md` plus project notes).
    MemoryIndex,
    /// The model-invocable skill listing.
    Skills,
}

impl LayerKind {
    /// `system`, `memory_index` or `skills`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::MemoryIndex => "memory_index",
            Self::Skills => "skills",
        }
    }
}

/// One layer and where it sits in [`SessionPrefix::text`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefixLayer {
    /// Which layer.
    pub kind: LayerKind,
    /// Byte range in the system text.
    pub range: std::ops::Range<usize>,
}

/// The frozen system text of a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionPrefix {
    text: String,
    layers: Vec<PrefixLayer>,
    sha256: String,
}

impl SessionPrefix {
    /// Freezes the layers. Empty layers are left out.
    pub fn freeze(
        system: impl Into<String>,
        memory_index: Option<&str>,
        skills: Option<&str>,
    ) -> Self {
        let system = system.into();
        let mut text = String::new();
        let mut layers = Vec::new();
        for (kind, part) in [
            (LayerKind::System, Some(system.as_str())),
            (LayerKind::MemoryIndex, memory_index),
            (LayerKind::Skills, skills),
        ] {
            let Some(part) = part.map(str::trim_end).filter(|p| !p.trim().is_empty()) else {
                continue;
            };
            if !text.is_empty() {
                text.push_str(LAYER_SEPARATOR);
            }
            let start = text.len();
            text.push_str(part);
            layers.push(PrefixLayer {
                kind,
                range: start..text.len(),
            });
        }
        let sha256 = hex::encode(Sha256::digest(text.as_bytes()));
        Self {
            text,
            layers,
            sha256,
        }
    }

    /// The system text to put in `Conversation::system`.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The layers in order.
    pub fn layers(&self) -> &[PrefixLayer] {
        &self.layers
    }

    /// The text of one layer.
    pub fn layer(&self, kind: LayerKind) -> Option<&str> {
        self.layers
            .iter()
            .find(|l| l.kind == kind)
            .map(|l| &self.text[l.range.clone()])
    }

    /// sha256 of the text (matches `system_sha256` in the event log).
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
}
