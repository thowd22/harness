//! Slash-command registry and parser.
//!
//! One [`CommandRegistry`] holds every slash command: the built-ins
//! ([`Builtin`]), workflows (their `command`), skills and agent switches,
//! each with a name, description and argument spec ([`CommandSpec`]). Input
//! completion and the command palette both read it, so they always agree.
//!
//! - Names: a bare `/name` goes to the highest-priority command (built-in,
//!   then workflow, skill, agent; within a kind the project shadows global
//!   shadows built-in files). When the same name exists in several scopes,
//!   every one stays reachable as `/global:name`, `/project:name` or
//!   `/builtin:name`; when kinds collide within a scope, as `/skill:name`,
//!   `/workflow:name` or `/agent:name`.
//! - Arguments ([`parse`]): positional words, `key=value` pairs and single-
//!   or double-quoted values, with errors that show the usage line.
//! - Hot reload: [`CommandRegistry::reload`] rebuilds from a reloaded
//!   [`harness_config::discovery::Catalog`].

pub mod builtin;
pub mod parse;
pub mod registry;

pub use builtin::Builtin;
pub use parse::{ArgKind, ArgSpec, CommandError, Invocation, Token, split_line, tokenize};
pub use registry::{Collision, CommandKind, CommandRegistry, CommandSpec};

#[cfg(test)]
mod tests;
