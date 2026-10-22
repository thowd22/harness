//! Golden snapshots (TASK-111, TASK-49): the rendered default prompts and
//! the exact request body each provider would send for every built-in
//! agent's first turn.
//!
//! Any change to prompt assembly, tool schemas or defaults shows up as a
//! snapshot diff. A changed snapshot fails the test until it is accepted
//! explicitly (`cargo insta review`, or `INSTA_UPDATE=always cargo test`),
//! so the diff of the `.snap` file is part of the change under review. CI
//! never writes snapshots. See docs/golden-snapshots.md.

use std::sync::Arc;

use harness_agent::dispatch::{StandardTools, ToolSet};
use harness_config::agents::{Agent, Effort, PromptRef};
use harness_config::builtins;
use harness_config::discovery::Catalog;
use harness_config::prompts::{PromptVars, SessionPrompt};
use harness_providers::anthropic::{AnthropicConfig, AnthropicProvider};
use harness_providers::model::{ChatRequest, Message, ReasoningEffort, ThinkingConfig};
use harness_providers::openai::OpenAiProvider;
use harness_providers::openrouter::{OpenRouterOptions, OpenRouterProvider};
use harness_tools::bash::EnvScrubber;
use harness_tools::config::ToolsConfig;
use harness_tools::local::{LocalExec, LocalWorkspace};
use harness_tools::spill::MemoryBlobSink;
use serde_json::{Value, json};

const USER: &str = "The test in tests/parser.rs fails. Fix it.";

fn vars() -> PromptVars {
    PromptVars {
        cwd: "/work/app".into(),
        os: "linux".into(),
        date: "2026-10-22".into(),
        git_branch: "main".into(),
    }
}

fn catalog() -> Catalog {
    let c = Catalog::load(&builtins::roots());
    assert!(c.errors().is_empty(), "{:?}", c.errors());
    c
}

fn system_prompt(c: &Catalog, agent: &Agent) -> String {
    let body = match &agent.prompt {
        PromptRef::Named(p) => SessionPrompt::new(c.prompts.get(p).unwrap(), vars())
            .unwrap()
            .text()
            .to_owned(),
        PromptRef::Inline(t) => t.clone(),
    };
    // The harness appends the provenance section to every system prompt.
    format!(
        "{body}\n\n{}",
        harness_policy::provenance::SYSTEM_PROMPT_SECTION
    )
}

/// The first request of `agent` (tools as the agent loop would offer them;
/// tools the build does not provide yet are listed in the snapshot).
fn request(c: &Catalog, agent: &Agent, model: &str) -> (ChatRequest, Vec<String>) {
    let dir = tempfile::tempdir().unwrap();
    let all = ToolSet::standard(StandardTools {
        workspace: Arc::new(LocalWorkspace::new(dir.path())),
        exec: Arc::new(LocalExec::new()),
        cwd: dir.path().to_owned(),
        config: ToolsConfig::default(),
        scrubber: EnvScrubber::new(Vec::<String>::new(), Vec::<String>::new()),
        spill: Arc::new(MemoryBlobSink::default()),
    });
    let wanted = agent.request_tools();
    let (tools, mut missing) = all.select(&wanted);
    let mut specs = tools.specs();
    if let Some(spec) = harness_memory::tool::spec(agent.memory) {
        specs.push(spec);
        missing.retain(|m| m != harness_memory::tool::NAME);
    }
    let mut req = ChatRequest::new(model);
    req.system = Some(system_prompt(c, agent));
    req.messages = vec![Message::user(USER)];
    req.tools = specs;
    req.thinking = agent.effort.map(|e| ThinkingConfig {
        budget_tokens: None,
        effort: Some(match e {
            Effort::Low => ReasoningEffort::Low,
            Effort::Medium => ReasoningEffort::Medium,
            Effort::High => ReasoningEffort::High,
        }),
    });
    (req, missing)
}

fn bodies(name: &str) -> Value {
    let c = catalog();
    let agent = c.agents.get(name).unwrap().clone();
    let (anthropic_req, missing) = request(&c, &agent, "claude-sonnet-5-5");
    let anthropic = AnthropicProvider::new("test-key", AnthropicConfig::new())
        .unwrap()
        .build_body(&anthropic_req)
        .unwrap();
    let (openai_req, _) = request(&c, &agent, "gpt-5.1");
    let openai = OpenAiProvider::with_key("test-key", None)
        .unwrap()
        .request_body(&openai_req)
        .unwrap();
    let (or_req, _) = request(&c, &agent, "anthropic/claude-sonnet-5-5");
    let openrouter = OpenRouterProvider::with_key("test-key", None, OpenRouterOptions::default())
        .unwrap()
        .request_body(&or_req);
    json!({
        "agent": name,
        "tools_not_built_yet": missing,
        "anthropic": anthropic,
        "openai": openai,
        "openrouter": openrouter,
    })
}

#[test]
fn default_prompts_render() {
    let c = catalog();
    for p in c.prompts.active() {
        let text = SessionPrompt::new(&p.item, vars())
            .unwrap()
            .text()
            .to_owned();
        insta::assert_snapshot!(format!("prompt_{}", p.item.name), text);
    }
}

#[test]
fn coder_requests() {
    insta::assert_json_snapshot!("request_coder", bodies("coder"));
}

#[test]
fn researcher_requests() {
    insta::assert_json_snapshot!("request_researcher", bodies("researcher"));
}

#[test]
fn reviewer_requests() {
    insta::assert_json_snapshot!("request_reviewer", bodies("reviewer"));
}

#[test]
fn every_builtin_agent_has_a_request_snapshot() {
    // Adding a built-in agent must add a snapshot test above.
    assert_eq!(builtins::AGENTS, ["coder", "researcher", "reviewer"]);
}

#[test]
fn no_unreviewed_snapshots_are_left_behind() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots");
    let pending: Vec<String> = std::fs::read_dir(&dir)
        .map(|d| {
            d.filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with(".snap.new") || n.ends_with(".pending-snap"))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        pending.is_empty(),
        "unaccepted snapshot changes: {pending:?}; review them with `cargo insta review`"
    );
}
