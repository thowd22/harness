//! `harness-hygiene`: Change ledger, checkpoints, scope, hygiene report, verify runner and review.
//!
//! See `backlog/docs/doc-1 - Architecture-and-Problem-Map.md` for crate boundaries
//! and `docs/hygiene.md` for user-facing behaviour.

pub mod checkpoints;
pub mod events;
pub mod ledger;
pub mod proof;
pub mod report;
pub mod review;
pub mod scope;
pub mod snapshot;
pub mod verify;
