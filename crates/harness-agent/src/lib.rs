//! `harness-agent`: Agent loop, tool dispatch, subagents, context management, compaction and budgets.
//!
//! See `backlog/docs/doc-1 - Architecture-and-Problem-Map.md` for crate boundaries.

pub mod agent_loop;
pub mod budget;
pub mod commands;
pub mod compaction;
pub mod context;
pub mod cost;
pub mod dispatch;
pub mod job_notices;
pub mod long_wait;
pub mod prewarm;
pub mod robustness;
pub mod scheduler;
pub mod session;
pub mod stagger;
pub mod subagents;
pub mod timing;
pub mod todo;
