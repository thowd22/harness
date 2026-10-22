//! Provider conformance suite.
//!
//! Every provider is held to the same contract, replayed offline from
//! recorded, redacted fixtures in `tests/fixtures/conformance/<provider>/`:
//!
//! | scenario              | contract                                                        |
//! |-----------------------|-----------------------------------------------------------------|
//! | `plain_text`          | text `Hello!`, stop `end_turn`, usage reported                  |
//! | `single_tool`         | one `tool_use` (`read`, `{"path": "src/main.rs"}`), stop `tool_use` |
//! | `parallel_tools`      | two `tool_use` blocks in order with distinct ids                |
//! | `malformed_tool_json` | the call survives; raw arguments kept as a JSON string           |
//! | `thinking`            | a reasoning carrier comes back and is resent verbatim next turn |
//! | `max_tokens`          | stop `max_tokens`                                               |
//! | `rate_limit`          | a 429 is retried once, then the call succeeds                    |
//! | `overloaded`          | a mid-stream overload is retried, partial output discarded       |
//!
//! Live smoke tests (`live_*`) run the first two scenarios against the real
//! APIs only when `HARNESS_LIVE_TESTS=1` and the provider's key variable is
//! set. With `HARNESS_RECORD_FIXTURES=<dir>` they also record the raw
//! responses through a local proxy, redacted, in the fixture format.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use harness_providers::anthropic::{AnthropicConfig, AnthropicProvider};
use harness_providers::fake::{FakeBlock, FakeError, FakeErrorKind, FakeProvider, FakeTurn};
use harness_providers::http::HttpConfig;
use harness_providers::model::{
    ChatRequest, ContentBlock, Message, StopKind, ThinkingConfig, ToolSpec,
};
use harness_providers::openai::OpenAiProvider;
use harness_providers::openrouter::{OpenRouterOptions, OpenRouterProvider};
use harness_providers::provider::{Provider, ProviderError, Response};
use harness_providers::retry::{RetryPolicy, RetryReason};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

/// Key used against the mock server; fixtures must never contain it.
const TEST_KEY: &str = "sk-conformance-test-key-0123456789abcdef";

// ---------------------------------------------------------------- fixtures

/// One canned HTTP reply.
#[derive(Debug, Clone)]
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/conformance")
}

/// Parses a fixture: `#` comment lines, then replies each introduced by
/// `--- <status>[; header: value]...`.
fn parse_fixture(text: &str) -> Vec<Reply> {
    let mut replies: Vec<Reply> = Vec::new();
    for line in text.split_inclusive('\n') {
        if let Some(head) = line.strip_prefix("--- ") {
            let mut parts = head.trim_end().split(';');
            let status = parts
                .next()
                .unwrap()
                .trim()
                .parse()
                .expect("fixture status");
            let headers = parts
                .filter_map(|h| h.split_once(':'))
                .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
                .collect();
            replies.push(Reply {
                status,
                headers,
                body: String::new(),
            });
        } else if let Some(r) = replies.last_mut() {
            r.body.push_str(line);
        } else {
            assert!(
                line.starts_with('#') || line.trim().is_empty(),
                "text before the first reply: {line}"
            );
        }
    }
    assert!(!replies.is_empty(), "fixture has no replies");
    replies
}

