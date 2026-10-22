//! CI budget for the default agent's initial context (TASK-89).
//!
//! Measures what every request of a fresh session with the built-in
//! `coder` agent carries before any history: the rendered system prompt,
//! the tool definitions, the memory index of a fresh drive and the skill
//! listing. The test fails above the threshold, and always prints the
//! threshold and the current value to the test output (stderr, which
//! `cargo test` shows in CI logs).

use std::io::Write as _;
use std::sync::Arc;

use harness_agent::context::{DEFAULT_INITIAL_BUDGET, InitialContext, SessionPrefix};
use harness_agent::dispatch::{StandardTools, ToolSet};
use harness_config::agents::{MemoryAccess, PromptRef};
use harness_config::builtins;
use harness_config::discovery::Catalog;
use harness_config::ini::config::Config;
use harness_config::prompts::{PromptVars, SessionPrompt};
use harness_memory::drive::Drive;
use harness_memory::index::SessionIndex;
use harness_providers::model::ToolSpec;
use harness_tools::bash::EnvScrubber;
use harness_tools::config::ToolsConfig;
use harness_tools::local::{LocalExec, LocalWorkspace};
use harness_tools::spill::MemoryBlobSink;

/// Writes past the test harness's output capture, so the report shows in
/// CI output even when the test passes.
fn report(line: &str) {
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "{line}");
}

fn default_tools(dir: &std::path::Path, names: &[&str]) -> Vec<ToolSpec> {
    let standard = ToolSet::standard(StandardTools {
        workspace: Arc::new(LocalWorkspace::new(dir)),
        exec: Arc::new(LocalExec::new()),
        cwd: dir.to_path_buf(),
        config: ToolsConfig::default(),
        scrubber: EnvScrubber::new(Vec::<String>::new(), Vec::<String>::new()),
        spill: Arc::new(MemoryBlobSink::default()),
    });
    let mut specs = Vec::new();
    for name in names {
        if let Some(t) = standard.get(name) {
            specs.push(t.spec());
        } else if *name == harness_agent::todo::NAME {
            specs.push(ToolSpec {
                name: harness_agent::todo::NAME.into(),
                description: harness_agent::todo::DESCRIPTION.into(),
                input_schema: harness_agent::todo::schema(),
            });
        } else if *name == harness_memory::tool::NAME {
            specs.extend(harness_memory::tool::spec(MemoryAccess::ReadWrite));
        } else {
            panic!("the default agent uses an unknown tool `{name}`");
        }
    }
    specs
}

#[test]
fn default_agent_initial_context_is_within_budget() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = Catalog::load(&builtins::roots());
    let agent = catalog
        .agents
        .get(builtins::DEFAULT_AGENT)
        .expect("built-in default agent");
    let PromptRef::Named(prompt_name) = &agent.prompt else {
        panic!("default agent uses a named prompt");
    };
    let prompt = catalog.prompts.get(prompt_name).expect("default prompt");
    let vars = PromptVars {
        cwd: "/home/user/projects/example".into(),
        os: "linux".into(),
        date: "2026-10-22".into(),
        git_branch: "main".into(),
    };
    let system = SessionPrompt::new(prompt, vars).expect("prompt renders");

    let drive = Drive::open_or_init(dir.path().join("memory")).unwrap();
    let cfg = Config::defaults();
    let index = SessionIndex::load(
        drive.root(),
        Some("example-0123abcd"),
        cfg.memory.index_max_tokens,
    )
    .unwrap();
    let skills = harness_config::skills::listing(catalog.skills.active().iter().map(|e| &e.item));

    let names = agent.request_tools();
    let tools = default_tools(dir.path(), &names);
    let prefix = SessionPrefix::freeze(system.text(), Some(index.text()), Some(skills.as_str()));
    let measured = InitialContext::measure(&prefix, &tools);
    report(&format!(
        "harness CI: default agent `{}` with tools [{}]: {}",
        agent.name,
        names.join(", "),
        measured.report(DEFAULT_INITIAL_BUDGET)
    ));
    if let Err(e) = measured.check(DEFAULT_INITIAL_BUDGET) {
        panic!("{e}");
    }
}
