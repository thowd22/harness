//! `harness-events`: Versioned event schema, run store (JSONL + blobs), redaction, reducers and log setup.
//!
//! See `backlog/docs/doc-1 - Architecture-and-Problem-Map.md` for crate boundaries.

pub mod blobs;
pub mod logging;
pub mod redact;
pub mod reducer;
pub mod schema;
pub mod store;