fn load_fixture(provider: &str, scenario: &str) -> Vec<Reply> {
    let path = fixture_dir().join(provider).join(format!("{scenario}.txt"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    parse_fixture(&text)
}

/// Masks credentials in recorded text: the given keys plus common key
/// shapes and auth headers.
fn redact_fixture(text: &str, keys: &[&str]) -> String {
    let mut out = text.to_owned();
    for k in keys.iter().filter(|k| k.len() >= 8) {
        out = out.replace(k, "[REDACTED]");
    }
    let mut result = String::with_capacity(out.len());
    for line in out.split_inclusive('\n') {
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("authorization:") || lower.starts_with("x-api-key:") {
            continue;
        }
        result.push_str(&mask_key_shapes(line));
    }
    result
}

/// Replaces `sk-...`/`sk-ant-...`/`sk-or-...` style tokens.
fn mask_key_shapes(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(pos) = rest.find("sk-") {
        let (before, after) = rest.split_at(pos);
        let boundary = before
            .chars()
            .last()
            .is_none_or(|c| !c.is_ascii_alphanumeric());
        let len = after
            .char_indices()
            .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '-' || *c == '_'))
            .map_or(after.len(), |(i, _)| i);
        out.push_str(before);
        if boundary && len >= 20 {
            out.push_str("[REDACTED]");
        } else {
            out.push_str(&after[..len.max(3)]);
        }
        rest = &after[len.max(3)..];
    }
    out.push_str(rest);
    out
}

/// Fails if `text` looks like it holds a credential.
fn assert_no_secrets(name: &str, text: &str) {
    assert_eq!(
        redact_fixture(text, &[TEST_KEY]),
        text,
        "{name} contains something that looks like a credential"
    );
    for needle in ["Bearer ", "x-api-key", "api_key\"", "Authorization"] {
        assert!(!text.contains(needle), "{name} contains `{needle}`");
    }
}

// ------------------------------------------------------------- mock server

#[derive(Clone, Debug)]
struct Recorded {
    head: String,
    body: Value,
}

struct Mock {
    url: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
}

impl Mock {
    /// Serves the n-th connection the n-th reply (the last one repeats).
    async fn start(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let rec = requests.clone();
        tokio::spawn(async move {
            let mut n = 0;
            while let Ok((sock, _)) = listener.accept().await {
                let reply = replies[n.min(replies.len() - 1)].clone();
                n += 1;
                let rec = rec.clone();
                tokio::spawn(async move { serve(sock, reply, rec).await });
            }
        });
        Self { url, requests }
    }

    fn recorded(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }
}

async fn read_request(sock: &mut TcpStream) -> Option<(String, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    let header_end = loop {
        let n = sock.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let len: usize = head
        .lines()
        .find_map(|l| {
            let l = l.to_ascii_lowercase();
            l.strip_prefix("content-length:")
                .map(|v| v.trim().parse().unwrap_or(0))
        })
        .unwrap_or(0);
    while buf.len() < header_end + len {
        let n = sock.read(&mut tmp).await.ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    Some((head, buf[header_end..].to_vec()))
}

async fn serve(mut sock: TcpStream, reply: Reply, rec: Arc<Mutex<Vec<Recorded>>>) {
    let Some((head, body)) = read_request(&mut sock).await else {
        return;
    };
    let body = serde_json::from_slice(&body).unwrap_or(Value::Null);
    rec.lock().unwrap().push(Recorded {
        head: head.to_ascii_lowercase(),
        body,
    });
    let content_type = if reply.status == 200 {
        "text/event-stream"
    } else {
        "application/json"
    };
    let mut out = format!(
        "HTTP/1.1 {} X\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n",
        reply.status,
        reply.body.len()
    );
    for (k, v) in &reply.headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    out.push_str(&reply.body);
    let _ = sock.write_all(out.as_bytes()).await;
}

// --------------------------------------------------------------- providers

fn fast_retry() -> RetryPolicy {
    RetryPolicy {
        max_retries: 2,
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(5),
        jitter: 0.0,
        max_retry_after: Duration::from_secs(1),
    }
}

fn http(url: &str) -> HttpConfig {
    let mut cfg = HttpConfig::new(url);
    cfg.retry = fast_retry();
    cfg
}

/// The provider `name` talking to `url`, plus the model used for it.
fn provider(name: &str, url: &str, key: &str) -> (Arc<dyn Provider>, &'static str) {
    match name {
        "anthropic" => {
            let mut cfg = AnthropicConfig::new().with_base_url(url);
            cfg.http.retry = fast_retry();
            (
                Arc::new(AnthropicProvider::new(key, cfg).unwrap()),
                "claude-sonnet-4-6",
            )
        }
        "openai" => (
            Arc::new(OpenAiProvider::new(key, http(url)).unwrap()),
            "gpt-5",
        ),
        "openrouter" => (
            Arc::new(
                OpenRouterProvider::new(key, http(url), OpenRouterOptions::default()).unwrap(),
            ),
            "anthropic/claude-sonnet-4.6",
        ),
        other => panic!("unknown provider {other}"),
    }
}

/// What carries reasoning across turns for each provider.
fn reasoning_carrier(name: &str) -> &'static str {
    match name {
        "openai" => "enc-conformance",
        _ => "sig-conformance",
    }
}

fn tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "read".into(),
            description: "Read a file".into(),
            input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}),
        },
        ToolSpec {
            name: "bash".into(),
            description: "Run a command".into(),
            input_schema: json!({"type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"]}),
        },
    ]
}

