//! Live smoke test (TASK-151): Anthropic prompt caching through OpenRouter.
//!
//! Ignored by default because it calls the real API. With
//! `OPENROUTER_API_KEY` set, run it with
//! `cargo nextest run -p harness-providers --test openrouter_cache_live --run-ignored all`.
//! It sends two tiny requests (a ~1.5k-token system prompt, 5 output
//! tokens) to the cheapest Anthropic model and expects the second one to
//! read the cache the first one wrote. The key is never printed.

use harness_providers::model::{ChatRequest, Message};
use harness_providers::openrouter::{OpenRouterOptions, OpenRouterProvider};
use harness_providers::provider::Provider;
use tokio_util::sync::CancellationToken;

/// The cheapest Anthropic model on OpenRouter when this was written;
/// `HARNESS_LIVE_OPENROUTER_MODEL` picks another Anthropic model.
const MODEL: &str = "anthropic/claude-haiku-5.5";

#[tokio::test]
#[ignore = "live: needs OPENROUTER_API_KEY; run with --run-ignored"]
async fn second_request_reads_the_prompt_cache() {
    let Some(key) = std::env::var("OPENROUTER_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
    else {
        eprintln!("OPENROUTER_API_KEY is not set; skipping");
        return;
    };
    let provider = OpenRouterProvider::with_key(key.trim(), None, OpenRouterOptions::default())
        .expect("provider");
    // A unique prefix per run, so the first call cannot hit an old entry.
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let rules: String = (0..150)
        .map(|i| format!("Rule {i}: answer in as few words as possible.\n"))
        .collect();
    let model = std::env::var("HARNESS_LIVE_OPENROUTER_MODEL").unwrap_or_else(|_| MODEL.into());
    let mut req = ChatRequest::new(model.as_str());
    req.system = Some(format!("Session {nonce}.\n{rules}"));
    req.messages = vec![Message::user("Say hi.")];
    req.max_tokens = Some(5);

    let first = provider
        .complete(req.clone(), CancellationToken::new())
        .await
        .expect("first request");
    let second = provider
        .complete(req, CancellationToken::new())
        .await
        .expect("second request");
    eprintln!(
        "first: read {} written {}; second: read {} written {}",
        first.usage.cache_read_tokens,
        first.usage.cache_write_tokens,
        second.usage.cache_read_tokens,
        second.usage.cache_write_tokens
    );
    assert!(
        first.usage.cache_write_tokens > 0,
        "first call wrote no cache: {:?}",
        first.usage
    );
    assert!(
        second.usage.cache_read_tokens > 0,
        "second call read no cache: {:?}",
        second.usage
    );
}
