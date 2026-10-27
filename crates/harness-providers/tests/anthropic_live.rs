//! Live checks against the real Anthropic API for behaviour the offline
//! tests only cover with a mock server: gzip request bodies, thinking
//! signatures across a tool loop, prompt-cache counters for every
//! `cache_ttl`, `thinking_display = omitted`, cache pre-warm and
//! `count_tokens`.
//!
//! Every test is ignored and also needs `HARNESS_LIVE_ANTHROPIC=1` and
//! `ANTHROPIC_API_KEY`:
//!
//! ```sh
//! HARNESS_LIVE_ANTHROPIC=1 ANTHROPIC_API_KEY=... cargo test -p harness-providers \
//!     --test anthropic_live -- --ignored --test-threads 1 --nocapture
//! ```
//!
//! They use Claude Haiku 5.5 and print each call's usage; a full run costs
//! well under a cent.

use harness_providers::anthropic::{
    AnthropicConfig, AnthropicProvider, CacheTtl, GzipRequests, ThinkingDisplay,
};
use harness_providers::http::GzipLatch;
use harness_providers::model::{
    ChatRequest, ContentBlock, Message, ReasoningEffort, Role, StopKind, ThinkingConfig, ToolSpec,
    Usage,
};
use harness_providers::openai::notice::request_body;
use harness_providers::provider::{Provider, Response};
use serde_json::json;
use tokio_util::sync::CancellationToken;

const MODEL: &str = "claude-haiku-5-5";

/// The key, when live tests are enabled.
fn key() -> Option<String> {
    if std::env::var("HARNESS_LIVE_ANTHROPIC").as_deref() != Ok("1") {
        eprintln!("skipping: set HARNESS_LIVE_ANTHROPIC=1 to run live Anthropic tests");
        return None;
    }
    Some(std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY"))
}

fn provider(key: &str, edit: impl FnOnce(&mut AnthropicConfig)) -> AnthropicProvider {
    let mut cfg = AnthropicConfig::new();
    // A private latch, so a rejected encoding shows up here instead of
    // being hidden by another test's fallback.
    cfg.gzip_latch = GzipLatch::new();
    edit(&mut cfg);
    AnthropicProvider::new(key, cfg).unwrap()
}

/// A system prompt of several thousand tokens, unique to `tag` and this
/// process, so the first call writes the cache and nothing earlier can
/// have warmed it.
fn long_system(tag: &str) -> String {
    let nonce = format!("{tag}-{}-{}", std::process::id(), unix_nanos());
    let mut s = format!("You are a terse assistant. Session {nonce}.\n\nReference notes:\n");
    for i in 0..400 {
        s.push_str(&format!(
            "Note {i}: module m{i} owns the parser stage {i} and reports errors with code E{i:04}.\n"
        ));
    }
    s
}

fn unix_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

async fn call(p: &AnthropicProvider, req: ChatRequest, what: &str) -> Response {
    let r = p
        .complete(req, CancellationToken::new())
        .await
        .unwrap_or_else(|e| panic!("{what}: {e}"));
    print_usage(what, &r.usage);
    r
}

fn print_usage(what: &str, u: &Usage) {
    eprintln!(
        "{what}: input {} output {} cache_read {} cache_write {}",
        u.input_tokens, u.output_tokens, u.cache_read_tokens, u.cache_write_tokens
    );
}

fn read_tool() -> ToolSpec {
    ToolSpec {
        name: "read".into(),
        description: "Read a file and return its contents.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"]
        }),
    }
}

fn thinking(effort: ReasoningEffort) -> Option<ThinkingConfig> {
    Some(ThinkingConfig {
        budget_tokens: None,
        effort: Some(effort),
    })
}

#[tokio::test]
#[ignore = "live: set HARNESS_LIVE_ANTHROPIC=1 and ANTHROPIC_API_KEY"]
async fn large_bodies_are_sent_gzipped_and_accepted() {
    let Some(key) = key() else { return };
    let latch = GzipLatch::new();
    // `Always`, not the default `Auto`: cargo exports SSL_CERT_FILE and
    // SSL_CERT_DIR to the processes it runs, and `Auto` treats those as a
    // custom CA and stays raw. The real API is the same either way.
    let p = provider(&key, |c| {
        c.gzip = GzipRequests::Always;
        c.gzip_latch = latch.clone();
    });
    let mut req = ChatRequest::new(MODEL);
    req.system = Some(long_system("gzip"));
    req.max_tokens = Some(16);
    req.messages.push(Message::user("Reply with: ok"));
    let r = call(&p, req, "gzip").await;
    let stats = request_body(&r.message).expect("request body notice");
    eprintln!(
        "gzip: raw {} wire {} encoding {:?}",
        stats.raw_bytes, stats.wire_bytes, stats.encoding
    );
    assert!(!latch.is_tripped(), "the API rejected the gzip body");
    assert_eq!(stats.encoding.as_deref(), Some("gzip"), "{stats:?}");
    assert!(stats.wire_bytes * 2 < stats.raw_bytes, "{stats:?}");
}