fn request(model: &str, messages: Vec<Message>, thinking: bool) -> ChatRequest {
    let mut r = ChatRequest::new(model);
    r.system = Some("You are a test.".into());
    r.messages = messages;
    r.tools = tools();
    r.max_tokens = Some(4096);
    if thinking {
        r.thinking = Some(ThinkingConfig {
            budget_tokens: Some(2048),
            effort: None,
        });
    }
    r
}

async fn complete(p: &Arc<dyn Provider>, req: ChatRequest) -> Result<Response, ProviderError> {
    p.complete(req, CancellationToken::new()).await
}

// ---------------------------------------------------------------- contract

fn tool_uses(r: &Response) -> Vec<(String, String, Value)> {
    r.message
        .tool_uses()
        .map(|(id, name, input)| (id.to_owned(), name.to_owned(), input.clone()))
        .collect()
}

fn check_plain_text(r: &Response) {
    assert_eq!(r.message.text(), "Hello!");
    assert_eq!(r.stop.kind, StopKind::EndTurn);
    assert!(
        r.usage.output_tokens > 0,
        "usage must be reported: {:?}",
        r.usage
    );
    assert!(tool_uses(r).is_empty());
}

fn check_single_tool(r: &Response) {
    let calls = tool_uses(r);
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0].1, "read");
    assert_eq!(calls[0].2, json!({"path": "src/main.rs"}));
    assert!(!calls[0].0.is_empty());
    assert_eq!(r.stop.kind, StopKind::ToolUse);
}

fn check_parallel_tools(r: &Response) {
    let calls = tool_uses(r);
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(calls[0].2, json!({"path": "a.rs"}));
    assert_eq!(calls[1].2, json!({"path": "b.rs"}));
    assert_ne!(calls[0].0, calls[1].0, "parallel calls need distinct ids");
    assert_eq!(r.stop.kind, StopKind::ToolUse);
}

fn check_malformed_tool_json(r: &Response) {
    let calls = tool_uses(r);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1, "bash");
    assert_eq!(
        calls[0].2,
        Value::String("{\"command\": \"ls\",".into()),
        "raw arguments must be kept for repair"
    );
}

fn check_max_tokens(r: &Response) {
    assert_eq!(r.stop.kind, StopKind::MaxTokens);
    assert!(r.message.text().starts_with("Hello!"));
}

fn check_retried(r: &Response, reason: RetryReason) {
    check_plain_text(r);
    assert_eq!(r.retries.len(), 1, "{:?}", r.retries);
    assert_eq!(r.retries[0].reason, reason);
}

fn has_reasoning(r: &Response, carrier: &str) -> bool {
    r.message.content.iter().any(|b| match b {
        ContentBlock::Reasoning { signature, .. } => signature.as_deref() == Some(carrier),
        ContentBlock::Opaque { data, .. } => data.to_string().contains(carrier),
        _ => false,
    })
}

