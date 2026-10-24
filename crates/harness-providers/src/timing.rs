//! Opt-in timing probe for `harness bench` (see `docs/performance.md`).
//!
//! Nothing here runs unless [`install`] was called: [`wrap`] returns the
//! provider unchanged and [`active`] is one atomic load, so the normal path
//! pays nothing. Once installed, every provider passed through [`wrap`]
//! records a [`CallTiming`] per call (time to first event, to first
//! content, tool-block completion times, end of stream, usage, request
//! body sizes and how much of the request prefix matched the previous
//! request), and other crates record named [`Span`]s (request hooks, turn
//! phases) with [`Probe::span`].
//!
//! Times are microseconds since the probe was installed. The probe also
//! keeps the wall-clock time of that instant, so run events (stamped with
//! wall-clock time) can be placed on the same axis.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::model::{Capabilities, ChatRequest, Usage};
use crate::provider::{EventStream, Provider, ProviderError, StreamEvent};

static PROBE: OnceLock<Arc<Probe>> = OnceLock::new();

/// Installs the process-wide probe (idempotent) and returns it.
pub fn install() -> Arc<Probe> {
    PROBE.get_or_init(|| Arc::new(Probe::new())).clone()
}

/// The installed probe, if any.
pub fn active() -> Option<&'static Arc<Probe>> {
    PROBE.get()
}

/// `provider` wrapped in a [`TimedProvider`] when the probe is installed,
/// otherwise `provider` itself.
pub fn wrap(provider: Arc<dyn Provider>) -> Arc<dyn Provider> {
    match active() {
        Some(probe) => Arc::new(TimedProvider {
            inner: provider,
            probe: probe.clone(),
        }),
        None => provider,
    }
}

/// Serialized sizes of one request body, by part.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BodySizes {
    /// The whole body as JSON bytes.
    pub total: u64,
    /// The system prompt (a top-level `system` field, or the leading
    /// `system`/`developer` messages of chat-style bodies).
    pub system: u64,
    /// Tool definitions.
    pub tools: u64,
    /// The conversation messages (without the system messages).
    pub messages: u64,
    /// Number of conversation messages.
    pub message_count: u64,
    /// Bytes of the cacheable prefix (tools, then system, then messages,
    /// each serialized) identical to the previous request's.
    pub prefix_shared: u64,
    /// Bytes of that prefix in this request.
    pub prefix_total: u64,
    /// Bytes of that prefix in the previous request: when it equals
    /// `prefix_shared`, the previous request is an exact prefix of this one
    /// (the cache can be reused in full).
    pub prefix_prev_total: u64,
    /// Tools identical to the previous request's.
    pub tools_unchanged: bool,
    /// System prompt identical to the previous request's.
    pub system_unchanged: bool,
}

/// Timing of one model call, in microseconds since the probe started.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CallTiming {
    /// Call number (0-based, in start order).
    pub index: usize,
    /// Provider name.
    pub provider: String,
    /// Model id.
    pub model: String,
    /// `Provider::stream` was entered.
    pub start_us: u64,
    /// Time spent building and serializing the body for [`BodySizes`]
    /// (measured off the timed path; an estimate of the provider's own
    /// request build + serialize cost).
    pub build_us: u64,
    /// `Provider::stream` returned (the stream is usually lazy: the HTTP
    /// request goes out on first poll).
    pub opened_us: Option<u64>,
    /// First stream event of any kind (≈ response headers plus first SSE
    /// event: time to first byte).
    pub first_event_us: Option<u64>,
    /// First text, reasoning or tool-use event (time to first token).
    pub first_content_us: Option<u64>,
    /// First text or reasoning delta.
    pub first_text_us: Option<u64>,
    /// When each tool-use block was complete (its `block_stop`).
    pub tool_block_stops_us: Vec<u64>,
    /// `message_stop` (or the error) arrived.
    pub end_us: Option<u64>,
    /// Stream events received.
    pub events: u64,
    /// Content deltas received.
    pub deltas: u64,
    /// Characters of text, reasoning and tool arguments streamed.
    pub streamed_chars: u64,
    /// Final usage.
    pub usage: Option<Usage>,
    /// Retries signalled by the transport.
    pub retries: u32,
    /// Error text when the call failed.
    pub error: Option<String>,
    /// Request body sizes (when the provider has a wire format, or the
    /// normalized request otherwise).
    pub body: Option<BodySizes>,
}

