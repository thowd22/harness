//! The built-in slash commands.

use super::parse::ArgSpec;

/// A built-in command. The core (TUI or binary) carries out the action;
/// this type names it, describes it and declares its arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Builtin {
    /// `/help [command]`.
    Help,
    /// `/new`.
    New,
    /// `/resume [session]`.
    Resume,
    /// `/rename <name...>`.
    Rename,
    /// `/model [model]`.
    Model,
    /// `/effort [default|low|medium|high]`.
    Effort,
    /// `/mode [ask|auto-edit|auto]`.
    Mode,
    /// `/agents`.
    Agents,
    /// `/workflows`.
    Workflows,
    /// `/skills`.
    Skills,
    /// `/memory [show|search|dream] [query...]`.
    Memory,
    /// `/runs [run]`.
    Runs,
    /// `/remote [list|test] [host]`.
    Remote,
    /// `/cost [model|agent|node]`.
    Cost,
    /// `/compact [focus...]`.
    Compact,
    /// `/undo`.
    Undo,
    /// `/diff [path]`.
    Diff,
    /// `/context`.
    Context,
    /// `/config [show|check]`.
    Config,
    /// `/doctor`.
    Doctor,
    /// `/mcp`: MCP servers, their status and actions.
    Mcp,
    /// `/jobs`: background bash jobs; open their output or stop them.
    Jobs,
    /// `/login [provider]`, `/login mcp [server]`.
    Login,
    /// `/quit`.
    Quit,
}

impl Builtin {
    /// Every built-in, in help order.
    pub const ALL: [Builtin; 24] = [
        Builtin::Help,
        Builtin::New,
        Builtin::Resume,
        Builtin::Rename,
        Builtin::Model,
        Builtin::Effort,
        Builtin::Mode,
        Builtin::Agents,
        Builtin::Workflows,
        Builtin::Skills,
        Builtin::Memory,
        Builtin::Runs,
        Builtin::Remote,
        Builtin::Cost,
        Builtin::Compact,
        Builtin::Undo,
        Builtin::Diff,
        Builtin::Context,
        Builtin::Config,
        Builtin::Doctor,
        Builtin::Mcp,
        Builtin::Jobs,
        Builtin::Login,
        Builtin::Quit,
    ];

    /// The command name (without `/`).
    pub fn name(self) -> &'static str {
        match self {
            Self::Help => "help",
            Self::New => "new",
            Self::Resume => "resume",
            Self::Rename => "rename",
            Self::Model => "model",
            Self::Effort => "effort",
            Self::Mode => "mode",
            Self::Agents => "agents",
            Self::Workflows => "workflows",
            Self::Skills => "skills",
            Self::Memory => "memory",
            Self::Runs => "runs",
            Self::Remote => "remote",
            Self::Cost => "cost",
            Self::Compact => "compact",
            Self::Undo => "undo",
            Self::Diff => "diff",
            Self::Context => "context",
            Self::Config => "config",
            Self::Doctor => "doctor",
            Self::Mcp => "mcp",
            Self::Jobs => "jobs",
            Self::Login => "login",
            Self::Quit => "quit",
        }
    }

    /// The built-in called `name`.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|b| b.name() == name)
    }

    /// One-line description (palette and `/help`).
    pub fn description(self) -> &'static str {
        match self {
            Self::Help => "Show commands and keys, or the details of one command",
            Self::New => "Start a new session in this project",
            Self::Resume => "Resume a saved session: pick one, or name it",
            Self::Rename => "Rename this session",
            Self::Model => "Show or switch the model for this session",
            Self::Effort => "Show or set the reasoning effort for this session's agent",
            Self::Mode => "Show or switch the approval mode",
            Self::Agents => "List agents and switch to one",
            Self::Workflows => "List workflows and their arguments",
            Self::Skills => "List skills and their arguments",
            Self::Memory => "Browse, search or consolidate memory",
            Self::Runs => "List runs or open one in the agent map",
            Self::Remote => "List remote hosts or test a connection",
            Self::Cost => "Show spend and tokens by model, agent and node",
            Self::Compact => "Condense the conversation into a handoff now",
            Self::Undo => "Restore the files from before the last agent turn",
            Self::Diff => "Show the changes made in this session",
            Self::Context => "Show what the next request carries, by layer",
            Self::Config => "Show the effective configuration or check harness.ini",
            Self::Doctor => "Check the setup: config, providers, sandbox, remotes",
            Self::Mcp => "MCP servers: status, tools and auth; restart, disable or view logs",
            Self::Jobs => "Background bash jobs: elapsed time, output; stop one",
            Self::Login => {
                "Add, check and save an API key for a provider, or authorize an MCP server"
            }
            Self::Quit => "Quit harness",
        }
    }

    /// The argument spec.
    pub fn args(self) -> Vec<ArgSpec> {
        match self {
            Self::Help => vec![ArgSpec::optional("command").with_help("a command name")],
            Self::Resume => {
                vec![
                    ArgSpec::rest("session")
                        .with_help("a name, `last`, an id prefix or a #tag; none opens the picker"),
                ]
            }
            Self::Rename => vec![ArgSpec::rest("name").with_help("the new session name")],
            Self::Model => {
                vec![ArgSpec::optional("model").with_help("model alias or provider/model")]
            }
            Self::Effort => vec![
                ArgSpec::optional("effort")
                    .with_help("reasoning effort; default restores the agent's or model's own")
                    .with_choices(&["default", "low", "medium", "high"]),
            ],
            Self::Mode => vec![
                ArgSpec::optional("mode")
                    .with_help("approval mode")
                    .with_choices(&["ask", "auto-edit", "auto"]),
            ],
            Self::Memory => vec![
                ArgSpec::optional("action")
                    .with_help("what to do")
                    .with_choices(&["show", "search", "dream"])
                    .with_default("show"),
                ArgSpec::rest("query").with_help("search words"),
            ],
            Self::Runs => vec![ArgSpec::optional("run").with_help("run id or prefix")],
            Self::Remote => vec![
                ArgSpec::optional("action")
                    .with_help("what to do")
                    .with_choices(&["list", "test"])
                    .with_default("list"),
                ArgSpec::optional("host").with_help("a [remote.<name>] host"),
            ],
            Self::Cost => vec![
                ArgSpec::optional("by")
                    .with_help("show only one breakdown")
                    .with_choices(&["model", "agent", "node"]),
            ],
            Self::Compact => {
                vec![ArgSpec::rest("focus").with_help("what the handoff should focus on")]
            }
            Self::Diff => vec![ArgSpec::optional("path").with_help("limit to one file")],
            Self::Login => vec![
                ArgSpec::optional("provider")
                    .with_help("provider to log in to, or mcp to authorize a remote MCP server")
                    .with_choices(&["anthropic", "openai", "openrouter", "mcp"]),
                ArgSpec::optional("server").with_help("with mcp: a remote [mcp.<name>] server"),
            ],
            Self::Config => vec![
                ArgSpec::optional("action")
                    .with_help("what to do")
                    .with_choices(&["show", "check"])
                    .with_default("show"),
            ],
            Self::New
            | Self::Agents
            | Self::Workflows
            | Self::Skills
            | Self::Undo
            | Self::Context
            | Self::Doctor
            | Self::Mcp
            | Self::Jobs
            | Self::Quit => Vec::new(),
        }
    }
}
