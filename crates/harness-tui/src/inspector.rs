//! Request inspector: the exact request and response of one model call.
//!
//! [`Inspection::from_events`] rebuilds a model call from the run log:
//!
//! - **Request**: the payload recorded in `model_call_started` exactly as
//!   logged (the agent loop records it after redaction), and the full
//!   conversation the provider received, rebuilt by concatenating the
//!   `messages` of every earlier call of the same node (each call logs only
//!   its new messages) plus the latest `system` prompt.
//! - **Provider body**: for Anthropic calls, the body
//!   `harness_providers::anthropic::request::build_body` produces for the
//!   rebuilt request, which shows where the cache breakpoints sit.
//! - **Response**: the redacted assistant message from
//!   `model_call_finished`, the raw streamed text and reasoning deltas,
//!   token usage and stop reason.
//! - **Request body size**: the raw and on-the-wire (for example gzip) size
//!   of the body the provider sent, from the request-body notice in the
//!   response, shown at the top of the Request tab.
//! - **Layers**: the prompt broken into system prompt, tool list and each
//!   message, with an estimated token count (bytes / 4), the reported
//!   input tokens and the cache breakpoints.
//!
//! Everything shown or exported passes through an optional
//! [`Redactor`] again, so a secret that slipped into a log is still masked.
//! [`InspectorView`] shows the three tabs with clickable, focusable tab
//! buttons and an export button; [`Inspection::export`] writes the
//! payloads to a JSON file.

pub mod context;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind};
use harness_events::blobs::BlobStore;
use harness_events::redact::Redactor;
use harness_events::schema::{DeltaChannel, EventKind, NodeId, Payload, RunEvent, Usage};
use harness_providers::anthropic::request::{ModelProfile, RequestOptions, build_body};
use harness_providers::http::BodyStats;
use harness_providers::model::{ChatRequest, Message, ToolSpec};
use harness_providers::registry::{ModelRef, Registry, ThinkingStyle};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use serde_json::{Value, json};

use crate::chat::text::{fmt_bytes, fmt_tokens, sanitize, wrap_line, wrap_lines};
use crate::interaction::{Dispatch, ElementId, HitMap, Interaction};
use crate::theme::{Theme, Token};

/// Estimated tokens of `text` (bytes / 4, rounded up).
pub fn estimate_tokens(text: &str) -> u64 {
    (text.len() as u64).div_ceil(4)
}

/// One part of the prompt.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptLayer {
    /// Label, e.g. `system`, `tools (4)`, `#3 user`.
    pub label: String,
    /// Estimated tokens.
    pub tokens: u64,
    /// Whether a cache breakpoint ends this layer.
    pub breakpoint: bool,
    /// Whether this layer was new in this call (not sent before).
    pub new: bool,
}

/// Everything known about one model call.
#[derive(Debug, Clone, PartialEq)]
pub struct Inspection {
    /// Node that made the call.
    pub node: Option<NodeId>,
    /// Model call id.
    pub call_id: String,
    /// Provider name.
    pub provider: String,
    /// Model.
    pub model: String,
    /// The `model_call_started.request` payload as logged.
    pub recorded: Value,
    /// The full request the provider received, rebuilt from the log.
    pub request: Value,
    /// The provider wire body: from the live provider
    /// ([`Inspection::set_provider_body`]) or rebuilt here for Anthropic.
    pub provider_body: Option<Value>,
    /// The normalized request, rebuilt from the log (tool schemas
    /// included when the run logged them).
    pub chat: Option<ChatRequest>,
    /// The assistant message from `model_call_finished`.
    pub response: Option<Value>,
    /// Streamed text.
    pub streamed_text: String,
    /// Streamed reasoning.
    pub streamed_reasoning: String,
    /// Reported usage.
    pub usage: Option<Usage>,
    /// Stop reason.
    pub stop_reason: Option<String>,
    /// Prompt layers.
    pub layers: Vec<PromptLayer>,
    /// Raw and on-the-wire request body size, when the provider recorded it.
    pub wire: Option<BodyStats>,
    /// Index of the first message this call added.
    new_from: usize,
}

