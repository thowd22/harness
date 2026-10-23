//! `harness-policy`: Rules engine, shell parsing, protected paths, approvals and sandboxing.
//!
//! See `backlog/docs/doc-1 - Architecture-and-Problem-Map.md` for crate boundaries.

pub mod approvals;
pub mod mcp;
pub mod outward;
pub mod provenance;
pub mod rules;
pub mod sandbox;
pub mod shell;
pub mod staging;
