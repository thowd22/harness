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

/// Runs a read-tool loop to the end and checks the answer names `a.rs`.
/// Every turn sends the previous assistant messages back unchanged, so the
/// API checks each thinking signature. Returns the first response.
async fn tool_loop(p: &AnthropicProvider, mut req: ChatRequest, what: &str) -> Response {
    req.system =
        Some("You are a coding agent. Use the read tool; never guess file contents.".into());
    req.tools = vec![read_tool()];
    req.messages.push(Message::user(
        "Read src/a.rs and src/b.rs, then tell me which one defines more functions.",
    ));
    let mut first = None;
    for turn in 1..=4 {
        let r = call(p, req.clone(), &format!("{what} turn {turn}")).await;
        if r.stop.kind != StopKind::ToolUse {
            assert!(r.message.text().contains("a.rs"), "{:?}", r.message);
            return first.unwrap_or(r);
        }
        let results: Vec<ContentBlock> = r
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
        req.messages.push(r.message.clone());
        req.messages.push(Message {
            role: Role::User,
            content: results,
        });
        first.get_or_insert(r);
    }
    panic!("{what}: no answer after 4 turns");
}

fn signed_reasoning(r: &Response) -> usize {
    r.message
        .content
        .iter()
        .filter(
            |b| matches!(b, ContentBlock::Reasoning { signature: Some(s), .. } if !s.is_empty()),
        )
        .count()
}

#[tokio::test]
#[ignore = "live: set HARNESS_LIVE_ANTHROPIC=1 and ANTHROPIC_API_KEY"]
async fn thinking_display_updates_is_accepted_through_a_tool_loop() {
    let Some(key) = key() else { return };
    // `updates` adds the thinking-display-updates beta header; a header the
    // API does not know fails the first call.
    let p = provider(&key, |c| {
        c.request.thinking_display = ThinkingDisplay::Updates
    });
    let mut req = ChatRequest::new(MODEL);
    req.thinking = thinking(ReasoningEffort::High);
    req.max_tokens = Some(4000);
    tool_loop(&p, req, "updates").await;
}

#[tokio::test]
#[ignore = "live: set HARNESS_LIVE_ANTHROPIC=1 and ANTHROPIC_API_KEY"]
async fn budget_thinking_on_haiku_4_5_round_trips() {
    let Some(key) = key() else { return };
    let p = provider(&key, |_| {});
    let mut req = ChatRequest::new("claude-haiku-4-5");
    // An effort on a budget-style model becomes `budget_tokens` (low:
    // 2048), and `max_tokens` is raised above it.
    req.thinking = thinking(ReasoningEffort::Low);
    req.max_tokens = Some(1000);
    assert_eq!(p.build_body(&req).unwrap()["thinking"]["type"], "enabled");
    let first = tool_loop(&p, req, "haiku-4-5 budget").await;
    assert!(signed_reasoning(&first) > 0, "{:?}", first.message);
}

#[tokio::test]
#[ignore = "live: set HARNESS_LIVE_ANTHROPIC=1 and ANTHROPIC_API_KEY"]
async fn parallel_tool_calls_arrive_in_one_message_and_return_together() {
    let Some(key) = key() else { return };
    let p = provider(&key, |_| {});
    let mut req = ChatRequest::new(MODEL);
    req.system = Some(
        "You are a coding agent. When several files are needed, call the read tool for all of \
         them at once in a single response."
            .into(),
    );
    req.tools = vec![read_tool()];
    req.max_tokens = Some(1000);
    req.messages.push(Message::user(
        "Read x.txt, y.txt and z.txt in parallel, then reply with their contents joined by commas.",
    ));
    let first = call(&p, req.clone(), "parallel turn 1").await;
    let calls: Vec<(String, String)> = first
        .message
        .tool_uses()
        .map(|(id, _, input)| {
            (
                id.to_owned(),
                input["path"].as_str().unwrap_or("").to_owned(),
            )
        })
        .collect();
    assert!(
        calls.len() >= 2,
        "expected parallel calls: {:?}",
        first.message
    );
    req.messages.push(first.message.clone());
    req.messages.push(Message {
        role: Role::User,
        content: calls
            .iter()
            .map(|(id, path)| {
                ContentBlock::tool_result(id, path.trim_end_matches(".txt").to_uppercase(), false)
            })
            .collect(),
    });
    let second = call(&p, req, "parallel turn 2").await;
    let text = second.message.text();
    assert!(text.contains('X') && text.contains('Y'), "{text}");
}