fn payload_value(p: &Payload, blobs: Option<&BlobStore>) -> Value {
    let p = match (p, blobs) {
        (Payload::Blob(_), Some(b)) => b.resolve(p).unwrap_or_else(|_| p.clone()),
        _ => p.clone(),
    };
    match p {
        Payload::Json(v) => v,
        Payload::Text(t) => serde_json::from_str(&t).unwrap_or(Value::String(t)),
        Payload::Blob(b) => json!({"blob": b.sha256, "size": b.size}),
    }
}

impl Inspection {
    /// Rebuilds model call `call_id` from a run's events. Blob payloads are
    /// resolved through `blobs` when given.
    pub fn from_events(
        events: &[RunEvent],
        call_id: &str,
        blobs: Option<&BlobStore>,
    ) -> Option<Inspection> {
        let start = events.iter().position(
            |e| matches!(&e.kind, EventKind::ModelCallStarted { call_id: c, .. } if c == call_id),
        )?;
        let ev = &events[start];
        let EventKind::ModelCallStarted {
            provider,
            model,
            request,
            ..
        } = &ev.kind
        else {
            return None;
        };
        let recorded = payload_value(request, blobs);
        // Rebuild the conversation from this node's earlier calls.
        let mut system: Option<String> = None;
        let mut messages: Vec<Value> = Vec::new();
        let mut specs: Vec<ToolSpec> = Vec::new();
        let mut new_from = 0;
        for e in &events[..=start] {
            if e.node_id != ev.node_id {
                continue;
            }
            // Assistant turns are logged with the call that produced them,
            // not in the next request's message delta.
            if let EventKind::ModelCallFinished {
                response: Some(r), ..
            } = &e.kind
            {
                let v = payload_value(r, blobs);
                if v.get("role").is_some() {
                    messages.push(v);
                }
                continue;
            }
            if let EventKind::ModelCallStarted { request, .. } = &e.kind {
                let v = payload_value(request, blobs);
                if let Some(s) = v.get("system").and_then(Value::as_str) {
                    system = Some(s.to_owned());
                }
                // Full tool definitions are logged when they change.
                if let Some(full) = v
                    .get("tools")
                    .and_then(Value::as_array)
                    .filter(|a| a.iter().all(Value::is_object))
                    .filter(|a| !a.is_empty())
                    .and_then(|a| {
                        a.iter()
                            .map(|t| serde_json::from_value::<ToolSpec>(t.clone()).ok())
                            .collect::<Option<Vec<_>>>()
                    })
                {
                    specs = full;
                }
                let from = v
                    .get("messages_from")
                    .and_then(Value::as_u64)
                    .unwrap_or(messages.len() as u64) as usize;
                messages.truncate(from);
                new_from = messages.len();
                if let Some(m) = v.get("messages").and_then(Value::as_array) {
                    messages.extend(m.iter().cloned());
                }
            }
        }
        let tools: Vec<String> = recorded
            .get("tools")
            .and_then(Value::as_array)
            .map(|v| {
                v.iter()
                    .filter_map(|t| {
                        t.as_str()
                            .or_else(|| t.get("name").and_then(Value::as_str))
                            .map(str::to_owned)
                    })
                    .collect()
            })
            .unwrap_or_default();
        // This call's tools, with their full definitions when known.
        let tool_specs: Vec<ToolSpec> = tools
            .iter()
            .map(|n| {
                specs
                    .iter()
                    .find(|s| &s.name == n)
                    .cloned()
                    .unwrap_or_else(|| ToolSpec {
                        name: n.clone(),
                        description: String::new(),
                        input_schema: json!({"type": "object"}),
                    })
            })
            .collect();
        let wire_model = recorded
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(model)
            .to_owned();
        let mut request = json!({
            "provider": provider,
            "model": wire_model,
            "system": system,
            "tools": tools,
            "tool_choice": recorded.get("tool_choice"),
            "max_tokens": recorded.get("max_tokens"),
            "thinking": recorded.get("thinking"),
            "messages": messages,
        });
        if request["system"].is_null() {
            request.as_object_mut().map(|o| o.remove("system"));
        }
        let chat = chat_request(
            &wire_model,
            system.as_deref(),
            &messages,
            &tool_specs,
            &recorded,
        );
        let provider_body = (provider == "anthropic")
            .then(|| chat.as_ref().and_then(anthropic_body))
            .flatten();
        // Response and stream.
        let mut response = None;
        let mut usage = None;
        let mut stop_reason = None;
        let mut text = String::new();
        let mut reasoning = String::new();
        for e in &events[start..] {
            match &e.kind {
                EventKind::Delta {
                    call_id: c,
                    channel,
                    text: t,
                } if c == call_id => match channel {
                    DeltaChannel::Text => text.push_str(t),
                    DeltaChannel::Reasoning => reasoning.push_str(t),
                    DeltaChannel::ToolArgs => {}
                },
                EventKind::ModelCallFinished {
                    call_id: c,
                    usage: u,
                    stop_reason: s,
                    response: r,
                    ..
                } if c == call_id => {
                    usage = Some(*u);
                    stop_reason.clone_from(s);
                    response = r.as_ref().map(|p| payload_value(p, blobs));
                    break;
                }
                _ => {}
            }
        }
        let layers = layers(
            system.as_deref(),
            &tools,
            &messages,
            new_from,
            provider_body.as_ref(),
        );
        let wire = response
            .as_ref()
            .and_then(|r| serde_json::from_value::<Message>(r.clone()).ok())
            .and_then(|m| harness_providers::openai::notice::request_body(&m));
        Some(Inspection {
            node: ev.node_id.clone(),
            call_id: call_id.to_owned(),
            provider: provider.clone(),
            model: model.clone(),
            recorded,
            request,
            provider_body,
            chat,
            response,
            streamed_text: text,
            streamed_reasoning: reasoning,
            usage,
            stop_reason,
            wire,
            layers,
            new_from,
        })
    }

