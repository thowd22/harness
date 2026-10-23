//! Policy for MCP tool calls.
//!
//! An MCP tool call reaches the engine as `Action::Tool` named
//! `mcp:<server>/<tool>` ([`action_name`]). It is decided, in order, by:
//!
//! 1. `[policy] mcp.<server>.deny` in harness.ini: never runs.
//! 2. `[policy] mcp.<server>.ask`: asks (a session approval can allow it).
//! 3. `[policy] mcp.<server>.allow`: runs without asking.
//! 4. With `mcp.<server>.trust_annotations = true`, a tool the server marks
//!    `readOnlyHint: true` ([`ReadOnlyTools`]) runs without asking.
//! 5. Otherwise the tool may act outside the workspace (create issues,
//!    write databases, send messages), so it is an **outward action**: it
//!    asks in every autonomy mode, including `auto`, unless a session
//!    approval covers it.
//!
//! Session approvals for MCP tools are `mcp:<server>/<tool>` patterns with
//! `*` wildcards; "always" proposes `mcp.<server>.allow = <tool>` for
//! harness.ini. Only harness.ini can loosen MCP policy.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};

use harness_config::mcp_tools::{McpPolicy, McpToolRules, wildcard_match};

/// Prefix of the names MCP tools are offered to the model under
/// (`mcp__<server>__<tool>`).
pub const MODEL_TOOL_PREFIX: &str = "mcp__";

/// The search-and-call tool that stands in for MCP tools whose schemas
/// would exceed the context budget.
pub const GATEWAY_TOOL: &str = "mcp_tools";

/// True for a tool name offered to the model for MCP: its results come
/// from an MCP server and are untrusted.
pub fn is_model_tool_name(name: &str) -> bool {
    name.starts_with(MODEL_TOOL_PREFIX) || name == GATEWAY_TOOL
}

/// Prefix of the action name of an MCP tool call.
pub const MCP_ACTION_PREFIX: &str = "mcp:";

/// The action name the policy engine sees for `tool` of `server`:
/// `mcp:<server>/<tool>`.
pub fn action_name(server: &str, tool: &str) -> String {
    format!("{MCP_ACTION_PREFIX}{server}/{tool}")
}

/// Splits an [`action_name`] into server and tool.
pub fn parse_action_name(name: &str) -> Option<(&str, &str)> {
    let (server, tool) = name.strip_prefix(MCP_ACTION_PREFIX)?.split_once('/')?;
    (!server.is_empty() && !tool.is_empty()).then_some((server, tool))
}

/// MCP tools known to be read-only: the server marked them
/// `readOnlyHint: true`. Consulted only for servers with
/// `mcp.<server>.trust_annotations = true`. Shared with the MCP tool
/// registry, which updates it whenever a server's tool list is (re)read.
#[derive(Debug, Default)]
pub struct ReadOnlyTools(RwLock<BTreeMap<String, BTreeSet<String>>>);

impl ReadOnlyTools {
    /// Replaces what is known about `server`.
    pub fn set(&self, server: &str, tools: impl IntoIterator<Item = String>) {
        let mut m = self.0.write().unwrap_or_else(|e| e.into_inner());
        m.insert(server.to_owned(), tools.into_iter().collect());
    }

    /// True when `tool` of `server` is known to be read-only.
    pub fn contains(&self, server: &str, tool: &str) -> bool {
        self.0
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(server)
            .is_some_and(|s| s.contains(tool))
    }
}

/// The user's MCP rules plus the read-only knowledge.
#[derive(Debug, Clone, Default)]
pub struct McpRules {
    rules: McpPolicy,
    read_only: Arc<ReadOnlyTools>,
}

/// What [`McpRules::decide`] found, before session approvals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum McpVerdict {
    /// A deny rule matched (key, pattern).
    Deny(&'static str, String),
    /// An ask rule matched.
    Ask(&'static str, String),
    /// An allow rule matched.
    Allow(&'static str, String),
    /// Trusted read-only annotation.
    ReadOnly,
    /// No rule: outward default.
    Outward,
}

impl McpRules {
    /// Rules from `[policy] mcp.*`, sharing `read_only` with the MCP tool
    /// registry.
    pub fn new(rules: McpPolicy, read_only: Arc<ReadOnlyTools>) -> Self {
        Self { rules, read_only }
    }

    /// The read-only knowledge.
    pub fn read_only(&self) -> &Arc<ReadOnlyTools> {
        &self.read_only
    }

    /// The rules for `server`.
    pub fn server(&self, server: &str) -> Option<&McpToolRules> {
        self.rules.get(server)
    }

    pub(crate) fn decide(&self, server: &str, tool: &str) -> McpVerdict {
        let Some(r) = self.rules.get(server) else {
            return McpVerdict::Outward;
        };
        let find = |list: &[String]| list.iter().find(|p| wildcard_match(p, tool)).cloned();
        if let Some(p) = find(&r.deny) {
            return McpVerdict::Deny("deny", p);
        }
        if let Some(p) = find(&r.ask) {
            return McpVerdict::Ask("ask", p);
        }
        if let Some(p) = find(&r.allow) {
            return McpVerdict::Allow("allow", p);
        }
        if r.trust_annotations && self.read_only.contains(server, tool) {
            return McpVerdict::ReadOnly;
        }
        McpVerdict::Outward
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        let n = action_name("github", "create_issue");
        assert_eq!(n, "mcp:github/create_issue");
        assert_eq!(parse_action_name(&n), Some(("github", "create_issue")));
        assert_eq!(parse_action_name("mcp:github"), None);
        assert_eq!(parse_action_name("bash"), None);
        assert_eq!(parse_action_name("mcp:/x"), None);
    }

    #[test]
    fn deny_then_ask_then_allow_then_annotations_then_outward() {
        let mut p = McpPolicy::new();
        p.insert(
            "gh".into(),
            McpToolRules {
                allow: vec!["get_*".into(), "create_draft".into()],
                ask: vec!["create_*".into()],
                deny: vec!["delete_*".into(), "get_secret".into()],
                trust_annotations: true,
            },
        );
        let ro = Arc::new(ReadOnlyTools::default());
        ro.set("gh", ["list_repos".to_owned()]);
        ro.set("other", ["list_repos".to_owned()]);
        let r = McpRules::new(p, ro);
        assert_eq!(
            r.decide("gh", "delete_repo"),
            McpVerdict::Deny("deny", "delete_*".into())
        );
        assert_eq!(
            r.decide("gh", "get_secret"),
            McpVerdict::Deny("deny", "get_secret".into())
        );
        // ask beats allow.
        assert_eq!(
            r.decide("gh", "create_draft"),
            McpVerdict::Ask("ask", "create_*".into())
        );
        assert_eq!(
            r.decide("gh", "get_issue"),
            McpVerdict::Allow("allow", "get_*".into())
        );
        assert_eq!(r.decide("gh", "list_repos"), McpVerdict::ReadOnly);
        assert_eq!(r.decide("gh", "push"), McpVerdict::Outward);
        // Annotations are not trusted unless the user said so.
        assert_eq!(r.decide("other", "list_repos"), McpVerdict::Outward);
    }
}