async fn run_scenario(name: &str, scenario: &str) {
    let mock = Mock::start(load_fixture(name, scenario)).await;
    let (p, model) = provider(name, &mock.url, TEST_KEY);
    let hi = vec![Message::user("hi")];
    match scenario {
        "plain_text" => check_plain_text(&complete(&p, request(model, hi, false)).await.unwrap()),
        "single_tool" => check_single_tool(&complete(&p, request(model, hi, false)).await.unwrap()),
        "parallel_tools" => {
            check_parallel_tools(&complete(&p, request(model, hi, false)).await.unwrap())
        }
        "malformed_tool_json" => {
            check_malformed_tool_json(&complete(&p, request(model, hi, false)).await.unwrap())
        }
        "max_tokens" => check_max_tokens(&complete(&p, request(model, hi, false)).await.unwrap()),
        "rate_limit" => {
            check_retried(
                &complete(&p, request(model, hi, false)).await.unwrap(),
                RetryReason::RateLimited,
            );
            assert_eq!(mock.recorded().len(), 2);
        }
        "overloaded" => {
            check_retried(
                &complete(&p, request(model, hi, false)).await.unwrap(),
                RetryReason::Overloaded,
            );
            assert!(mock.recorded().len() >= 2);
        }
        "thinking" => {
            let carrier = reasoning_carrier(name);
            let first = complete(&p, request(model, hi.clone(), true))
                .await
                .unwrap();
            assert!(
                has_reasoning(&first, carrier),
                "no reasoning carrier in {:?}",
                first.message
            );
            assert_eq!(first.message.text(), "Hello!");
            let mut history = hi;
            history.push(first.message.clone());
            history.push(Message::user("continue"));
            check_plain_text(&complete(&p, request(model, history, true)).await.unwrap());
            let second = mock.recorded()[1].body.to_string();
            assert!(
                second.contains(carrier),
                "reasoning was not sent back verbatim: {second}"
            );
        }
        other => panic!("unknown scenario {other}"),
    }
    for r in mock.recorded() {
        assert!(
            !r.body.to_string().contains(TEST_KEY),
            "key leaked into a request body"
        );
        assert!(
            r.head.contains(&TEST_KEY.to_ascii_lowercase()),
            "key must be sent as a header"
        );
    }
}

macro_rules! conformance {
    ($provider:ident: $($scenario:ident),* $(,)?) => {
        mod $provider {
            $(
                #[tokio::test]
                async fn $scenario() {
                    super::run_scenario(stringify!($provider), stringify!($scenario)).await;
                }
            )*
        }
    };
}

conformance!(anthropic: plain_text, single_tool, parallel_tools, malformed_tool_json, thinking, max_tokens, rate_limit, overloaded);
conformance!(openai: plain_text, single_tool, parallel_tools, malformed_tool_json, thinking, max_tokens, rate_limit, overloaded);
conformance!(openrouter: plain_text, single_tool, parallel_tools, malformed_tool_json, thinking, max_tokens, rate_limit, overloaded);

const PROVIDERS: [&str; 3] = ["anthropic", "openai", "openrouter"];
const SCENARIOS: [&str; 8] = [
    "plain_text",
    "single_tool",
    "parallel_tools",
    "malformed_tool_json",
    "thinking",
    "max_tokens",
    "rate_limit",
    "overloaded",
];

#[test]
fn every_provider_has_every_fixture_and_fixtures_are_redacted() {
    for p in PROVIDERS {
        for s in SCENARIOS {
            let path = fixture_dir().join(p).join(format!("{s}.txt"));
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            assert_no_secrets(&path.display().to_string(), &text);
            assert!(!parse_fixture(&text).is_empty());
        }
    }
}

#[test]
fn fixture_redaction_masks_keys_and_auth_headers() {
    let raw = "authorization: Bearer abc\nx-api-key: sk-ant-api03-abcdefghijklmnopqrstuvwxyz\n\
               data: {\"echo\":\"sk-or-v1-0123456789abcdef0123\",\"key\":\"my-live-key-123\"}\n";
    let red = redact_fixture(raw, &["my-live-key-123"]);
    assert_eq!(
        red,
        "data: {\"echo\":\"[REDACTED]\",\"key\":\"[REDACTED]\"}\n"
    );
    assert_eq!(
        mask_key_shapes("task-runner sk-short"),
        "task-runner sk-short"
    );
}