    /// Replaces the wire body (e.g. rendered by the live provider from
    /// [`Inspection::chat`]) and updates the cache breakpoints of the layers.
    pub fn set_provider_body(&mut self, body: Option<Value>) {
        let names: Vec<String> = self
            .chat
            .as_ref()
            .map(|c| c.tools.iter().map(|t| t.name.clone()).collect())
            .unwrap_or_default();
        let messages: Vec<Value> = self
            .request
            .get("messages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let new_from = self.new_from;
        self.provider_body = body;
        self.layers = layers(
            self.chat.as_ref().and_then(|c| c.system.as_deref()),
            &names,
            &messages,
            new_from,
            self.provider_body.as_ref(),
        );
    }

    /// Masks secrets in every payload with `redactor`.
    pub fn redact(&mut self, redactor: &Redactor) {
        redactor.redact_json(&mut self.recorded);
        redactor.redact_json(&mut self.request);
        if let Some(b) = &mut self.provider_body {
            redactor.redact_json(b);
        }
        if let Some(r) = &mut self.response {
            redactor.redact_json(r);
        }
        self.streamed_text = redactor.redact(&self.streamed_text).into_owned();
        self.streamed_reasoning = redactor.redact(&self.streamed_reasoning).into_owned();
    }

    /// Everything as one JSON document (what [`Inspection::export`] writes).
    pub fn to_json(&self) -> Value {
        json!({
            "call_id": self.call_id,
            "node": self.node.as_ref().map(NodeId::as_str),
            "provider": self.provider,
            "model": self.model,
            "recorded_request": self.recorded,
            "request": self.request,
            "provider_body": self.provider_body,
            "response": self.response,
            "stream": {"text": self.streamed_text, "reasoning": self.streamed_reasoning},
            "usage": self.usage,
            "stop_reason": self.stop_reason,
            "request_body": self.wire.as_ref().map(|w| json!({
                "raw_bytes": w.raw_bytes, "wire_bytes": w.wire_bytes, "encoding": w.encoding,
            })),
            "layers": self.layers.iter().map(|l| json!({
                "label": l.label, "tokens": l.tokens, "breakpoint": l.breakpoint, "new": l.new,
            })).collect::<Vec<_>>(),
        })
    }

    /// Writes the inspection to `<dir>/request-<call_id>.json` and returns
    /// the path.
    pub fn export(&self, dir: &Path) -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(dir)?;
        let safe: String = self
            .call_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let path = dir.join(format!("request-{safe}.json"));
        let text = serde_json::to_string_pretty(&self.to_json()).map_err(std::io::Error::other)?;
        std::fs::write(&path, text)?;
        Ok(path)
    }
}

