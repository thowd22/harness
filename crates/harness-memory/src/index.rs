//! Memory index and search.
//!
//! - [`SessionIndex`]: the always-loaded part of memory (`MEMORY.md` plus the
//!   current project's note index), capped at `[memory] index_max_tokens`
//!   and frozen for the whole session so the prompt prefix stays
//!   cache-stable.
//! - [`SearchIndex`]: BM25 over note bullets, updated incrementally (only
//!   notes whose content hash changed are re-tokenised) and cached in
//!   `.harness/search-index.json`.
//! - [`UsageLog`]: every read is recorded (note, run, time) in
//!   `.harness/usage.jsonl` for Dreaming to prune unused notes.

mod bm25;
mod session;
mod usage;

pub use bm25::{
    BM25_B, BM25_K1, DocEntry, INDEX_FILE, IndexStats, MAX_SNIPPETS, SearchHit, SearchIndex,
    tokenize,
};
pub use session::{SessionIndex, TRUNCATION_NOTICE, estimate_tokens, project_section};
pub use usage::{USAGE_FILE, UsageKind, UsageLog, UsageRecord, UsageSummary};