#[tokio::test]
#[ignore = "live: set HARNESS_LIVE_ANTHROPIC=1 and ANTHROPIC_API_KEY"]
async fn the_api_records_each_cache_ttl_as_requested() {
    let Some(key) = key() else { return };
    // The streamed usage has no per-lifetime split, so this sends the body
    // the provider builds without streaming and reads `cache_creation`.
    let http = reqwest::Client::new();
    for ttl in [CacheTtl::FiveMinutes, CacheTtl::OneHour, CacheTtl::Mixed] {
        let p = provider(&key, |c| c.request.cache_ttl = ttl);
        let mut req = ChatRequest::new(MODEL);
        req.system = Some(long_system(&format!("raw-{ttl:?}")));
        req.tools = vec![read_tool()];
        req.max_tokens = Some(16);
        req.messages.push(Message::user(long_system("tail")));
        let mut body = p.build_body(&req).unwrap();
        body["stream"] = json!(false);
        let v: serde_json::Value = http
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &key)
            .header(
                "anthropic-version",
                harness_providers::anthropic::API_VERSION,
            )
            .json(&body)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let cc = &v["usage"]["cache_creation"];
        eprintln!("{ttl:?}: cache_creation {cc}");
        let five = cc["ephemeral_5m_input_tokens"].as_u64().unwrap_or(0);
        let hour = cc["ephemeral_1h_input_tokens"].as_u64().unwrap_or(0);
        match ttl {
            CacheTtl::FiveMinutes => assert!(five > 1000 && hour == 0, "{v}"),
            CacheTtl::OneHour => assert!(hour > 1000 && five == 0, "{v}"),
            // 1h for tools and system, 5m for the message tail.
            CacheTtl::Mixed => assert!(hour > 1000 && five > 1000, "{v}"),
        }
    }
}

#[tokio::test]
#[ignore = "live: set HARNESS_LIVE_ANTHROPIC=1 and ANTHROPIC_API_KEY"]
async fn count_tokens_accepts_a_gzip_body() {
    let Some(key) = key() else { return };
    let latch = GzipLatch::new();
    let p = provider(&key, |c| {
        c.gzip = GzipRequests::Always;
        c.gzip_latch = latch.clone();
    });
    let mut req = ChatRequest::new(MODEL);
    req.system = Some(long_system("count-gzip"));
    req.messages.push(Message::user("Reply with: ok"));
    let n = p
        .count_tokens(&req, &CancellationToken::new())
        .await
        .expect("count_tokens");
    eprintln!("count_tokens (gzip): {n}");
    assert!(n > 10_000, "{n}");
    assert!(!latch.is_tripped(), "count_tokens rejected the gzip body");
}

#[tokio::test]
#[ignore = "live: set HARNESS_LIVE_ANTHROPIC=1 and ANTHROPIC_API_KEY"]
async fn every_effort_level_and_current_model_is_accepted() {
    let Some(key) = key() else { return };
    let p = provider(&key, |_| {});
    let cases = [
        (MODEL, ReasoningEffort::Low),
        (MODEL, ReasoningEffort::Medium),
        (MODEL, ReasoningEffort::High),
        ("claude-sonnet-5-5", ReasoningEffort::Low),
        ("claude-opus-5-5", ReasoningEffort::Low),
    ];
    for (model, effort) in cases {
        let mut req = ChatRequest::new(model);
        req.thinking = thinking(effort);
        req.max_tokens = Some(2000);
        req.messages.push(Message::user(
            "What is 17 * 23? Reply with the number only.",
        ));
        let r = call(&p, req, &format!("{model} {effort:?}")).await;
        assert!(r.message.text().contains("391"), "{:?}", r.message);
        assert_eq!(r.stop.kind, StopKind::EndTurn);
    }
}

#[tokio::test]
#[ignore = "live: set HARNESS_LIVE_ANTHROPIC=1 and ANTHROPIC_API_KEY"]
async fn a_short_max_tokens_stops_with_max_tokens() {
    let Some(key) = key() else { return };
    let p = provider(&key, |_| {});
    let mut req = ChatRequest::new(MODEL);
    req.max_tokens = Some(5);
    req.messages
        .push(Message::user("Count from 1 to 100, separated by spaces."));
    let r = call(&p, req, "max_tokens").await;
    assert_eq!(r.stop.kind, StopKind::MaxTokens, "{:?}", r.stop);
}

/// A 32x32 solid red PNG.
const RED_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAIAAAD8GO2jAAAAKElEQVR4nO3NsQ0AAAzCMP5/un0CNkuZ41wybXsHAAAAAAAAAAAAxR4yw/wuPL6QkAAAAABJRU5ErkJggg==";

#[tokio::test]
#[ignore = "live: set HARNESS_LIVE_ANTHROPIC=1 and ANTHROPIC_API_KEY"]
async fn image_input_is_understood() {
    let Some(key) = key() else { return };
    let p = provider(&key, |_| {});
    let mut req = ChatRequest::new(MODEL);
    req.max_tokens = Some(20);
    req.messages.push(Message {
        role: Role::User,
        content: vec![
            ContentBlock::Image {
                source: harness_providers::model::ImageSource::Base64 {
                    media_type: "image/png".into(),
                    data: RED_PNG.into(),
                },
            },
            ContentBlock::text("What colour is this image? One word."),
        ],
    });
    let r = call(&p, req, "image").await;
    assert!(
        r.message.text().to_lowercase().contains("red"),
        "{:?}",
        r.message
    );
}
