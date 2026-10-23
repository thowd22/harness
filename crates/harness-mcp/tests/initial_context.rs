//! The initial-context CI budget (TASK-89) with a large MCP server
//! configured (TASK-146.2 AC#4).
//!
//! A server with 150 tools is configured in harness.ini. The built-in
//! `coder` agent selects none of them, so its initial context is unchanged;
//! an agent that selects all of them (`mcp:big/*`) gets the search-and-call
//! tool instead of 150 schemas and stays within the same budget. Both
//! measurements are printed to the test output like the agent crate's test.

mod common;

use std::io::Write as _;
use std::sync::Arc;

use common::*;
use harness_agent::context::{DEFAULT_INITIAL_BUDGET, InitialContext, SessionPrefix};
use harness_agent::dispatch::{StandardTools, ToolSet};
use harness_agent::subagents::ExtraTools;
use harness_config::agents::{Agent, MemoryAccess, PromptRef};
use harness_config::builtins;
use harness_config::discovery::{Artifact, Catalog, Kind, Scope, SourceFile};
use harness_config::ini::config::Config;
use harness_config::prompts::{PromptVars, SessionPrompt};
use harness_mcp::{McpToolbox, Servers, ToolboxOptions};
use harness_memory::drive::Drive;
use harness_memory::index::SessionIndex;
use harness_policy::mcp::ReadOnlyTools;
use harness_providers::model::ToolSpec;
use harness_tools::bash::EnvScrubber;
use harness_tools::config::ToolsConfig;
use harness_tools::local::{LocalExec, LocalWorkspace};
use harness_tools::spill::MemoryBlobSink;

fn report(line: &str) {
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "{line}");
}

fn builtin_tools(dir: &std::path::Path, agent: &Agent) -> Vec<ToolSpec> {
    let standard = ToolSet::standard(StandardTools {
        workspace: Arc::new(LocalWorkspace::new(dir)),
        exec: Arc::new(LocalExec::new()),
        cwd: dir.to_path_buf(),
        config: ToolsConfig::default(),
        scrubber: EnvScrubber::new(Vec::<String>::new(), Vec::<String>::new()),
        spill: Arc::new(MemoryBlobSink::default()),
    });
    let mut specs = Vec::new();
    for name in agent.request_tools() {
        if let Some(t) = standard.get(name) {
            specs.push(t.spec());
        } else if name == harness_agent::todo::NAME {
            specs.push(ToolSpec {
                name: harness_agent::todo::NAME.into(),
                description: harness_agent::todo::DESCRIPTION.into(),
                input_schema: harness_agent::todo::schema(),
            });
        } else if name == harness_memory::tool::NAME {
            specs.extend(harness_memory::tool::spec(MemoryAccess::ReadWrite));
        } else {
            panic!("unknown tool `{name}`");
        }
    }
    specs
}

#[tokio::test]
async fn initial_context_stays_within_budget_with_a_large_mcp_server() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::defaults();
    cfg.mcp.insert(
        "big".into(),
        stdio(&["--bulk-tools", "150", "--agent-tools"]),
    );
    let servers = Arc::new(Servers::from_config(&cfg, fast_opts()));
    let toolbox = McpToolbox::new(
        servers.clone(),
        ToolboxOptions::from_config(&cfg, None),
        Arc::new(ReadOnlyTools::default()),
    );

    let catalog = Catalog::load(&builtins::roots());
    let coder = catalog
        .agents
        .get(builtins::DEFAULT_AGENT)
        .expect("built-in default agent")
        .clone();
    let PromptRef::Named(prompt_name) = coder.prompt.clone() else {
        panic!("default agent uses a named prompt");
    };
    let prompt = catalog.prompts.get(&prompt_name).expect("default prompt");
    let vars = PromptVars {
        cwd: "/home/user/projects/example".into(),
        os: "linux".into(),
        date: "2026-10-22".into(),
        git_branch: "main".into(),
    };
    let system = SessionPrompt::new(prompt, vars).expect("prompt renders");
    let drive = Drive::open_or_init(dir.path().join("memory")).unwrap();
    let index = SessionIndex::load(
        drive.root(),
        Some("example-0123abcd"),
        cfg.memory.index_max_tokens,
    )
    .unwrap();
    let skills = harness_config::skills::listing(catalog.skills.active().iter().map(|e| &e.item));
    let prefix = SessionPrefix::freeze(system.text(), Some(index.text()), Some(skills.as_str()));

    // The same agent selecting every tool of the large server.
    let mut tools_line: Vec<String> = coder.tools.clone();
    tools_line.push("mcp:big/*".into());
    let yaml = format!(
        "name: coder-mcp\nprompt: {prompt_name}\nmemory: read-write\ntools: [{}]\n",
        tools_line.join(", ")
    );
    let with_mcp = Agent::load(&SourceFile::inline(
        Scope::Global,
        Kind::Agent,
        "agents/coder-mcp.yaml",
        &yaml,
    ))
    .unwrap();

    for agent in [&coder, &with_mcp] {
        ExtraTools::prepare(&toolbox, agent, &[]).await;
        let mcp: Vec<ToolSpec> = ExtraTools::tools(&toolbox, agent, &[])
            .iter()
            .map(|t| t.spec())
            .collect();
        if agent.mcp_tools.is_empty() {
            assert!(mcp.is_empty(), "nothing from MCP unless selected");
        } else {
            let names: Vec<&str> = mcp.iter().map(|t| t.name.as_str()).collect();
            assert_eq!(names, ["mcp_tools"], "150 schemas are over the budget");
        }
        let mut tools = builtin_tools(dir.path(), agent);
        tools.extend(mcp);
        let measured = InitialContext::measure(&prefix, &tools);
        report(&format!(
            "harness CI: agent `{}` with a 150-tool MCP server configured: {}",
            agent.name,
            measured.report(DEFAULT_INITIAL_BUDGET)
        ));
        if let Err(e) = measured.check(DEFAULT_INITIAL_BUDGET) {
            panic!("{e}");
        }
    }
    // The coder agent selected nothing, so it never started the server.
    assert!(toolbox.take_warnings().is_empty());
    servers.shutdown_all().await;
}