// ------------------------------------------------------- the fake provider

/// The fake provider follows the same response contract (it has no HTTP
/// retry layer, so errors surface directly and must classify correctly).
#[tokio::test]
async fn fake_provider_meets_the_contract() {
    let tool = |id: &str, name: &str, input: Value| FakeBlock::ToolUse {
        id: Some(id.into()),
        name: name.into(),
        input,
    };
    let usage = harness_providers::model::Usage {
        input_tokens: 12,
        output_tokens: 8,
        ..Default::default()
    };
    let turn = |content: Vec<FakeBlock>| FakeTurn {
        content,
        usage,
        ..FakeTurn::default()
    };
    let fake = FakeProvider::with_turns([
        turn(vec![FakeBlock::Text("Hello!".into())]),
        turn(vec![tool("t1", "read", json!({"path": "src/main.rs"}))]),
        turn(vec![
            tool("t1", "read", json!({"path": "a.rs"})),
            tool("t2", "read", json!({"path": "b.rs"})),
        ]),
        turn(vec![tool("t1", "bash", json!("{\"command\": \"ls\","))]),
        turn(vec![
            FakeBlock::Reasoning {
                text: "Let me think.".into(),
                signature: Some("sig-conformance".into()),
            },
            FakeBlock::Text("Hello!".into()),
        ]),
        turn(vec![FakeBlock::Text("Hello!".into())]),
        FakeTurn {
            stop: Some(StopKind::MaxTokens),
            ..turn(vec![FakeBlock::Text("Hello! This is cut".into())])
        },
        FakeTurn::error(FakeError::new(FakeErrorKind::RateLimited, "slow down")),
        FakeTurn::error(FakeError::new(FakeErrorKind::Overloaded, "busy")),
    ]);
    let p: Arc<dyn Provider> = Arc::new(fake.clone());
    let hi = || vec![Message::user("hi")];
    check_plain_text(&complete(&p, request("m", hi(), false)).await.unwrap());
    check_single_tool(&complete(&p, request("m", hi(), false)).await.unwrap());
    check_parallel_tools(&complete(&p, request("m", hi(), false)).await.unwrap());
    check_malformed_tool_json(&complete(&p, request("m", hi(), false)).await.unwrap());
    let first = complete(&p, request("m", hi(), true)).await.unwrap();
    assert!(has_reasoning(&first, "sig-conformance"));
    let mut history = hi();
    history.push(first.message);
    history.push(Message::user("continue"));
    check_plain_text(&complete(&p, request("m", history, true)).await.unwrap());
    let resent = serde_json::to_string(&fake.last_request().unwrap().messages).unwrap();
    assert!(resent.contains("sig-conformance"));
    check_max_tokens(&complete(&p, request("m", hi(), false)).await.unwrap());
    let e = complete(&p, request("m", hi(), false)).await.unwrap_err();
    assert_eq!(e.retry_reason(), Some(RetryReason::RateLimited));
    let e = complete(&p, request("m", hi(), false)).await.unwrap_err();
    assert_eq!(e.retry_reason(), Some(RetryReason::Overloaded));
}

// -------------------------------------------------------- live smoke tests

/// Live credentials for `provider`, when live tests are enabled.
fn live_key(provider: &str) -> Option<String> {
    if std::env::var("HARNESS_LIVE_TESTS").ok().as_deref() != Some("1") {
        eprintln!("skipping live {provider} test: set HARNESS_LIVE_TESTS=1 to run it");
        return None;
    }
    let var = match provider {
        "anthropic" => "ANTHROPIC_API_KEY",
        "openai" => "OPENAI_API_KEY",
        _ => "OPENROUTER_API_KEY",
    };
    let key = std::env::var(var).ok().filter(|k| !k.is_empty());
    if key.is_none() {
        eprintln!("skipping live {provider} test: {var} is not set");
    }
    key
}

