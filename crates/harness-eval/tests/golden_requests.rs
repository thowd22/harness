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
use harness_providers::model::{ChatRequest, Message};
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
    req.thinking = harness_agent::subagents::thinking_for(agent.effort);
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
    // The binary sends one `session_id` per process by default
    // (`[provider.openrouter] session_id`); a fixed stand-in here.
    let or_options = OpenRouterOptions {
        session_id: Some("harness-SESSION".into()),
        ..OpenRouterOptions::default()
    };
    let openrouter = OpenRouterProvider::with_key("test-key", None, or_options)
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

/// The identity layer opens every built-in agent's request on every
/// provider, with bytes that do not depend on the session (cwd, date,
/// branch), so it stays inside the cached prefix across turns and sessions.
#[test]
fn identity_layer_opens_the_cached_prefix_of_every_request() {
    use harness_config::prompts::IDENTITY;
    let c = catalog();
    let other = PromptVars {
        cwd: "/elsewhere".into(),
        os: "macos".into(),
        date: "2027-01-01".into(),
        git_branch: "dev".into(),
    };
    for a in c.agents.active() {
        let PromptRef::Named(p) = &a.item.prompt else {
            panic!("built-in agents use named prompts");
        };
        let prompt = c.prompts.get(p).unwrap();
        for v in [vars(), other.clone()] {
            let text = SessionPrompt::new(prompt, v).unwrap();
            assert!(text.text().starts_with(IDENTITY), "{}", a.item.name);
        }
        let b = bodies(&a.item.name);
        let anthropic = b["anthropic"]["system"][0]["text"].as_str().unwrap();
        assert!(anthropic.starts_with(IDENTITY), "{}", a.item.name);
        let openai = b["openai"]["instructions"].as_str().unwrap();
        assert!(openai.starts_with(IDENTITY), "{}", a.item.name);
        // Anthropic models on OpenRouter get the system prompt as content
        // parts (for cache breakpoints).
        let openrouter = b["openrouter"]["messages"][0]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert!(openrouter.starts_with(IDENTITY), "{}", a.item.name);
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

/// Where the cache breakpoints of an Anthropic body are, with their TTL.
fn breakpoints(body: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    let mut scan = |at: String, b: &Value| {
        if let Some(cc) = b.get("cache_control") {
            out.push(json!([at, cc.get("ttl").cloned().unwrap_or(json!("5m"))]));
        }
    };
    for (i, t) in body["tools"].as_array().into_iter().flatten().enumerate() {
        scan(format!("tools[{i}]"), t);
    }
    for (i, b) in body["system"].as_array().into_iter().flatten().enumerate() {
        scan(format!("system[{i}]"), b);
    }
    for (m, msg) in body["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        for (i, b) in msg["content"].as_array().into_iter().flatten().enumerate() {
            scan(format!("messages[{m}][{i}]"), b);
        }
    }
    out
}

/// The opt-in request options (TASK-161.4) on the coder's first request:
/// `[provider.anthropic] thinking_display = omitted`, `cache_ttl = mixed`,
/// the `[experimental] cache_prewarm` body, and `[provider.openrouter]
/// reasoning_exclude`, with `effort: medium` on the agent so thinking is
/// requested. Only the parts the options change are kept; the full bodies
/// are in the default snapshots above.
#[test]
fn coder_request_options() {
    use harness_providers::anthropic::{CacheTtl, ThinkingDisplay};
    let c = catalog();
    let mut agent = c.agents.get("coder").unwrap().clone();
    agent.effort = Some(Effort::Medium);
    let (req, _) = request(&c, &agent, "claude-sonnet-5-5");
    let mut conf = AnthropicConfig::new();
    conf.request.thinking_display = ThinkingDisplay::Omitted;
    conf.request.cache_ttl = CacheTtl::Mixed;
    let p = AnthropicProvider::new("test-key", conf).unwrap();
    let body = p.build_body(&req).unwrap();
    let mut session_start = req.clone();
    session_start.messages.clear();
    let warm = p.prewarm_body(&session_start).unwrap().unwrap();
    let (or_req, _) = request(&c, &agent, "anthropic/claude-sonnet-5-5");
    let or = OpenRouterProvider::with_key(
        "test-key",
        None,
        OpenRouterOptions {
            reasoning_exclude: true,
            service_tier: Some(harness_providers::openrouter::ServiceTier::Fast),
            ..OpenRouterOptions::default()
        },
    )
    .unwrap()
    .request_body(&or_req);
    insta::assert_json_snapshot!(
        "request_options_coder",
        json!({
            "anthropic": {
                "thinking": body["thinking"],
                "output_config": body["output_config"],
                "breakpoints": breakpoints(&body),
            },
            "anthropic_prewarm": {
                "stream": warm["stream"],
                "max_tokens": warm["max_tokens"],
                "thinking": warm["thinking"],
                "output_config": warm["output_config"],
                "messages": warm["messages"],
                "breakpoints": breakpoints(&warm),
                "same_tools_and_system": warm["tools"] == body["tools"] && warm["system"] == body["system"],
            },
            "openrouter": {"reasoning": or["reasoning"], "service_tier": or["service_tier"]},
        })
    );
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