/// `request body 280.1 KB raw · 46.2 KB on the wire (gzip, 84% smaller)`.
fn wire_line(t: &Theme, w: &BodyStats) -> Line<'static> {
    let wire = match &w.encoding {
        Some(enc) => {
            let saved = 100u64.saturating_sub(
                (w.wire_bytes * 100 + w.raw_bytes / 2)
                    .checked_div(w.raw_bytes)
                    .unwrap_or(100),
            );
            format!(
                "{} on the wire ({}, {saved}% smaller)",
                fmt_bytes(w.wire_bytes),
                sanitize(enc)
            )
        }
        None => format!("{} on the wire (uncompressed)", fmt_bytes(w.wire_bytes)),
    };
    Line::from(vec![
        Span::styled("request body ", t.fg(Token::Muted)),
        Span::styled(
            format!("{} raw", fmt_bytes(w.raw_bytes)),
            t.fg(Token::Number),
        ),
        Span::styled(" · ", t.fg(Token::Muted)),
        Span::styled(wire, t.fg(Token::Number)),
    ])
}

/// The normalized request a call sent, rebuilt from the log.
fn chat_request(
    model: &str,
    system: Option<&str>,
    messages: &[Value],
    tools: &[ToolSpec],
    recorded: &Value,
) -> Option<ChatRequest> {
    let messages: Vec<Message> = messages
        .iter()
        .map(|m| serde_json::from_value(m.clone()))
        .collect::<Result<_, _>>()
        .ok()?;
    let field = |k: &str| recorded.get(k).cloned().filter(|v| !v.is_null());
    let mut req = ChatRequest::new(model);
    req.system = system.map(str::to_owned);
    req.messages = messages;
    req.tools = tools.to_vec();
    if let Some(v) = field("tool_choice").and_then(|v| serde_json::from_value(v).ok()) {
        req.tool_choice = v;
    }
    req.max_tokens = field("max_tokens").and_then(|v| serde_json::from_value(v).ok());
    req.temperature = field("temperature").and_then(|v| serde_json::from_value(v).ok());
    req.thinking = field("thinking").and_then(|v| serde_json::from_value(v).ok());
    if let Some(v) = field("stop_sequences").and_then(|v| serde_json::from_value(v).ok()) {
        req.stop_sequences = v;
    }
    Some(req)
}

/// The Anthropic Messages body for `req` (built-in model profiles).
fn anthropic_body(req: &ChatRequest) -> Option<Value> {
    let reg = Registry::shared();
    let info = reg.lookup(&ModelRef::parse(&format!("anthropic/{}", req.model)).ok()?);
    let profile = ModelProfile {
        capabilities: reg.capabilities("anthropic", &req.model),
        thinking: info.as_ref().map_or(ThinkingStyle::None, |i| i.thinking),
        efforts: info.map(|i| i.efforts).unwrap_or_default(),
    };
    build_body(req, &profile, &RequestOptions::default()).ok()
}

fn has_cache_control(v: &Value) -> bool {
    match v {
        Value::Object(o) => o.contains_key("cache_control") || o.values().any(has_cache_control),
        Value::Array(a) => a.iter().any(has_cache_control),
        _ => false,
    }
}

fn layers(
    system: Option<&str>,
    tools: &[String],
    messages: &[Value],
    new_from: usize,
    body: Option<&Value>,
) -> Vec<PromptLayer> {
    let mut out = Vec::new();
    let bp_tools = body.is_some_and(|b| has_cache_control(&b["tools"]));
    let bp_system = body.is_some_and(|b| has_cache_control(&b["system"]));
    let body_msgs: Vec<Value> = body
        .and_then(|b| b["messages"].as_array().cloned())
        .unwrap_or_default();
    if !tools.is_empty() {
        out.push(PromptLayer {
            label: format!("tools ({}): {}", tools.len(), tools.join(", ")),
            tokens: estimate_tokens(&tools.join(",")),
            breakpoint: bp_tools,
            new: false,
        });
    }
    if let Some(s) = system {
        out.push(PromptLayer {
            label: "system prompt".into(),
            tokens: estimate_tokens(s),
            breakpoint: bp_system,
            new: false,
        });
    }
    for (i, m) in messages.iter().enumerate() {
        let role = m["role"].as_str().unwrap_or("?");
        let kinds: Vec<&str> = m["content"]
            .as_array()
            .map(|c| c.iter().filter_map(|b| b["type"].as_str()).collect())
            .unwrap_or_default();
        let text = m.to_string();
        out.push(PromptLayer {
            label: format!("#{} {role} [{}]", i + 1, kinds.join(", ")),
            tokens: estimate_tokens(&text),
            breakpoint: body_msgs.get(i).is_some_and(has_cache_control),
            new: i >= new_from,
        });
    }
    out
}

