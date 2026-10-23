//! Selecting and governing MCP tools.
//!
//! Agent YAML picks MCP tools explicitly in `tools:` with
//! `mcp:<server>/<tool>` entries ([`McpSelector`]); `<tool>` may use `*`
//! wildcards (`mcp:db/*`, `mcp:github/create_*`). Nothing from an MCP
//! server reaches a model unless an entry selects it, and `<server>` must be
//! a `[mcp.<server>]` section of harness.ini (checked by
//! [`crate::agents::validate`]).
//!
//! harness.ini governs calls with `[policy] mcp.<server>.allow`,
//! `mcp.<server>.ask` and `mcp.<server>.deny` ([`McpToolRules`]): lists of
//! tool-name wildcards, and `mcp.<server>.trust_annotations`.

use std::collections::BTreeMap;

use serde::Serialize;

/// Prefix of an MCP entry in an agent's `tools:` list.
pub const MCP_TOOL_PREFIX: &str = "mcp:";

/// `*` wildcard match (`*` matches any run of characters, including none);
/// every other character matches itself, case-sensitively.
pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// True for characters allowed in a tool pattern: the MCP tool-name
/// characters (letters, digits, `_`, `-`, `.`) plus `*`.
fn valid_tool_pattern(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '*'))
}

/// One `mcp:<server>/<tool>` entry of an agent's `tools:` list.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct McpSelector {
    /// The `[mcp.<server>]` name.
    pub server: String,
    /// Tool name or `*` wildcard pattern.
    pub tool: String,
}

impl McpSelector {
    /// Parses `mcp:<server>/<tool>`.
    pub fn parse(entry: &str) -> Result<Self, String> {
        let rest = entry.strip_prefix(MCP_TOOL_PREFIX).ok_or_else(|| {
            format!("`{entry}` is not an MCP tool; write it as `mcp:<server>/<tool>`")
        })?;
        let Some((server, tool)) = rest.split_once('/') else {
            return Err(format!(
                "`{entry}` needs a tool after the server: `mcp:{rest}/<tool>`, or `mcp:{rest}/*` for every tool"
            ));
        };
        if !crate::ini::mcp::valid_server_name(server) {
            return Err(format!(
                "`{entry}`: `{server}` is not a valid MCP server name (letters, digits, `_` and `-`)"
            ));
        }
        if !valid_tool_pattern(tool) {
            return Err(format!(
                "`{entry}`: `{tool}` is not a valid tool name or pattern (letters, digits, `_`, `-`, `.` and `*`)"
            ));
        }
        Ok(Self {
            server: server.to_owned(),
            tool: tool.to_owned(),
        })
    }

    /// True when this entry selects `tool` of `server`.
    pub fn matches(&self, server: &str, tool: &str) -> bool {
        self.server == server && wildcard_match(&self.tool, tool)
    }

    /// True when the tool part has a wildcard.
    pub fn is_pattern(&self) -> bool {
        self.tool.contains('*')
    }
}

impl std::fmt::Display for McpSelector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{MCP_TOOL_PREFIX}{}/{}", self.server, self.tool)
    }
}

impl Serialize for McpSelector {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

/// `[policy] mcp.<server>.allow / ask / deny` for one server.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct McpToolRules {
    /// Tools that run without asking (unless a deny or ask rule matches).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
    /// Tools that always need approval.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ask: Vec<String>,
    /// Tools that never run.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub deny: Vec<String>,
    /// Believe the server's tool annotations: tools it marks
    /// `readOnlyHint: true` run without asking. Default false (MCP
    /// annotations are untrusted unless the user trusts the server).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub trust_annotations: bool,
}

/// Every `[policy] mcp.*` rule, by server name.
pub type McpPolicy = BTreeMap<String, McpToolRules>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards() {
        assert!(wildcard_match("*", ""));
        assert!(wildcard_match("create_*", "create_issue"));
        assert!(!wildcard_match("create_*", "list_issues"));
        assert!(wildcard_match("*_issue*", "create_issue_comment"));
        assert!(wildcard_match("a*b*c", "aXXbYYc"));
        assert!(!wildcard_match("a*b*c", "aXXbYY"));
        assert!(wildcard_match("exact", "exact"));
        assert!(!wildcard_match("exact", "Exact"));
    }

    #[test]
    fn selectors_parse_and_match() {
        let s = McpSelector::parse("mcp:github/create_issue").unwrap();
        assert_eq!(s.server, "github");
        assert!(s.matches("github", "create_issue"));
        assert!(!s.matches("gitlab", "create_issue"));
        assert!(!s.is_pattern());
        assert_eq!(s.to_string(), "mcp:github/create_issue");
        let all = McpSelector::parse("mcp:db/*").unwrap();
        assert!(all.is_pattern());
        assert!(all.matches("db", "query"));
        for bad in [
            "github/x",
            "mcp:github",
            "mcp:/x",
            "mcp:git hub/x",
            "mcp:github/",
            "mcp:github/a b",
        ] {
            assert!(McpSelector::parse(bad).is_err(), "{bad}");
        }
        assert!(
            McpSelector::parse("mcp:github")
                .unwrap_err()
                .contains("mcp:github/*")
        );
    }

    #[test]
    fn policy_rules_and_schema_budget_come_from_harness_ini() {
        let c = crate::ini::load_str(
            "[policy]\nbash.allow = ls*\nmcp.github.ask = create_*, delete_*\nmcp.github.allow = get_*\n\
             mcp.github.trust_annotations = true\nmcp.db.deny = drop_*\n[context]\nmcp_schema_budget = 500\n",
            None,
        )
        .unwrap();
        let gh = &c.policy.mcp["github"];
        assert_eq!(gh.ask, ["create_*", "delete_*"]);
        assert_eq!(gh.allow, ["get_*"]);
        assert!(gh.deny.is_empty());
        assert!(gh.trust_annotations);
        assert_eq!(c.policy.mcp["db"].deny, ["drop_*"]);
        assert_eq!(c.context.mcp_schema_budget, 500);
        assert_eq!(c.policy.bash_allow, ["ls*"]);

        let d = crate::ini::Config::defaults();
        assert_eq!(
            d.context.mcp_schema_budget,
            crate::ini::config::DEFAULT_MCP_SCHEMA_BUDGET
        );
        assert!(d.policy.mcp.is_empty());
        // Defaults serialize as before, so config hashes do not change.
        let json = serde_json::to_value(&d).unwrap();
        assert!(json["policy"].get("mcp").is_none());
        assert!(json["context"].get("mcp_schema_budget").is_none());

        for (text, want) in [
            ("[policy]\nmcp.github = x\n", "mcp.<server>.allow"),
            ("[policy]\nmcp.git:hub.ask = x\n", "not an MCP server name"),
            (
                "[policy]\nmcp.github.permit = x\n",
                "unknown MCP rule `permit`",
            ),
            ("[policy]\nmcp.github.trust_annotations = maybe\n", "true"),
        ] {
            let e = crate::ini::load_str(text, None).unwrap_err().to_string();
            assert!(e.contains(want), "{text}: {e}");
        }
    }
}
