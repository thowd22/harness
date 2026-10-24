//! Anthropic provider tests against a local mock Messages API server.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use harness_providers::anthropic::{AnthropicConfig, AnthropicProvider};
use harness_providers::model::{
    ChatRequest, ContentBlock, Message, ReasoningEffort, Role, StopKind, ThinkingConfig, ToolSpec,
};
use harness_providers::provider::{Provider, ProviderError, StreamEvent, collect};
use harness_providers::retry::RetryPolicy;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

/// A recorded request: lower-cased head and parsed JSON body.
#[derive(Clone, Debug)]
struct Recorded {
    head: String,
    body: Value,
}

/// Serves the n-th connection the n-th SSE body (the last repeats), or a
/// status reply for `(status, body)` entries.
struct Mock {
    url: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
}

#[derive(Clone)]
enum Reply {
    Sse(String),
    Status(u16, String),
}

impl Mock {
    async fn start(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let rec = requests.clone();
        tokio::spawn(async move {
            let mut n = 0;
            loop {
                let (sock, _) = listener.accept().await.unwrap();
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

async fn serve(mut sock: TcpStream, reply: Reply, rec: Arc<Mutex<Vec<Recorded>>>) {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    let header_end = loop {
        let n = sock.read(&mut tmp).await.unwrap_or(0);
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_ascii_lowercase();
    let len: usize = head
        .lines()
        .find_map(|l| {
            l.strip_prefix("content-length:")
                .map(|v| v.trim().parse().unwrap())
        })
        .unwrap_or(0);
    while buf.len() < header_end + len {
        let n = sock.read(&mut tmp).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let body = serde_json::from_slice(&buf[header_end..]).unwrap_or(Value::Null);
    rec.lock().unwrap().push(Recorded { head, body });
    let out = match reply {
        Reply::Sse(body) => format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{body}"
        ),
        Reply::Status(code, body) => format!(
            "HTTP/1.1 {code} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        ),
    };
    let _ = sock.write_all(out.as_bytes()).await;
}

fn sse(events: &[Value]) -> String {
    events
        .iter()
        .map(|e| format!("event: {}\ndata: {}\n\n", e["type"].as_str().unwrap(), e))
        .collect()
}

/// A turn with live thinking, text and two parallel tool calls.
fn tool_turn() -> String {
    sse(&[
        json!({"type": "message_start", "message": {"id": "msg_1", "model": "claude-opus-5-5", "usage": {
            "input_tokens": 50, "output_tokens": 1, "cache_read_input_tokens": 4000, "cache_creation_input_tokens": 1200}}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": "", "signature": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "Both files "}}),
        json!({"type": "ping"}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "are needed."}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "EqQBCkYIBxgCKkD"}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "content_block_start", "index": 1, "content_block": {"type": "redacted_thinking", "data": "EmwKAhgBEgy3va3pzix"}}),
        json!({"type": "content_block_stop", "index": 1}),
        json!({"type": "content_block_start", "index": 2, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 2, "delta": {"type": "text_delta", "text": "Reading both."}}),
        json!({"type": "content_block_stop", "index": 2}),
        json!({"type": "content_block_start", "index": 3, "content_block": {"type": "tool_use", "id": "toolu_a", "name": "read", "input": {}}}),
        json!({"type": "content_block_delta", "index": 3, "delta": {"type": "input_json_delta", "partial_json": "{\"path\":"}}),
        json!({"type": "content_block_delta", "index": 3, "delta": {"type": "input_json_delta", "partial_json": "\"a.rs\"}"}}),
        json!({"type": "content_block_stop", "index": 3}),
        json!({"type": "content_block_start", "index": 4, "content_block": {"type": "tool_use", "id": "toolu_b", "name": "read", "input": {}}}),
        json!({"type": "content_block_delta", "index": 4, "delta": {"type": "input_json_delta", "partial_json": "{\"path\":\"b.rs\"}"}}),
        json!({"type": "content_block_stop", "index": 4}),
        json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 77}}),
        json!({"type": "message_stop"}),
    ])
}

fn final_turn() -> String {
    sse(&[
        json!({"type": "message_start", "message": {"id": "msg_2", "model": "claude-opus-5-5", "usage": {
            "input_tokens": 20, "output_tokens": 1, "cache_read_input_tokens": 5200, "cache_creation_input_tokens": 90}}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Done."}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 3}}),
        json!({"type": "message_stop"}),
    ])
}

fn provider(url: &str) -> AnthropicProvider {
    let mut cfg = AnthropicConfig::new().with_base_url(url);
    cfg.http.retry = RetryPolicy {
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(5),
        ..RetryPolicy::default()
    };
    AnthropicProvider::new("sk-ant-test-key", cfg).unwrap()
}

fn request() -> ChatRequest {
    let mut req = ChatRequest::new("claude-opus-5-5");
    req.system = Some("You are a coding agent.".into());
    req.tools = vec![ToolSpec {
        name: "read".into(),
        description: "Read a file".into(),
        input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
    }];
    req.thinking = Some(ThinkingConfig {
        budget_tokens: None,
        effort: Some(ReasoningEffort::High),
    });
    req.messages.push(Message::user("Compare a.rs and b.rs"));
    req
}

#[tokio::test]
async fn streams_live_reasoning_text_and_parallel_tool_calls_with_usage() {
    let mock = Mock::start(vec![Reply::Sse(tool_turn())]).await;
    let p = provider(&mock.url);
    let mut stream = p.stream(request(), CancellationToken::new()).await.unwrap();
    let mut events = Vec::new();
    while let Some(e) = stream.next().await {
        events.push(e.unwrap());
    }
    // Reasoning arrives as separate live deltas before the block stops.
    let reasoning: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::ReasoningDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(reasoning, vec!["Both files ", "are needed."]);
    let first_delta = events
        .iter()
        .position(|e| matches!(e, StreamEvent::ReasoningDelta { .. }))
        .unwrap();
    let first_stop = events
        .iter()
        .position(|e| matches!(e, StreamEvent::BlockStop { index: 0 }))
        .unwrap();
    assert!(first_delta < first_stop);
    let starts = events
        .iter()
        .filter(|e| matches!(e, StreamEvent::ToolUseStart { .. }))
        .count();
    assert_eq!(starts, 2);

    let mut builder = harness_providers::provider::ResponseBuilder::new();
    for e in events {
        builder.apply(e).unwrap();
    }
    let resp = builder.finish().unwrap();
    assert_eq!(resp.stop.kind, StopKind::ToolUse);
    assert_eq!(resp.usage.input_tokens, 50);
    assert_eq!(resp.usage.output_tokens, 77);
    assert_eq!(resp.usage.cache_read_tokens, 4000);
    assert_eq!(resp.usage.cache_write_tokens, 1200);
    let calls: Vec<_> = resp
        .message
        .tool_uses()
        .map(|(id, _, input)| (id.to_owned(), input.clone()))
        .collect();
    assert_eq!(
        calls,
        vec![
            ("toolu_a".into(), json!({"path": "a.rs"})),
            ("toolu_b".into(), json!({"path": "b.rs"}))
        ]
    );

    let sent = &mock.recorded()[0];
    assert!(sent.head.starts_with("post /v1/messages "));
    assert!(sent.head.contains("x-api-key: sk-ant-test-key"));
    assert!(sent.head.contains("anthropic-version: 2026-10-21"));
    assert_eq!(sent.body["stream"], true);
    assert_eq!(sent.body["thinking"]["type"], "adaptive");
    assert_eq!(sent.body["output_config"]["effort"], "high");
    assert_eq!(sent.body["tools"][0]["cache_control"]["type"], "ephemeral");
    assert_eq!(sent.body["system"][0]["cache_control"]["type"], "ephemeral");
    assert_eq!(
        sent.body["messages"][0]["content"][0]["cache_control"]["type"],
        "ephemeral"
    );
}

#[tokio::test]
async fn thinking_blocks_survive_a_tool_loop_byte_for_byte() {
    let mock = Mock::start(vec![Reply::Sse(tool_turn()), Reply::Sse(final_turn())]).await;
    let p = provider(&mock.url);
    let mut req = request();
    let first = p
        .complete(req.clone(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(first.message.role, Role::Assistant);
    req.messages.push(first.message.clone());
    req.messages.push(Message {
        role: Role::User,
        content: vec![
            ContentBlock::tool_result("toolu_a", "fn a() {}", false),
            ContentBlock::tool_result("toolu_b", "fn b() {}", false),
        ],
    });
    let second = p.complete(req, CancellationToken::new()).await.unwrap();
    assert_eq!(second.message.text(), "Done.");
    assert_eq!(second.usage.cache_read_tokens, 5200);

    let sent = &mock.recorded()[1].body;
    let assistant = &sent["messages"][1];
    assert_eq!(assistant["role"], "assistant");
    let blocks = assistant["content"].as_array().unwrap();
    assert_eq!(
        blocks[0],
        json!({"type": "thinking", "thinking": "Both files are needed.", "signature": "EqQBCkYIBxgCKkD"})
    );
    assert_eq!(
        blocks[1],
        json!({"type": "redacted_thinking", "data": "EmwKAhgBEgy3va3pzix"})
    );
    assert_eq!(blocks[2]["text"], "Reading both.");
    assert_eq!(blocks[3]["id"], "toolu_a");
    assert_eq!(blocks[4]["id"], "toolu_b");
    // Both tool results go back in one user message.
    let results = sent["messages"][2]["content"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[1]["cache_control"]["type"], "ephemeral");
}

#[tokio::test]
async fn mid_stream_overload_is_retried_and_partial_output_discarded() {
    let overloaded = format!(
        "{}{}",
        sse(&[
            json!({"type": "message_start", "message": {"id": "msg_x", "usage": {"input_tokens": 1}}}),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "partial"}}),
        ]),
        "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n"
    );
    let mock = Mock::start(vec![Reply::Sse(overloaded), Reply::Sse(final_turn())]).await;
    let p = provider(&mock.url);
    let resp = p
        .complete(request(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(resp.message.text(), "Done.");
    assert_eq!(resp.retries.len(), 1);
    assert!(resp.retries[0].discard_partial);
    assert_eq!(mock.recorded().len(), 2);
}

#[tokio::test]
async fn http_errors_map_to_typed_errors_without_retrying_permanent_ones() {
    let body = r#"{"type":"error","error":{"type":"invalid_request_error","message":"max_tokens: too large"}}"#;
    let mock = Mock::start(vec![Reply::Status(400, body.into())]).await;
    let p = provider(&mock.url);
    let err = match p.stream(request(), CancellationToken::new()).await {
        Ok(s) => collect(s).await.unwrap_err(),
        Err(e) => e,
    };
    match err.root() {
        ProviderError::Http {
            status: 400,
            message,
            ..
        } => assert!(message.contains("max_tokens")),
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(mock.recorded().len(), 1);

    let mock = Mock::start(vec![Reply::Status(
        401,
        r#"{"error":{"message":"invalid x-api-key"}}"#.into(),
    )])
    .await;
    let err = collect(
        provider(&mock.url)
            .stream(request(), CancellationToken::new())
            .await
            .unwrap(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err.root(), ProviderError::Auth(_)));
    assert!(!err.to_string().contains("sk-ant-test-key"));
}

/// The pre-warm call: one non-streaming `max_tokens: 0` request carrying
/// the real call's tools, system and thinking, then the real call reads
/// what it wrote.
#[tokio::test]
async fn cache_prewarm_sends_a_non_generating_call_first() {
    let warm = json!({"id": "msg_w", "type": "message", "role": "assistant", "content": [],
        "stop_reason": "max_tokens", "usage": {"input_tokens": 1, "output_tokens": 0,
        "cache_creation_input_tokens": 4100, "cache_read_input_tokens": 0}});
    let mock = Mock::start(vec![
        Reply::Status(200, warm.to_string()),
        Reply::Sse(tool_turn()),
    ])
    .await;
    let p = provider(&mock.url);
    let mut req = ChatRequest::new("claude-opus-5-5");
    req.system = Some("You are terse.".into());
    req.tools = vec![ToolSpec {
        name: "read".into(),
        description: "read a file".into(),
        input_schema: json!({"type": "object"}),
    }];
    req.thinking = Some(ThinkingConfig {
        budget_tokens: None,
        effort: Some(ReasoningEffort::High),
    });
    assert!(p.can_prewarm_cache(&req));
    let usage = p
        .prewarm_cache(req.clone(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!((usage.cache_write_tokens, usage.output_tokens), (4100, 0));
    req.messages.push(Message::user("read a and b"));
    let resp = collect(p.stream(req, CancellationToken::new()).await.unwrap())
        .await
        .unwrap();
    assert_eq!(resp.stop.kind, StopKind::ToolUse);

    let rec = mock.recorded();
    assert_eq!(rec.len(), 2);
    let (w, r) = (&rec[0].body, &rec[1].body);
    assert!(
        rec[0].head.starts_with("post /v1/messages "),
        "{}",
        rec[0].head
    );
    assert_eq!(w["stream"], false);
    assert_eq!(w["max_tokens"], 0);
    assert_eq!(r["stream"], true);
    for k in ["model", "tools", "system", "thinking", "output_config"] {
        assert_eq!(w[k], r[k], "{k} must match the real call");
    }
    assert_eq!(w["messages"][0]["content"][0]["text"], "warmup");
    assert!(
        w["messages"][0]["content"][0]
            .get("cache_control")
            .is_none()
    );
    assert!(w["tools"][0].get("cache_control").is_some());
    assert!(w["system"][0].get("cache_control").is_some());
}

/// Live smoke test; runs only with `HARNESS_LIVE_ANTHROPIC=1` and
/// `ANTHROPIC_API_KEY` set.
#[tokio::test]
#[ignore = "live network test; set HARNESS_LIVE_ANTHROPIC=1 and run with --ignored"]
async fn live_smoke() {
    if std::env::var("HARNESS_LIVE_ANTHROPIC").as_deref() != Ok("1") {
        return;
    }
    let key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY");
    let p = AnthropicProvider::new(&key, AnthropicConfig::new()).unwrap();
    let mut req = ChatRequest::new("claude-haiku-5-5");
    req.max_tokens = Some(64);
    req.messages.push(Message::user("Say hi."));
    let resp = p.complete(req, CancellationToken::new()).await.unwrap();
    assert!(!resp.message.text().is_empty());
}
