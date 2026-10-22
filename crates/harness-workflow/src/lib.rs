//! `harness-workflow`: Workflow schema, templating, DAG executor, gates, artifacts and rerun/resume.
//!
//! See `backlog/docs/doc-1 - Architecture-and-Problem-Map.md` for crate boundaries
//! and `docs/workflows.md` for the user guide.

pub mod artifacts;
pub mod bundled;
pub mod commands;
pub mod executor;
pub mod gates;
pub mod placement;
pub mod remote;
pub mod rerun;
pub mod runners;
pub mod schema;
pub mod template;
