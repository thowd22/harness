//! `harness-config`: INI loader (`harness.ini`), secrets, YAML discovery/layering, schemas, trust store and lint.
//!
//! See `backlog/docs/doc-1 - Architecture-and-Problem-Map.md` for crate boundaries.

pub mod agents;
pub mod builtins;
pub mod discovery;
pub mod effective;
pub mod ini;
pub mod lint;
pub mod login;
pub mod mcp_tools;
pub mod paths;
pub mod prompts;
pub mod secrets;
pub mod skills;
pub mod trust;
pub mod yaml;