/// A named interval, in microseconds since the probe started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    /// What ran (`hook:context`, `turn`, ...).
    pub name: String,
    /// Start.
    pub start_us: u64,
    /// End.
    pub end_us: u64,
}

/// Everything recorded so far.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProbeData {
    /// Wall-clock time of the probe's start, in Unix microseconds.
    pub epoch_unix_us: u64,
    /// Model calls in start order.
    pub calls: Vec<CallTiming>,
    /// Spans in end order.
    pub spans: Vec<Span>,
}

#[derive(Debug, Default)]
struct State {
    data: ProbeData,
    /// Cacheable prefix parts of the last request per provider/model.
    last_prefix: std::collections::HashMap<String, Vec<String>>,
}

/// The process-wide recorder.
#[derive(Debug)]
pub struct Probe {
    epoch: Instant,
    state: Mutex<State>,
}

impl Probe {
    fn new() -> Self {
        let epoch_unix_us = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_micros() as u64);
        Self {
            epoch: Instant::now(),
            state: Mutex::new(State {
                data: ProbeData {
                    epoch_unix_us,
                    ..ProbeData::default()
                },
                ..State::default()
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Microseconds from the probe's start to `at`.
    pub fn us(&self, at: Instant) -> u64 {
        at.saturating_duration_since(self.epoch).as_micros() as u64
    }

    /// Microseconds since the probe's start.
    pub fn now_us(&self) -> u64 {
        self.us(Instant::now())
    }

    /// Records a span from `start` to now.
    pub fn span(&self, name: impl Into<String>, start: Instant) {
        let (start_us, end_us) = (self.us(start), self.now_us());
        self.lock().data.spans.push(Span {
            name: name.into(),
            start_us,
            end_us,
        });
    }

    /// A copy of everything recorded so far.
    pub fn snapshot(&self) -> ProbeData {
        self.lock().data.clone()
    }

    /// Forgets the recorded calls and spans (keeps the epoch).
    pub fn clear(&self) {
        let mut st = self.lock();
        st.data.calls.clear();
        st.data.spans.clear();
        st.last_prefix.clear();
    }

    fn update(&self, index: usize, f: impl FnOnce(&mut CallTiming, u64)) {
        let now = self.now_us();
        let mut st = self.lock();
        if let Some(c) = st.data.calls.get_mut(index) {
            f(c, now);
        }
    }
}

/// `v` without `cache_control` markers (breakpoints move to the newest
/// message every turn; they are not part of the cached content).
fn strip_cache_control(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .filter(|(k, _)| k.as_str() != "cache_control")
                .map(|(k, v)| (k.clone(), strip_cache_control(v)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(strip_cache_control).collect()),
        other => other.clone(),
    }
}

/// Splits a request body into its cacheable parts (tools, system,
/// messages) and their sizes. Parts are compared without `cache_control`
/// markers.
fn body_parts(body: &Value) -> (BodySizes, Vec<String>) {
    let total = serde_json::to_string(body).map_or(0, |s| s.len() as u64);
    let body = &strip_cache_control(body);
    let ser = |v: &Value| serde_json::to_string(v).unwrap_or_default();
    let mut sizes = BodySizes {
        total,
        ..BodySizes::default()
    };
    let mut parts = Vec::new();
    let tools = body.get("tools").map(ser).unwrap_or_default();
    sizes.tools = tools.len() as u64;
    parts.push(tools);
    let mut system = body.get("system").map(ser).unwrap_or_default();
    let mut messages = Vec::new();
    for m in body
        .get("messages")
        .or_else(|| body.get("input"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let role = m.get("role").and_then(Value::as_str);
        if messages.is_empty() && matches!(role, Some("system" | "developer")) {
            system.push_str(&ser(m));
        } else {
            messages.push(ser(m));
        }
    }
    sizes.system = system.len() as u64;
    parts.push(system);
    sizes.message_count = messages.len() as u64;
    sizes.messages = messages.iter().map(|m| m.len() as u64).sum();
    parts.extend(messages);
    (sizes, parts)
}

/// Bytes at the start of `new` (parts concatenated) identical to `old`.
fn shared_prefix(old: &[String], new: &[String]) -> u64 {
    let mut shared = 0u64;
    for (a, b) in old.iter().zip(new) {
        if a == b {
            shared += b.len() as u64;
        } else {
            let common = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
            return shared + common as u64;
        }
    }
    shared
}

/// A provider that records [`CallTiming`]s into the [`Probe`].
pub struct TimedProvider {
    inner: Arc<dyn Provider>,
    probe: Arc<Probe>,
}

impl TimedProvider {
    fn measure_body(&self, request: &ChatRequest) -> (Option<BodySizes>, u64) {
        let started = Instant::now();
        let body = self
            .inner
            .request_body(request)
            .or_else(|| serde_json::to_value(request).ok());
        let Some(body) = body else {
            return (None, 0);
        };
        // The provider serializes the body once more to send it.
        let _ = serde_json::to_vec(&body);
        let build_us = started.elapsed().as_micros() as u64;
        let (mut sizes, parts) = body_parts(&body);
        let key = format!("{}/{}", self.inner.name(), request.model);
        let mut st = self.probe.lock();
        if let Some(old) = st.last_prefix.get(&key) {
            sizes.prefix_shared = shared_prefix(old, &parts);
            sizes.prefix_prev_total = old.iter().map(|p| p.len() as u64).sum();
            sizes.tools_unchanged = old.first() == parts.first();
            sizes.system_unchanged = old.get(1) == parts.get(1);
        }
        sizes.prefix_total = parts.iter().map(|p| p.len() as u64).sum();
        st.last_prefix.insert(key, parts);
        (Some(sizes), build_us)
    }
}

#[async_trait]
impl Provider for TimedProvider {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn capabilities(&self, model: &str) -> Capabilities {
        self.inner.capabilities(model)
    }

    fn request_body(&self, request: &ChatRequest) -> Option<Value> {
        self.inner.request_body(request)
    }

    async fn prewarm(&self) {
        let start = Instant::now();
        self.inner.prewarm().await;
        self.probe.span("prewarm", start);
    }

    fn can_prewarm_cache(&self, request: &ChatRequest) -> bool {
        self.inner.can_prewarm_cache(request)
    }

    async fn prewarm_cache(
        &self,
        request: ChatRequest,
        cancel: CancellationToken,
    ) -> Result<Usage, ProviderError> {
        let start = Instant::now();
        let r = self.inner.prewarm_cache(request, cancel).await;
        self.probe.span("cache_prewarm", start);
        r
    }

    async fn stream(
        &self,
        request: ChatRequest,
        cancel: CancellationToken,
    ) -> Result<EventStream, ProviderError> {
        let start_us = self.probe.now_us();
        let (body, build_us) = self.measure_body(&request);
        let index = {
            let mut st = self.probe.lock();
            let index = st.data.calls.len();
            st.data.calls.push(CallTiming {
                index,
                provider: self.inner.name().to_owned(),
                model: request.model.clone(),
                start_us,
                build_us,
                body,
                ..CallTiming::default()
            });
            index
        };
        // The body measurement is not part of the call: shift the start
        // past it so first-event times stay comparable.
        self.probe.update(index, |c, now| c.start_us = now);
        let opened = self.inner.stream(request, cancel).await;
        let stream = match opened {
            Ok(s) => s,
            Err(e) => {
                let msg = e.to_string();
                self.probe.update(index, |c, now| {
                    c.opened_us = Some(now);
                    c.end_us = Some(now);
                    c.error = Some(msg);
                });
                return Err(e);
            }
        };
        self.probe.update(index, |c, now| c.opened_us = Some(now));
        let probe = self.probe.clone();
        let mut tool_blocks = BTreeSet::new();
        Ok(Box::pin(stream.inspect(move |item| {
            let tool_stop = match item {
                Ok(StreamEvent::ToolUseStart { index: b, .. }) => {
                    tool_blocks.insert(*b);
                    false
                }
                Ok(StreamEvent::BlockStop { index: b }) => tool_blocks.remove(b),
                _ => false,
            };
            probe.update(index, |c, now| {
                c.events += 1;
                c.first_event_us.get_or_insert(now);
                match item {
                    Ok(StreamEvent::TextDelta { text, .. })
                    | Ok(StreamEvent::ReasoningDelta { text, .. }) => {
                        c.first_content_us.get_or_insert(now);
                        c.first_text_us.get_or_insert(now);
                        c.deltas += 1;
                        c.streamed_chars += text.chars().count() as u64;
                    }
                    Ok(StreamEvent::ToolUseStart { .. }) => {
                        c.first_content_us.get_or_insert(now);
                    }
                    Ok(StreamEvent::ToolInputDelta { partial_json, .. }) => {
                        c.deltas += 1;
                        c.streamed_chars += partial_json.chars().count() as u64;
                    }
                    Ok(StreamEvent::Retry(_)) => c.retries += 1,
                    Ok(StreamEvent::MessageStop { usage, .. }) => {
                        c.usage = Some(*usage);
                        c.end_us = Some(now);
                    }
                    Err(e) => {
                        c.error = Some(e.to_string());
                        c.end_us = Some(now);
                    }
                    _ => {}
                }
                if tool_stop {
                    c.tool_block_stops_us.push(now);
                }
            });
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeBlock, FakeProvider, FakeTurn};
    use crate::model::Message;
    use serde_json::json;

    #[test]
    fn body_parts_split_chat_and_messages_bodies() {
        let chat = json!({
            "model": "m",
            "tools": [{"name": "read"}],
            "messages": [
                {"role": "system", "content": "sys"},
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "yo"}
            ]
        });
        let (sizes, parts) = body_parts(&chat);
        assert_eq!(sizes.message_count, 2);
        assert_eq!(sizes.tools, r#"[{"name":"read"}]"#.len() as u64);
        assert_eq!(
            sizes.system,
            r#"{"content":"sys","role":"system"}"#.len() as u64
        );
        assert_eq!(parts.len(), 4);
        let marked = json!({"messages": [{"role": "user", "content": [
            {"type": "text", "text": "x", "cache_control": {"type": "ephemeral"}}]}]});
        let plain =
            json!({"messages": [{"role": "user", "content": [{"type": "text", "text": "x"}]}]});
        assert_eq!(body_parts(&marked).1, body_parts(&plain).1);
        let anthropic = json!({"system": "s", "messages": [{"role": "user", "content": "x"}]});
        let (sizes, _) = body_parts(&anthropic);
        assert_eq!((sizes.system, sizes.message_count, sizes.tools), (3, 1, 0));
    }

    #[test]
    fn shared_prefix_counts_identical_leading_bytes() {
        let a = vec!["tools".to_owned(), "sys".to_owned(), "m1".to_owned()];
        let b = vec![
            "tools".to_owned(),
            "sys".to_owned(),
            "m1".to_owned(),
            "m2".to_owned(),
        ];
        assert_eq!(shared_prefix(&a, &b), 10);
        let c = vec!["tools".to_owned(), "SYS".to_owned()];
        assert_eq!(shared_prefix(&a, &c), 5);
    }

    #[tokio::test]
    async fn timed_provider_records_stream_milestones_and_prefix() {
        let probe = install();
        let fake = Arc::new(FakeProvider::with_turns([
            FakeTurn {
                content: vec![
                    FakeBlock::Text("hello".into()),
                    FakeBlock::ToolUse {
                        id: None,
                        name: "read".into(),
                        input: json!({"path": "a"}),
                    },
                ],
                ..FakeTurn::default()
            },
            FakeTurn::text("done"),
        ]));
        let p = wrap(fake);
        let before = probe.snapshot().calls.len();
        let mut req = ChatRequest {
            model: "fake-model-timing-test".into(),
            messages: vec![Message::user("hi")],
            ..ChatRequest::default()
        };
        p.complete(req.clone(), CancellationToken::new())
            .await
            .unwrap();
        req.messages.push(Message::user("again"));
        p.complete(req, CancellationToken::new()).await.unwrap();
        let calls: Vec<CallTiming> = probe
            .snapshot()
            .calls
            .into_iter()
            .skip(before)
            .filter(|c| c.model == "fake-model-timing-test")
            .collect();
        assert_eq!(calls.len(), 2);
        let first = &calls[0];
        assert!(first.first_event_us.is_some());
        assert!(first.first_content_us >= first.first_event_us);
        assert_eq!(first.tool_block_stops_us.len(), 1);
        assert!(first.end_us >= first.tool_block_stops_us.first().copied());
        assert!(first.streamed_chars >= 5);
        let second = calls[1].body.clone().unwrap();
        // The second request repeats the first one's messages.
        assert!(second.prefix_shared > 0);
        assert!(second.prefix_shared < second.prefix_total);
        assert!(second.tools_unchanged && second.system_unchanged);
        assert_eq!(second.prefix_shared, second.prefix_prev_total);
    }
}