fn upstream(provider: &str) -> &'static str {
    match provider {
        "anthropic" => harness_providers::anthropic::DEFAULT_BASE_URL,
        "openai" => harness_providers::openai::DEFAULT_BASE_URL,
        _ => harness_providers::openrouter::DEFAULT_BASE_URL,
    }
}

/// A local proxy that forwards to the real API and appends each redacted
/// response to `out` in the fixture format.
async fn recording_proxy(provider: &'static str, key: String, out: PathBuf) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let base = upstream(provider).trim_end_matches('/').to_owned();
    tokio::spawn(async move {
        let client = reqwest::Client::new();
        while let Ok((mut sock, _)) = listener.accept().await {
            let Some((head, body)) = read_request(&mut sock).await else {
                continue;
            };
            let mut lines = head.lines();
            let path = lines
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
                .unwrap_or("/")
                .to_owned();
            // `base` may carry a path prefix such as /v1 that the provider
            // also sends; avoid doubling it.
            let target = match base.rfind("/v1").or_else(|| base.rfind("/api/v1")) {
                Some(i) if path.starts_with(&base[i..]) => format!("{}{path}", &base[..i]),
                _ => format!("{base}{path}"),
            };
            let mut req = client.post(target).body(body);
            for line in lines {
                if let Some((k, v)) = line.split_once(':') {
                    let k = k.trim().to_ascii_lowercase();
                    if !matches!(
                        k.as_str(),
                        "host" | "content-length" | "connection" | "accept-encoding"
                    ) {
                        req = req.header(k, v.trim());
                    }
                }
            }
            let Ok(resp) = req.send().await else { continue };
            let status = resp.status().as_u16();
            let bytes = resp.bytes().await.unwrap_or_default();
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let record = format!("--- {status}\n{}", redact_fixture(&text, &[&key]));
            let mut existing = std::fs::read_to_string(&out)
                .unwrap_or_else(|_| "# recorded live; redacted\n".into());
            existing.push_str(&record);
            let _ = std::fs::write(&out, existing);
            let reply = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                bytes.len()
            );
            let _ = sock.write_all(reply.as_bytes()).await;
            let _ = sock.write_all(&bytes).await;
        }
    });
    url
}

async fn live(provider: &'static str) {
    let Some(key) = live_key(provider) else {
        return;
    };
    let base = match std::env::var("HARNESS_RECORD_FIXTURES").ok() {
        Some(dir) => {
            std::fs::create_dir_all(&dir).unwrap();
            recording_proxy(
                provider,
                key.clone(),
                Path::new(&dir).join(format!("{provider}_live.txt")),
            )
            .await
        }
        None => upstream(provider).to_owned(),
    };
    let (p, model) = provider_live(provider, &base, &key);
    let mut req = request(
        model,
        vec![Message::user("Reply with exactly: Hello!")],
        false,
    );
    req.tools.clear();
    req.max_tokens = Some(64);
    let r = complete(&p, req).await.expect("live plain text call");
    assert!(r.message.text().contains("Hello"), "{:?}", r.message);
    assert!(r.usage.output_tokens > 0);
    let mut req = request(
        model,
        vec![Message::user("Use the read tool to read src/main.rs.")],
        false,
    );
    req.max_tokens = Some(256);
    let r = complete(&p, req).await.expect("live tool call");
    assert!(
        tool_uses(&r).iter().any(|(_, name, _)| name == "read"),
        "{:?}",
        r.message
    );
}

fn provider_live(name: &str, base: &str, key: &str) -> (Arc<dyn Provider>, &'static str) {
    let (p, _) = provider(name, base, key);
    let model = match name {
        "anthropic" => "claude-haiku-4-5",
        "openai" => "gpt-5-mini",
        _ => "openai/gpt-5-mini",
    };
    (p, model)
}

#[tokio::test]
async fn live_anthropic() {
    live("anthropic").await;
}

#[tokio::test]
async fn live_openai() {
    live("openai").await;
}

#[tokio::test]
async fn live_openrouter() {
    live("openrouter").await;
}