/// Inspector tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    /// Request payloads.
    Request,
    /// Response and stream.
    Response,
    /// Prompt layers and cache breakpoints.
    Layers,
}

impl Tab {
    const ALL: [Tab; 3] = [Tab::Request, Tab::Response, Tab::Layers];

    fn label(self) -> &'static str {
        match self {
            Tab::Request => "Request",
            Tab::Response => "Response",
            Tab::Layers => "Layers",
        }
    }
}

/// Inspector actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InspectorAction {
    /// Show a tab.
    Show(Tab),
    /// Export the payloads.
    Export,
}

/// What the inspector asks the app to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InspectorOutput {
    /// Exported to this file.
    Exported(PathBuf),
    /// Export failed.
    ExportFailed(String),
    /// Esc.
    Back,
}

/// Element id of a tab button.
pub fn tab_id(tab: Tab) -> String {
    format!("inspector.tab.{}", tab.label().to_lowercase())
}

/// Element id of the export button.
pub const EXPORT_ID: &str = "inspector.export";

/// The request inspector view.
pub struct InspectorView {
    data: Inspection,
    theme: Theme,
    tab: Tab,
    scroll: usize,
    height: usize,
    export_dir: PathBuf,
    interaction: Interaction<InspectorAction>,
}

impl InspectorView {
    /// Shows `data`; exports go to `export_dir`. With a redactor, every
    /// payload is redacted again first.
    pub fn new(
        mut data: Inspection,
        theme: Theme,
        export_dir: PathBuf,
        redactor: Option<Arc<Redactor>>,
    ) -> Self {
        if let Some(r) = redactor {
            data.redact(&r);
        }
        InspectorView {
            data,
            theme,
            tab: Tab::Request,
            scroll: 0,
            height: 0,
            export_dir,
            interaction: Interaction::new(),
        }
    }

    /// The inspected call.
    pub fn data(&self) -> &Inspection {
        &self.data
    }

    /// The shown tab.
    pub fn tab(&self) -> Tab {
        self.tab
    }

    fn activate(&mut self, a: InspectorAction) -> Option<InspectorOutput> {
        match a {
            InspectorAction::Show(t) => {
                self.tab = t;
                self.scroll = 0;
                None
            }
            InspectorAction::Export => Some(match self.data.export(&self.export_dir) {
                Ok(p) => InspectorOutput::Exported(p),
                Err(e) => InspectorOutput::ExportFailed(e.to_string()),
            }),
        }
    }

    /// Handles input: Tab/arrows move focus over the tab buttons and the
    /// export button, Enter activates, `1`-`3` pick a tab, `e` exports,
    /// PgUp/PgDn and the wheel scroll, Esc backs out.
    pub fn handle(&mut self, ev: &Event) -> Option<InspectorOutput> {
        match ev {
            Event::Key(k) => self.handle_key(k),
            Event::Mouse(m) => match m.kind {
                MouseEventKind::ScrollUp => {
                    self.scroll = self.scroll.saturating_sub(3);
                    None
                }
                MouseEventKind::ScrollDown => {
                    self.scroll += 3;
                    None
                }
                _ => match self.interaction.handle_mouse(m)? {
                    Dispatch::Activate(a) => self.activate(a),
                    Dispatch::Back => Some(InspectorOutput::Back),
                    _ => None,
                },
            },
            _ => None,
        }
    }

