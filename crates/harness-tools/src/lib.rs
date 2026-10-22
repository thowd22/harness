//! `harness-tools`: `Workspace`/`Exec` traits, local implementation, the core tools (read/write/edit/bash) and output spill.
//!
//! See `backlog/docs/doc-1 - Architecture-and-Problem-Map.md` for crate boundaries
//! and `docs/tools.md` for user-facing behaviour.
//!
//! Layering:
//! - [`workspace`] / [`exec`]: host abstraction; [`local`] implements it and
//!   [`conformance`] is the shared suite every implementation must pass.
//! - [`tool`]: context, output, errors and events shared by all tools.
//! - [`read`], [`write`](mod@write), [`edit`], [`bash`]: the four core tools.
//! - [`tracker`]: stale-read detection; [`spill`]: output budget.

pub mod bash;
pub mod config;
pub mod conformance;
pub mod edit;
pub mod exec;
pub mod local;
pub mod read;
pub mod spill;
pub mod tool;
pub mod tracker;
pub mod workspace;
pub mod write;
