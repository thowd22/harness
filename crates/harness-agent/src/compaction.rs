//! Tool-result elision and structured-handoff compaction.
//!
//! - [`elide`]: old, large tool results are swapped for a short stub that
//!   points at the full text in the blob store (`spill:<id>`, readable with
//!   the `read` tool). Elision runs in batches so the cached prefix changes
//!   at most once every `elide_after_turns` turns.
//! - [`handoff`]: at `compact_at` (or on `/compact`) the older history is
//!   summarized by the model into a [`Handoff`] (goal, decisions, current
//!   state, files touched, next steps, open questions); the latest
//!   `keep_recent_turns` turns stay verbatim. Each compaction is recorded as
//!   a `compaction` child node of the agent, with its own model call and
//!   cost, so it shows in the map and node detail.
//! - [`manager`]: the [`ContextManager`] request hook that ties tracking,
//!   reasoning retention, elision and compaction together. With
//!   [`ContextSettings::background_at`] (`[context] background_compaction`)
//!   the summary starts earlier on its own task and is swapped in at
//!   `compact_at`; a failed, cancelled or outdated one falls back to
//!   in-line compaction.

pub mod elide;
pub mod handoff;
pub mod manager;

pub use elide::{ELIDED_PREFIX, ElidedResult, Elider, ElisionReport};
pub use handoff::{
    COMPACTION_NODE, CompactionError, CompactionReport, Compactor, HANDOFF_OPEN, HANDOFF_SECTIONS,
    Handoff,
};
pub use manager::{ContextManager, ContextSettings};
