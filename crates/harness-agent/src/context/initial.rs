//! Initial-context measurement for the CI budget.
//!
//! The initial context of an agent is what every request of a fresh session
//! carries before any history: the system prompt, the tool definitions, the
//! memory index and the skill listing. The default agent must stay under
//! [`DEFAULT_INITIAL_BUDGET`] tokens (doc-1), which a test asserts in CI.

use harness_providers::model::ToolSpec;

use super::estimate::{estimate_text, estimate_tools};
use super::prefix::{LayerKind, SessionPrefix};

/// Budget for the default agent's initial context, in tokens.
pub const DEFAULT_INITIAL_BUDGET: u64 = 4000;

/// Estimated initial context, by layer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InitialContext {
    /// System prompt.
    pub system: u64,
    /// Tool definitions.
    pub tools: u64,
    /// Memory index.
    pub memory_index: u64,
    /// Skill listing.
    pub skills: u64,
}

impl InitialContext {
    /// Measures a frozen prefix and the session's tools.
    pub fn measure(prefix: &SessionPrefix, tools: &[ToolSpec]) -> Self {
        let layer = |k| prefix.layer(k).map_or(0, estimate_text);
        Self {
            system: layer(LayerKind::System),
            tools: estimate_tools(tools),
            memory_index: layer(LayerKind::MemoryIndex),
            skills: layer(LayerKind::Skills),
        }
    }

    /// Sum of the layers.
    pub fn total(&self) -> u64 {
        self.system + self.tools + self.memory_index + self.skills
    }

    /// `Ok` when the total is within `budget`, else the report as error.
    pub fn check(&self, budget: u64) -> Result<(), String> {
        if self.total() <= budget {
            Ok(())
        } else {
            Err(format!(
                "initial context over budget: {}",
                self.report(budget)
            ))
        }
    }

    /// One line with every layer, the total and the threshold.
    pub fn report(&self, budget: u64) -> String {
        format!(
            "initial context {} tokens (threshold {budget}): system {}, tools {}, memory index {}, skills {}",
            self.total(),
            self.system,
            self.tools,
            self.memory_index,
            self.skills
        )
    }
}
