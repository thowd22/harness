//! Request context assembly with a stable, cache-friendly prefix.
//!
//! - [`estimate`]: request size estimates and the per-node
//!   [`ContextTracker`] that calibrates them against provider-reported
//!   usage, so compaction decisions are made before a request is sent.
//! - [`prefix`]: the [`SessionPrefix`], frozen at session start: system
//!   prompt, then the memory index, then the skill listing (tools travel in
//!   the request's own field and are fixed for the session too).
//! - [`reasoning`]: how reasoning blocks are kept across turns.
//! - [`guard`]: the "harness handles context" prompt section and a checker
//!   that no request tells the model how much context is left.
//! - [`initial`]: the initial-context measurement asserted in CI.

pub mod estimate;
pub mod guard;
pub mod initial;
pub mod prefix;
pub mod reasoning;

pub use estimate::{
    ContextTracker, ContextUsage, RequestEstimate, cache_hit_rate_of, estimate_message,
    estimate_messages, estimate_request, estimate_text, estimate_tools,
};
pub use guard::{CONTEXT_PROMPT_SECTION, mentions_remaining_context};
pub use initial::{DEFAULT_INITIAL_BUDGET, InitialContext};
pub use prefix::{LAYER_SEPARATOR, LayerKind, PrefixLayer, SessionPrefix};
pub use reasoning::ReasoningRetention;

#[cfg(test)]
mod tests;
