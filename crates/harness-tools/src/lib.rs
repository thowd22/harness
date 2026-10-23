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
//! - [`grep`], [`glob`]: read-only search tools; [`search`] is their engine.
//! - [`tracker`]: stale-read detection; [`spill`]: output budget.
//! - [`liveness`]: cheap CPU/process sampling of a running process group.
//! - [`jobs`]: background bash jobs and the `bash_jobs` tool.

pub mod bash;
pub mod config;
pub mod conformance;
pub mod edit;
pub mod exec;
pub mod glob;
pub mod grep;
pub mod jobs;
pub mod liveness;
pub mod local;
pub mod read;
pub mod search;
pub mod spill;
pub mod tool;
pub mod tracker;
pub mod workspace;
pub mod write;