#[tokio::test]
#[ignore = "live: set HARNESS_LIVE_ANTHROPIC=1 and ANTHROPIC_API_KEY"]
async fn thinking_signatures_survive_a_tool_loop() {
    let Some(key) = key() else { return };
    for display in [ThinkingDisplay::Summarized, ThinkingDisplay::Omitted] {
        let p = provider(&key, |c| c.request.thinking_display = display);
        let mut req = ChatRequest::new(MODEL);
        req.system =
            Some("You are a coding agent. Use the read tool; never guess file contents.".into());
        req.tools = vec![read_tool()];
        req.thinking = thinking(ReasoningEffort::High);
        req.max_tokens = Some(4000);
        req.messages.push(Message::user(
            "Read src/a.rs and src/b.rs, then tell me which one defines more functions. \
             Think it through before calling the tool.",
        ));
        let first = call(&p, req.clone(), &format!("{display:?} turn 1")).await;
        assert_eq!(first.stop.kind, StopKind::ToolUse, "{:?}", first.message);
        let reasoning: Vec<(&String, &Option<String>)> = first
            .message
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Reasoning { text, signature } => Some((text, signature)),
                _ => None,
            })
            .collect();
        assert!(
            !reasoning.is_empty(),
            "no thinking block: {:?}",
            first.message
        );
        assert!(
            reasoning
                .iter()
                .all(|(_, s)| s.as_deref().is_some_and(|s| !s.is_empty())),
            "unsigned thinking: {reasoning:?}"
        );
        let shown: usize = reasoning.iter().map(|(t, _)| t.len()).sum();
        match display {
            ThinkingDisplay::Omitted => assert_eq!(shown, 0, "{reasoning:?}"),
            _ => assert!(shown > 0, "summarized thinking came back empty"),
        }

        let results: Vec<ContentBlock> = first
            .message
            .tool_uses()
            .map(|(id, _, input)| {
                let body = if input["path"].as_str().unwrap_or("").ends_with("a.rs") {
                    "fn one() {}\nfn two() {}\nfn three() {}\n"
                } else {
                    "fn only() {}\n"
                };
                ContentBlock::tool_result(id, body, false)
            })
            .collect();
        req.messages.push(first.message.clone());
        req.messages.push(Message {
            role: Role::User,
            content: results,
        });
        // The API checks the returned signatures; an edited or dropped
        // thinking block fails this call.
        let second = call(&p, req.clone(), &format!("{display:?} turn 2")).await;
        if second.stop.kind == StopKind::ToolUse {
            // It asked for the other file only now; answer once more.
            let results: Vec<ContentBlock> = second
                .message
                .tool_uses()
                .map(|(id, _, _)| ContentBlock::tool_result(id, "fn only() {}\n", false))
                .collect();
            req.messages.push(second.message.clone());
            req.messages.push(Message {
                role: Role::User,
                content: results,
            });
            let third = call(&p, req, &format!("{display:?} turn 3")).await;
            assert!(third.message.text().contains("a.rs"), "{:?}", third.message);
        } else {
            assert!(
                second.message.text().contains("a.rs"),
                "{:?}",
                second.message
            );
        }
    }
}

#[tokio::test]
#[ignore = "live: set HARNESS_LIVE_ANTHROPIC=1 and ANTHROPIC_API_KEY"]
async fn every_cache_ttl_writes_then_reads() {
    let Some(key) = key() else { return };
    for ttl in [CacheTtl::FiveMinutes, CacheTtl::OneHour, CacheTtl::Mixed] {
        let p = provider(&key, |c| c.request.cache_ttl = ttl);
        let mut req = ChatRequest::new(MODEL);
        req.system = Some(long_system(&format!("{ttl:?}")));
        req.tools = vec![read_tool()];
        req.max_tokens = Some(16);
        req.messages.push(Message::user("Reply with: ok"));
        let first = call(&p, req.clone(), &format!("{ttl:?} write")).await;
        assert!(first.usage.cache_write_tokens > 1000, "{:?}", first.usage);
        assert_eq!(first.usage.cache_read_tokens, 0, "{:?}", first.usage);
        let second = call(&p, req, &format!("{ttl:?} read")).await;
        assert!(
            second.usage.cache_read_tokens >= first.usage.cache_write_tokens,
            "{:?}",
            second.usage
        );
    }
}

#[tokio::test]
#[ignore = "live: set HARNESS_LIVE_ANTHROPIC=1 and ANTHROPIC_API_KEY"]
async fn prewarm_fills_the_cache_for_the_next_call() {
    let Some(key) = key() else { return };
    let p = provider(&key, |_| {});
    let mut req = ChatRequest::new(MODEL);
    req.system = Some(long_system("prewarm"));
    req.tools = vec![read_tool()];
    req.max_tokens = Some(16);
    req.messages.push(Message::user("Reply with: ok"));
    let warm = p
        .prewarm_cache(req.clone(), CancellationToken::new())
        .await
        .expect("prewarm");
    print_usage("prewarm", &warm);
    assert!(warm.cache_write_tokens > 1000, "{warm:?}");
    let r = call(&p, req, "after prewarm").await;
    assert!(
        r.usage.cache_read_tokens >= warm.cache_write_tokens,
        "{:?}",
        r.usage
    );
}

#[tokio::test]
#[ignore = "live: set HARNESS_LIVE_ANTHROPIC=1 and ANTHROPIC_API_KEY"]
async fn count_tokens_matches_the_billed_input() {
    let Some(key) = key() else { return };
    let p = provider(&key, |c| {
        c.request.cache = harness_providers::anthropic::CacheMode::Off
    });
    let mut req = ChatRequest::new(MODEL);
    req.system = Some(long_system("count"));
    req.tools = vec![read_tool()];
    req.max_tokens = Some(16);
    req.messages.push(Message::user("Reply with: ok"));
    let counted = p
        .count_tokens(&req, &CancellationToken::new())
        .await
        .expect("count_tokens");
    let r = call(&p, req, "count").await;
    let billed = r.usage.total_input();
    eprintln!("count_tokens {counted}, billed input {billed}");
    // The count is an estimate; it should be within a few percent.
    let diff = counted.abs_diff(billed);
    assert!(diff * 20 <= billed, "counted {counted}, billed {billed}");
}
