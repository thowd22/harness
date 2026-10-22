//! `harness-eval`: Eval suite format, runner and baseline comparison.
//!
//! See `backlog/docs/doc-1 - Architecture-and-Problem-Map.md` for crate boundaries
//! and `docs/evals.md` for the user guide.

pub mod baseline;
pub mod cli;
pub mod defaults;
pub mod doctor;
pub mod memory_evals;
pub mod runner;
pub mod suite;