    fn handle_key(&mut self, k: &KeyEvent) -> Option<InspectorOutput> {
        if k.kind == KeyEventKind::Release
            || !k.modifiers.difference(KeyModifiers::SHIFT).is_empty()
        {
            return None;
        }
        match k.code {
            KeyCode::Char('1') => return self.activate(InspectorAction::Show(Tab::Request)),
            KeyCode::Char('2') => return self.activate(InspectorAction::Show(Tab::Response)),
            KeyCode::Char('3') => return self.activate(InspectorAction::Show(Tab::Layers)),
            KeyCode::Char('e') => return self.activate(InspectorAction::Export),
            KeyCode::PageDown => {
                self.scroll += self.height.max(1);
                return None;
            }
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_sub(self.height.max(1));
                return None;
            }
            _ => {}
        }
        match self.interaction.handle_key(k)? {
            Dispatch::Activate(a) => self.activate(a),
            Dispatch::Back => Some(InspectorOutput::Back),
            _ => None,
        }
    }

    fn json_lines(&self, title: &str, v: &Value, out: &mut Vec<Line<'static>>) {
        let t = &self.theme;
        out.push(Line::from(Span::styled(
            title.to_owned(),
            t.fg(Token::Accent).add_modifier(Modifier::BOLD),
        )));
        let text = serde_json::to_string_pretty(v).unwrap_or_default();
        for l in text.lines() {
            out.push(Line::from(Span::styled(sanitize(l), t.fg(Token::Fg))));
        }
        out.push(Line::default());
    }

    fn body(&self) -> Vec<Line<'static>> {
        let t = &self.theme;
        let d = &self.data;
        let mut out = Vec::new();
        match self.tab {
            Tab::Request => {
                if let Some(w) = &d.wire {
                    out.push(wire_line(t, w));
                    out.push(Line::default());
                }
                self.json_lines("recorded request (as logged)", &d.recorded, &mut out);
                self.json_lines("full request (rebuilt from the log)", &d.request, &mut out);
                if let Some(b) = &d.provider_body {
                    self.json_lines(&format!("{} request body", d.provider), b, &mut out);
                }
            }
            Tab::Response => {
                if let Some(u) = d.usage {
                    out.push(Line::from(vec![
                        Span::styled("usage ", t.fg(Token::Muted)),
                        Span::styled(
                            format!(
                                "in {} cached {} out {} reasoning {}",
                                fmt_tokens(u.input),
                                fmt_tokens(u.cached_input),
                                fmt_tokens(u.output),
                                fmt_tokens(u.reasoning)
                            ),
                            t.fg(Token::Number),
                        ),
                        Span::styled(
                            format!("  stop {}", d.stop_reason.as_deref().unwrap_or("?")),
                            t.fg(Token::Muted),
                        ),
                    ]));
                    out.push(Line::default());
                }
                if !d.streamed_reasoning.is_empty() {
                    out.push(Line::from(Span::styled(
                        "streamed reasoning",
                        t.fg(Token::Accent),
                    )));
                    for l in d.streamed_reasoning.lines() {
                        out.push(Line::from(Span::styled(
                            sanitize(l),
                            t.fg(Token::Reasoning),
                        )));
                    }
                    out.push(Line::default());
                }
                out.push(Line::from(Span::styled(
                    "streamed text",
                    t.fg(Token::Accent),
                )));
                for l in d.streamed_text.lines() {
                    out.push(Line::from(Span::styled(sanitize(l), t.fg(Token::Fg))));
                }
                out.push(Line::default());
                match &d.response {
                    Some(r) => self.json_lines("raw response message", r, &mut out),
                    None => out.push(Line::from(Span::styled(
                        "no response recorded",
                        t.fg(Token::Muted),
                    ))),
                }
            }
            Tab::Layers => {
                let est: u64 = d.layers.iter().map(|l| l.tokens).sum();
                let mut head = vec![Span::styled(
                    format!("≈{} tokens estimated", fmt_tokens(est)),
                    t.fg(Token::Number),
                )];
                if let Some(u) = d.usage {
                    head.push(Span::styled(
                        format!(
                            " · reported input {} (cached {})",
                            fmt_tokens(u.input + u.cached_input),
                            fmt_tokens(u.cached_input)
                        ),
                        t.fg(Token::Muted),
                    ));
                }
                out.push(Line::from(head));
                if d.provider_body.is_none() {
                    out.push(Line::from(Span::styled(
                        format!(
                            "cache breakpoints: not placed by harness for provider `{}`",
                            d.provider
                        ),
                        t.fg(Token::Muted),
                    )));
                }
                for l in &d.layers {
                    let mut spans = vec![
                        Span::styled(format!("{:>8} ", fmt_tokens(l.tokens)), t.fg(Token::Number)),
                        Span::styled(sanitize(&l.label), t.fg(Token::Fg)),
                    ];
                    if l.new {
                        spans.push(Span::styled("  new", t.fg(Token::Running)));
                    }
                    if l.breakpoint {
                        spans.push(Span::styled("  ◆ cache breakpoint", t.fg(Token::Ok)));
                    }
                    out.push(Line::from(spans));
                }
            }
        }
        out
    }

    /// Renders the inspector into `area`. The title and the buttons flow
    /// onto as many rows as they need; the body wraps and scrolls.
    pub fn render(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let t = self.theme.clone();
        let mut hits: HitMap<InspectorAction> = self.interaction.begin_frame();
        let aw = usize::from(area.width).max(1);
        let title = sanitize(&format!(
            " {} · {} · {} ",
            self.data.call_id, self.data.provider, self.data.model
        ));
        // Lay out the bar: the title (wrapped), then the buttons, each on
        // the current row when it fits, else on the next.
        let mut rows: Vec<Line<'static>> =
            wrap_line(Line::from(Span::styled(title, t.fg(Token::Agent))), aw, &[]);
        let mut buttons: Vec<(String, String, InspectorAction, Style)> = Vec::new();
        for tab in Tab::ALL {
            let id = tab_id(tab);
            let style = if hits.is_focused(&ElementId::from(id.as_str())) {
                t.focused()
            } else if tab == self.tab {
                t.fg(Token::Accent).add_modifier(Modifier::BOLD)
            } else {
                t.fg(Token::Muted)
            };
            buttons.push((
                id,
                format!("[{}]", tab.label()),
                InspectorAction::Show(tab),
                style,
            ));
        }
        let style = if hits.is_focused(&ElementId::from(EXPORT_ID)) {
            t.focused()
        } else {
            t.fg(Token::Warn)
        };
        buttons.push((
            EXPORT_ID.into(),
            "[export]".into(),
            InspectorAction::Export,
            style,
        ));
        let mut placed: Vec<(String, Rect, InspectorAction)> = Vec::new();
        for (id, label, action, style) in buttons {
            let w = crate::chat::text::width(&label);
            let row = rows.last_mut().expect("a row");
            let mut used = crate::chat::text::line_width(row);
            let spaced = row.spans.last().is_none_or(|s| s.content.ends_with(' '));
            let mut sep = usize::from(used > 0 && !spaced);
            if used > 0 && used + sep + w > aw {
                rows.push(Line::default());
                used = 0;
                sep = 0;
            }
            let row = rows.last_mut().expect("a row");
            if sep > 0 {
                row.spans.push(Span::raw(" "));
                used += 1;
            }
            let y = (rows.len() - 1) as u16;
            placed.push((
                id,
                Rect::new(
                    area.x + used as u16,
                    area.y + y,
                    (w as u16).min(area.width.saturating_sub(used as u16)),
                    1,
                ),
                action,
            ));
            rows.last_mut()
                .expect("a row")
                .spans
                .push(Span::styled(label, style));
        }
        let bar_h = (rows.len() as u16).min((area.height / 2).max(1));
        let [bar, body] =
            Layout::vertical([Constraint::Length(bar_h), Constraint::Min(1)]).areas(area);
        frame.render_widget(Paragraph::new(rows), bar);
        for (id, rect, action) in placed {
            if rect.y < bar.bottom() && rect.width > 0 {
                hits.register_button(id, rect, action);
            }
        }
        hits.register_pane("inspector", body);
        self.height = usize::from(body.height);
        let lines = wrap_lines(self.body(), usize::from(body.width).max(1), &[]);
        self.scroll = self.scroll.min(lines.len().saturating_sub(self.height));
        let first = self.scroll;
        crate::selection::update("inspector", |r| {
            r.set_scroll(first);
            r.set_joins(
                lines
                    .iter()
                    .skip(first)
                    .take(usize::from(body.height))
                    .map(crate::chat::text::soft_join),
            );
        });
        let shown: Vec<Line<'static>> = lines
            .into_iter()
            .skip(self.scroll)
            .take(self.height)
            .collect();
        frame.render_widget(Paragraph::new(shown), body);
        self.interaction.end_frame(hits);
    }
}

#[cfg(test)]
mod tests;
