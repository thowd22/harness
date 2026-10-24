//! `harness bench`: latency benchmark of a scripted multi-turn coding task.
//!
//! The binary drives the session (fake provider or a real model, see
//! `docs/performance.md`) with the timing probe of
//! [`harness_providers::timing`] installed, then hands the probe data and
//! the run's events to [`analyze`]. This module turns them into a
//! [`RunReport`]: per model call (time to first byte and token, output
//! tokens/s, call time, cached and uncached input, request sizes, prefix
//! stability) and per gap between calls (tool dispatch, verify, request
//! hooks, the rest of the harness overhead), plus per turn and per tool
//! totals. [`summarize`] folds several runs into percentiles and
//! [`render_table`] prints the human-readable report; the JSON is the
//! [`BenchReport`] itself.
//!
//! [`scenario`] holds the bundled task: a small Python library with a bug,
//! its tests, two user prompts and the fake replies.

pub mod scenario;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Instant;

use harness_events::redact::Redactor;
use harness_events::schema::{EventKind, RunEvent};
use harness_events::store::{EventDraft, RecorderOptions, RunRecorder};
use harness_providers::timing::{BodySizes, CallTiming, ProbeData, Span};
use serde::{Deserialize, Serialize};

/// Version of the bench JSON format.
pub const BENCH_SCHEMA: u32 = 1;

/// Span name prefix the driver uses for user turns (`turn:1`, ...).
pub const TURN_SPAN: &str = "turn:";

/// Span the driver records around session start (config, catalog,
/// provider, tools, prompt).
pub const START_SPAN: &str = "session_start";

/// Span of a connection pre-warm (`[experimental] prewarm_connection`).
pub const PREWARM_SPAN: &str = "prewarm";

/// Hook span prefix (see `harness_agent::timing`).
pub const HOOK_SPAN: &str = "hook:";

/// The context hook (elision and compaction); model calls inside it are
/// compaction calls.
pub const CONTEXT_HOOK: &str = "hook:context";

fn ms(us: u64) -> f64 {
    us as f64 / 1000.0
}

fn span_ms(a: u64, b: u64) -> f64 {
    ms(b.saturating_sub(a))
}

/// One model call.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CallRow {
    /// Call number in the run (0-based).
    pub index: usize,
    /// User turn (1-based; 0 = outside any turn).
    pub turn: usize,
    /// `agent` or `compaction`.
    pub kind: String,
    /// Model id.
    pub model: String,
    /// Time to first stream event (response headers + first SSE event).
    pub ttfb_ms: Option<f64>,
    /// Time to first text, reasoning or tool-use event.
    pub ttft_ms: Option<f64>,
    /// Whole call (stream entered to message stop).
    pub total_ms: Option<f64>,
    /// Output tokens per second after the first token.
    pub output_tps: Option<f64>,
    /// Output tokens.
    pub output_tokens: u64,
    /// Uncached input tokens.
    pub input_tokens: u64,
    /// Input tokens read from the cache.
    pub cache_read_tokens: u64,
    /// Input tokens written to the cache.
    pub cache_write_tokens: u64,
    /// Content deltas received.
    pub deltas: u64,
    /// Time from the first complete tool-use block to message stop: how
    /// much earlier read-only tools could start if they ran at
    /// `block_stop` (streaming tool execution).
    pub tool_early_start_ms: Option<f64>,
    /// Request body sizes.
    pub body: Option<BodySizes>,
    /// Estimated cost of building and serializing the body.
    pub build_ms: f64,
    /// The call failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The harness's time between the end of one model call (or the start of a
/// turn) and the next request.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GapRow {
    /// Turn (1-based).
    pub turn: usize,
    /// The call that follows the gap.
    pub before_call: usize,
    /// Whether the gap starts at the turn start (not after a call).
    pub turn_start: bool,
    /// Whole gap.
    pub gap_ms: f64,
    /// Tool dispatch: first `tool_call` to last `tool_result`.
    pub tools_ms: f64,
    /// Sum of the tools' own durations (`> tools_ms` means they overlapped).
    pub tool_sum_ms: f64,
    /// Tool calls in the gap.
    pub tool_calls: u32,
    /// Verify commands.
    pub verify_ms: f64,
    /// Request hooks (compaction calls included).
    pub hooks_ms: f64,
    /// Time the context hook spent inside compaction calls in this gap:
    /// the blocking part of compaction. A background summary
    /// (`[context] background_compaction`) counts only while the hook waits
    /// for it.
    pub compaction_ms: f64,
    /// The rest: `gap - tools - verify - hooks`. Pure harness overhead:
    /// response finishing, event recording, redaction, history updates,
    /// request build.
    pub overhead_ms: f64,
    /// Part of the overhead: previous stream end to `model_call_finished`.
    pub finish_ms: Option<f64>,
    /// Part of the overhead: last hook (or tool result) to
    /// `model_call_started` (request build and logging).
    pub build_ms: Option<f64>,
    /// Part of the overhead: `model_call_started` to the provider call.
    pub send_ms: Option<f64>,
}

/// One user turn.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TurnRow {
    /// Turn number (1-based).
    pub turn: usize,
    /// Wall time of the turn.
    pub wall_ms: f64,
    /// Model calls (agent + compaction).
    pub calls: u32,
    /// Time inside model calls.
    pub model_ms: f64,
    /// Tool dispatch.
    pub tools_ms: f64,
    /// Verify commands.
    pub verify_ms: f64,
    /// Request hooks outside model calls.
    pub hooks_ms: f64,
    /// Harness overhead: everything else.
    pub overhead_ms: f64,
    /// Turn start to the first request.
    pub start_ms: f64,
    /// Last call end to turn end (verify, session save).
    pub end_ms: f64,
}

/// Totals for one tool.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolRow {
    /// Tool name.
    pub tool: String,
    /// Calls.
    pub calls: u32,
    /// Sum of durations.
    pub total_ms: f64,
    /// Longest call.
    pub max_ms: f64,
}

/// Totals for one request hook.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HookRow {
    /// Hook name.
    pub hook: String,
    /// Invocations.
    pub calls: u32,
    /// Sum of durations.
    pub total_ms: f64,
    /// Longest invocation.
    pub max_ms: f64,
}

/// One benchmark run (one session).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunReport {
    /// Run label (`A1`, `fake-3`, ...).
    pub label: String,
    /// Wall time of all turns.
    pub wall_ms: f64,
    /// Model calls.
    pub calls: Vec<CallRow>,
    /// Gaps before each agent call.
    pub gaps: Vec<GapRow>,
    /// User turns.
    pub turns: Vec<TurnRow>,
    /// Tools.
    pub tools: Vec<ToolRow>,
    /// Request hooks.
    pub hooks: Vec<HookRow>,
    /// Cost reported by the run (USD).
    pub cost_usd: f64,
    /// Events recorded.
    pub events: usize,
    /// Session start (before the first turn).
    pub session_start_ms: f64,
    /// Connection pre-warm, when it ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prewarm_ms: Option<f64>,
    /// Whether the pre-warm finished before the first model call started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prewarm_before_first_call: Option<bool>,
}

/// Percentiles of one metric over several values.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Stat {
    /// Values.
    pub n: usize,
    /// Median.
    pub p50: f64,
    /// 90th percentile.
    pub p90: f64,
    /// Smallest.
    pub min: f64,
    /// Largest.
    pub max: f64,
    /// Mean.
    pub mean: f64,
}

impl Stat {
    /// Percentiles of `values` (`None` when empty).
    pub fn of(values: &[f64]) -> Option<Stat> {
        if values.is_empty() {
            return None;
        }
        let mut v = values.to_vec();
        v.sort_by(f64::total_cmp);
        let pick = |q: f64| v[((v.len() - 1) as f64 * q).round() as usize];
        Some(Stat {
            n: v.len(),
            p50: pick(0.5),
            p90: pick(0.9),
            min: v[0],
            max: v[v.len() - 1],
            mean: v.iter().sum::<f64>() / v.len() as f64,
        })
    }
}

/// Cost of recording events, by type (replayed into a fresh run log).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EmitCost {
    /// Event type.
    pub event: String,
    /// Events.
    pub count: u32,
    /// Total time in `RunRecorder::emit`.
    pub total_us: u64,
    /// Longest emit.
    pub max_us: u64,
}

/// TUI render cost: the run's events replayed into the app, one draw per
/// dirty frame on a test backend.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RenderStats {
    /// Terminal size.
    pub size: (u16, u16),
    /// Events applied.
    pub events: u64,
    /// Frames drawn.
    pub frames: u64,
    /// Per-frame draw time in ms.
    pub frame_ms: Option<Stat>,
    /// Per-event `handle` time in ms.
    pub handle_ms: Option<Stat>,
}

/// Connection setup measured with plain GETs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NetProbe {
    /// URL requested.
    pub url: String,
    /// HTTP version negotiated.
    pub version: String,
    /// Time to response headers on a new client (DNS + TCP + TLS + request).
    pub cold_ms: Vec<f64>,
    /// Time to response headers on the same client afterwards (reused
    /// connection).
    pub warm_ms: Vec<f64>,
    /// Time to response headers on a newly built provider client after the
    /// warm requests: what a subagent, session summary or dreaming
    /// provider pays on its first call (warm when clients share the
    /// connection pool, cold otherwise).
    #[serde(default)]
    pub new_client_ms: Vec<f64>,
}

/// One tool definition's size.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolSchema {
    /// Tool name.
    pub name: String,
    /// JSON bytes of the definition (name, description, schema).
    pub bytes: u64,
    /// Estimated tokens (bytes / 4).
    pub tokens_est: u64,
}

/// Across-run summary.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    /// Over every agent call of every run.
    pub ttfb_ms: Option<Stat>,
    /// Over every agent call.
    pub ttft_ms: Option<Stat>,
    /// Over every agent call.
    pub call_ms: Option<Stat>,
    /// Over every agent call with output.
    pub output_tps: Option<Stat>,
    /// Harness overhead per gap.
    pub overhead_ms: Option<Stat>,
    /// Gap before the first request of a turn.
    pub turn_start_ms: Option<Stat>,
    /// Tool dispatch per gap with tools.
    pub tools_ms: Option<Stat>,
    /// Request hooks per gap.
    pub hooks_ms: Option<Stat>,
    /// Wall time per run.
    pub run_wall_ms: Option<Stat>,
    /// Session start per run.
    pub session_start_ms: Option<Stat>,
    /// First agent call of each run (cold connection and cache).
    pub first_call_ttfb_ms: Option<Stat>,
    /// Later agent calls (warm connection and cache).
    pub later_call_ttfb_ms: Option<Stat>,
    /// Harness overhead per run (sum of gaps' overhead + turn start/end).
    pub run_overhead_ms: Option<Stat>,
    /// Median harness overhead per model call, per run (the regression
    /// guard number).
    pub overhead_per_call_ms: Option<Stat>,
    /// Share of the previous request found byte-identical at the start of
    /// the next one (100 = the whole previous request is a prefix).
    pub prefix_shared_pct: Option<Stat>,
    /// Agent requests whose prefix broke: the previous request (same
    /// model) is not an exact prefix, so part of the cache is lost.
    pub prefix_breaks: u32,
    /// Cache-read share of input tokens per call (calls after the first).
    pub cache_hit_pct: Option<Stat>,
    /// Possible early start of tools per call with tool calls.
    pub tool_early_start_ms: Option<Stat>,
    /// Compaction summary calls, whole call time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_call_ms: Option<Stat>,
    /// Time turns waited for compaction, per run (sum of the gaps'
    /// `compaction_ms`; runs with a compaction only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_blocking_ms: Option<Stat>,
    /// Total cost of all runs.
    pub cost_usd: f64,
}

/// Machine and build the bench ran on.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Machine {
    /// `std::env::consts::OS`/`ARCH`.
    pub os: String,
    /// CPUs available.
    pub cpus: usize,
    /// Harness version.
    pub harness_version: String,
    /// `debug` or `release`.
    pub profile: String,
}

impl Machine {
    /// This machine.
    pub fn detect(harness_version: &str) -> Machine {
        Machine {
            os: format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
            cpus: std::thread::available_parallelism().map_or(1, |n| n.get()),
            harness_version: harness_version.to_owned(),
            profile: if cfg!(debug_assertions) {
                "debug".into()
            } else {
                "release".into()
            },
        }
    }
}

/// The whole benchmark result.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BenchReport {
    /// [`BENCH_SCHEMA`].
    pub schema: u32,
    /// `fake` or `live`.
    pub mode: String,
    /// Model reference.
    pub model: String,
    /// Free-form configuration label (`baseline`, `session_id`, ...).
    pub config: String,
    /// Machine.
    pub machine: Machine,
    /// Runs.
    pub runs: Vec<RunReport>,
    /// Across runs.
    pub summary: Summary,
    /// Tool definitions sent with every request.
    pub tool_schemas: Vec<ToolSchema>,
    /// Event recording cost (last run).
    pub emit: Vec<EmitCost>,
    /// TUI render cost (last run).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub render: Option<RenderStats>,
    /// Connection setup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net: Option<NetProbe>,
}

fn event_us(e: &RunEvent, epoch_unix_us: u64) -> u64 {
    let unix_us = (e.ts.unix_timestamp_nanos() / 1000).max(0) as u64;
    unix_us.saturating_sub(epoch_unix_us)
}

fn inside(t: u64, a: u64, b: u64) -> bool {
    t >= a && t <= b
}

/// Builds the report of one run from the probe data and the run's events
/// (both of this run only; see [`harness_providers::timing::Probe::clear`]).
pub fn analyze(label: &str, probe: &ProbeData, events: &[RunEvent]) -> RunReport {
    let epoch = probe.epoch_unix_us;
    let timed: Vec<(u64, &RunEvent)> = events.iter().map(|e| (event_us(e, epoch), e)).collect();
    let turns: Vec<&Span> = probe
        .spans
        .iter()
        .filter(|s| s.name.starts_with(TURN_SPAN))
        .collect();
    let hooks: Vec<&Span> = probe
        .spans
        .iter()
        .filter(|s| s.name.starts_with(HOOK_SPAN))
        .collect();
    let turn_of = |t: u64| {
        turns
            .iter()
            .position(|s| inside(t, s.start_us, s.end_us))
            .map_or(0, |i| i + 1)
    };
    let is_compaction = |c: &CallTiming| {
        hooks
            .iter()
            .any(|h| h.name == CONTEXT_HOOK && inside(c.start_us, h.start_us, h.end_us))
    };

    let prewarm = probe.spans.iter().find(|s| s.name == PREWARM_SPAN);
    let first_call = probe.calls.first().map(|c| c.start_us);
    let mut report = RunReport {
        session_start_ms: probe
            .spans
            .iter()
            .find(|s| s.name == START_SPAN)
            .map_or(0.0, |s| span_ms(s.start_us, s.end_us)),
        prewarm_ms: prewarm.map(|s| span_ms(s.start_us, s.end_us)),
        prewarm_before_first_call: prewarm.zip(first_call).map(|(s, c)| s.end_us <= c),
        label: label.to_owned(),
        wall_ms: turns.iter().map(|s| span_ms(s.start_us, s.end_us)).sum(),
        events: events.len(),
        ..RunReport::default()
    };

    for c in &probe.calls {
        let usage = c.usage.unwrap_or_default();
        let ttfb = c.first_event_us.map(|t| span_ms(c.start_us, t));
        let ttft = c.first_content_us.map(|t| span_ms(c.start_us, t));
        let total = c.end_us.map(|t| span_ms(c.start_us, t));
        let output_tps = match (c.first_content_us, c.end_us) {
            (Some(a), Some(b)) if b > a && usage.output_tokens > 0 => {
                Some(usage.output_tokens as f64 / ((b - a) as f64 / 1e6))
            }
            _ => None,
        };
        report.calls.push(CallRow {
            index: c.index,
            turn: turn_of(c.start_us),
            kind: if is_compaction(c) {
                "compaction".into()
            } else {
                "agent".into()
            },
            model: c.model.clone(),
            ttfb_ms: ttfb,
            ttft_ms: ttft,
            total_ms: total,
            output_tps,
            output_tokens: usage.output_tokens,
            input_tokens: usage.input_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            cache_write_tokens: usage.cache_write_tokens,
            deltas: c.deltas,
            tool_early_start_ms: match (c.tool_block_stops_us.first(), c.end_us) {
                (Some(&a), Some(b)) => Some(span_ms(a, b)),
                _ => None,
            },
            body: c.body.clone(),
            build_ms: ms(c.build_us),
            error: c.error.clone(),
        });
    }

    // Gaps before each agent call.
    let agent: Vec<&CallTiming> = probe.calls.iter().filter(|c| !is_compaction(c)).collect();
    let compactions: Vec<&CallTiming> = probe.calls.iter().filter(|c| is_compaction(c)).collect();
    for (i, c) in agent.iter().enumerate() {
        let turn = turn_of(c.start_us);
        let prev_end = i
            .checked_sub(1)
            .and_then(|p| agent[p].end_us)
            .filter(|&e| turn > 0 && turn_of(e) == turn);
        let (from, turn_start) = match prev_end {
            Some(e) => (e, false),
            None => match turns.get(turn.wrapping_sub(1)) {
                Some(s) => (s.start_us, true),
                None => continue,
            },
        };
        let to = c.start_us;
        report.gaps.push(gap(
            turn,
            c.index,
            turn_start,
            from,
            to,
            &timed,
            &hooks,
            &compactions,
        ));
    }

    // Turns.
    for (n, s) in turns.iter().enumerate() {
        let turn = n + 1;
        let calls: Vec<&CallTiming> = probe
            .calls
            .iter()
            .filter(|c| inside(c.start_us, s.start_us, s.end_us))
            .collect();
        let model_us: u64 = calls
            .iter()
            .filter(|c| !is_compaction(c))
            .map(|c| c.end_us.unwrap_or(c.start_us).saturating_sub(c.start_us))
            .sum();
        let gaps: Vec<&GapRow> = report.gaps.iter().filter(|g| g.turn == turn).collect();
        let last_end = calls
            .iter()
            .filter_map(|c| c.end_us)
            .max()
            .unwrap_or(s.end_us);
        let (verify_after, _) = window(last_end, s.end_us, &timed);
        let wall = span_ms(s.start_us, s.end_us);
        let tools: f64 = gaps.iter().map(|g| g.tools_ms).sum();
        let verify = gaps.iter().map(|g| g.verify_ms).sum::<f64>() + verify_after.verify_ms;
        let hooks_ms: f64 = gaps.iter().map(|g| g.hooks_ms).sum();
        report.turns.push(TurnRow {
            turn,
            wall_ms: wall,
            calls: calls.len() as u32,
            model_ms: ms(model_us),
            tools_ms: tools + verify_after.tools_ms,
            verify_ms: verify,
            hooks_ms,
            overhead_ms: (wall - ms(model_us) - tools - verify_after.tools_ms - verify - hooks_ms)
                .max(0.0),
            start_ms: gaps.iter().find(|g| g.turn_start).map_or(0.0, |g| g.gap_ms),
            end_ms: span_ms(last_end, s.end_us),
        });
    }

    // Tools and hooks.
    let mut tools: BTreeMap<String, ToolRow> = BTreeMap::new();
    let mut names: BTreeMap<&str, &str> = BTreeMap::new();
    for e in events {
        match &e.kind {
            EventKind::ToolCall { call_id, tool, .. } => {
                names.insert(call_id, tool);
            }
            EventKind::ToolResult {
                call_id,
                duration_ms,
                ..
            } => {
                let tool = names.get(call_id.as_str()).copied().unwrap_or("?");
                let row = tools.entry(tool.to_owned()).or_insert_with(|| ToolRow {
                    tool: tool.to_owned(),
                    ..ToolRow::default()
                });
                row.calls += 1;
                row.total_ms += *duration_ms as f64;
                row.max_ms = row.max_ms.max(*duration_ms as f64);
            }
            EventKind::Cost { usd } => report.cost_usd += usd,
            _ => {}
        }
    }
    report.tools = tools.into_values().collect();
    let mut hook_rows: BTreeMap<String, HookRow> = BTreeMap::new();
    for h in &hooks {
        let name = h.name.trim_start_matches(HOOK_SPAN).to_owned();
        let row = hook_rows.entry(name.clone()).or_insert_with(|| HookRow {
            hook: name,
            ..HookRow::default()
        });
        let d = span_ms(h.start_us, h.end_us);
        row.calls += 1;
        row.total_ms += d;
        row.max_ms = row.max_ms.max(d);
    }
    report.hooks = hook_rows.into_values().collect();
    report
}

#[derive(Default)]
struct Window {
    tools_ms: f64,
    tool_sum_ms: f64,
    tool_calls: u32,
    verify_ms: f64,
    last_tool_us: Option<u64>,
}

fn window<'a>(
    from: u64,
    to: u64,
    timed: &[(u64, &'a RunEvent)],
) -> (Window, Vec<(u64, &'a RunEvent)>) {
    let inside: Vec<(u64, &RunEvent)> = timed
        .iter()
        .filter(|(t, _)| *t >= from && *t <= to)
        .copied()
        .collect();
    let mut w = Window::default();
    let mut first_call = None;
    let mut verify_start = None;
    for (t, e) in &inside {
        match &e.kind {
            EventKind::ToolCall { .. } => {
                first_call.get_or_insert(*t);
                w.tool_calls += 1;
            }
            EventKind::ToolResult { duration_ms, .. } => {
                w.tool_sum_ms += *duration_ms as f64;
                w.last_tool_us = Some(*t);
            }
            EventKind::VerifyStarted { .. } => verify_start = Some(*t),
            EventKind::VerifyFinished { duration_ms, .. } => {
                w.verify_ms += match verify_start.take() {
                    Some(s) => span_ms(s, *t),
                    None => *duration_ms as f64,
                };
            }
            _ => {}
        }
    }
    if let (Some(a), Some(b)) = (first_call, w.last_tool_us) {
        w.tools_ms = span_ms(a, b);
    }
    (w, inside)
}

#[allow(clippy::too_many_arguments)]
fn gap(
    turn: usize,
    before_call: usize,
    turn_start: bool,
    from: u64,
    to: u64,
    timed: &[(u64, &RunEvent)],
    hooks: &[&Span],
    compactions: &[&CallTiming],
) -> GapRow {
    let (w, inside) = window(from, to, timed);
    let gap_hooks: Vec<&&Span> = hooks
        .iter()
        .filter(|h| h.start_us >= from && h.end_us <= to)
        .collect();
    let hooks_ms: f64 = gap_hooks
        .iter()
        .map(|h| span_ms(h.start_us, h.end_us))
        .sum();
    let compaction_ms = compactions
        .iter()
        .flat_map(|c| {
            let end = c.end_us.unwrap_or(c.start_us);
            gap_hooks
                .iter()
                .filter(|h| h.name == CONTEXT_HOOK)
                .map(move |h| span_ms(c.start_us.max(h.start_us), end.min(h.end_us)))
        })
        .fold(0.0, |a, b| a + b);
    let gap_ms = span_ms(from, to);
    let finish_ms = (!turn_start)
        .then(|| {
            inside
                .iter()
                .find(|(_, e)| matches!(e.kind, EventKind::ModelCallFinished { .. }))
                .map(|(t, _)| span_ms(from, *t))
        })
        .flatten();
    let started = inside
        .iter()
        .rev()
        .find(|(_, e)| matches!(e.kind, EventKind::ModelCallStarted { .. }))
        .map(|(t, _)| *t);
    let before_build = gap_hooks
        .iter()
        .map(|h| h.end_us)
        .max()
        .or(w.last_tool_us)
        .unwrap_or(from);
    GapRow {
        turn,
        before_call,
        turn_start,
        gap_ms,
        tools_ms: w.tools_ms,
        tool_sum_ms: w.tool_sum_ms,
        tool_calls: w.tool_calls,
        verify_ms: w.verify_ms,
        hooks_ms,
        compaction_ms,
        overhead_ms: (gap_ms - w.tools_ms - w.verify_ms - hooks_ms).max(0.0),
        finish_ms,
        build_ms: started.map(|s| span_ms(before_build, s)),
        send_ms: started.map(|s| span_ms(s, to)),
    }
}

/// Folds runs into percentiles.
pub fn summarize(runs: &[RunReport]) -> Summary {
    let agent = || {
        runs.iter()
            .flat_map(|r| r.calls.iter())
            .filter(|c| c.kind == "agent" && c.error.is_none())
    };
    let collect =
        |f: &dyn Fn(&CallRow) -> Option<f64>| -> Vec<f64> { agent().filter_map(f).collect() };
    let gaps = || runs.iter().flat_map(|r| r.gaps.iter());
    let mid: Vec<f64> = gaps()
        .filter(|g| !g.turn_start)
        .map(|g| g.overhead_ms)
        .collect();
    let starts: Vec<f64> = gaps().filter(|g| g.turn_start).map(|g| g.gap_ms).collect();
    let tools: Vec<f64> = gaps()
        .filter(|g| g.tool_calls > 0)
        .map(|g| g.tools_ms)
        .collect();
    let hooks: Vec<f64> = gaps().map(|g| g.hooks_ms).collect();
    let run_overhead: Vec<f64> = runs
        .iter()
        .map(|r| r.turns.iter().map(|t| t.overhead_ms).sum())
        .collect();
    let per_call: Vec<f64> = runs
        .iter()
        .filter_map(|r| {
            let v: Vec<f64> = r
                .gaps
                .iter()
                .filter(|g| !g.turn_start)
                .map(|g| g.overhead_ms)
                .collect();
            Stat::of(&v).map(|s| s.p50)
        })
        .collect();
    let prefix: Vec<f64> = agent()
        .filter_map(|c| c.body.as_ref())
        .filter(|b| b.prefix_prev_total > 0)
        .map(|b| 100.0 * b.prefix_shared as f64 / b.prefix_prev_total as f64)
        .collect();
    let hits: Vec<f64> = runs
        .iter()
        .flat_map(|r| r.calls.iter().filter(|c| c.kind == "agent").skip(1))
        .filter_map(|c| {
            let input = c.input_tokens + c.cache_read_tokens + c.cache_write_tokens;
            (input > 0).then(|| 100.0 * c.cache_read_tokens as f64 / input as f64)
        })
        .collect();
    Summary {
        ttfb_ms: Stat::of(&collect(&|c| c.ttfb_ms)),
        ttft_ms: Stat::of(&collect(&|c| c.ttft_ms)),
        call_ms: Stat::of(&collect(&|c| c.total_ms)),
        output_tps: Stat::of(&collect(&|c| c.output_tps)),
        overhead_ms: Stat::of(&mid),
        turn_start_ms: Stat::of(&starts),
        tools_ms: Stat::of(&tools),
        hooks_ms: Stat::of(&hooks),
        run_wall_ms: Stat::of(&runs.iter().map(|r| r.wall_ms).collect::<Vec<_>>()),
        session_start_ms: Stat::of(&runs.iter().map(|r| r.session_start_ms).collect::<Vec<_>>()),
        first_call_ttfb_ms: Stat::of(
            &runs
                .iter()
                .filter_map(|r| r.calls.iter().find(|c| c.kind == "agent"))
                .filter_map(|c| c.ttfb_ms)
                .collect::<Vec<_>>(),
        ),
        later_call_ttfb_ms: Stat::of(
            &runs
                .iter()
                .flat_map(|r| r.calls.iter().filter(|c| c.kind == "agent").skip(1))
                .filter_map(|c| c.ttfb_ms)
                .collect::<Vec<_>>(),
        ),
        run_overhead_ms: Stat::of(&run_overhead),
        overhead_per_call_ms: Stat::of(&per_call),
        prefix_breaks: prefix.iter().filter(|p| **p < 100.0).count() as u32,
        prefix_shared_pct: Stat::of(&prefix),
        cache_hit_pct: Stat::of(&hits),
        tool_early_start_ms: Stat::of(&collect(&|c| c.tool_early_start_ms)),
        compaction_call_ms: Stat::of(
            &runs
                .iter()
                .flat_map(|r| r.calls.iter().filter(|c| c.kind == "compaction"))
                .filter_map(|c| c.total_ms)
                .collect::<Vec<_>>(),
        ),
        compaction_blocking_ms: Stat::of(
            &runs
                .iter()
                .filter(|r| r.calls.iter().any(|c| c.kind == "compaction"))
                .map(|r| r.gaps.iter().map(|g| g.compaction_ms).sum())
                .collect::<Vec<_>>(),
        ),
        cost_usd: runs.iter().map(|r| r.cost_usd).sum(),
    }
}

/// Replays `events` into a fresh run log in a temporary directory and times
/// each `emit` (redaction, blob spill, serialization, queueing), by event
/// type. Payloads should be inline (resolve blob references first) so the
/// cost matches the live path.
pub async fn emit_costs(events: &[RunEvent], redactor: Arc<Redactor>) -> Vec<EmitCost> {
    let Ok(dir) = tempfile::tempdir() else {
        return Vec::new();
    };
    let Ok(recorder) = RunRecorder::create(
        dir.path(),
        harness_events::schema::RunId::new(),
        redactor,
        RecorderOptions::default(),
    )
    .await
    else {
        return Vec::new();
    };
    let mut costs: BTreeMap<&'static str, EmitCost> = BTreeMap::new();
    for e in events {
        let draft = EventDraft {
            node_id: e.node_id.clone(),
            parent_node_id: e.parent_node_id.clone(),
            kind: e.kind.clone(),
        };
        let name = e.kind.type_name();
        let start = Instant::now();
        let _ = recorder.emit(draft);
        let us = start.elapsed().as_micros() as u64;
        let row = costs.entry(name).or_insert_with(|| EmitCost {
            event: name.to_owned(),
            ..EmitCost::default()
        });
        row.count += 1;
        row.total_us += us;
        row.max_us = row.max_us.max(us);
    }
    let _ = recorder.finish().await;
    let mut out: Vec<EmitCost> = costs.into_values().collect();
    out.sort_by_key(|c| std::cmp::Reverse(c.total_us));
    out
}

fn opt(v: Option<f64>) -> String {
    v.map_or_else(|| "-".into(), |v| format!("{v:.1}"))
}

fn stat_line(out: &mut String, name: &str, s: &Option<Stat>, unit: &str) {
    if let Some(s) = s {
        let _ = writeln!(
            out,
            "  {name:<28} p50 {:>9.2}  p90 {:>9.2}  min {:>9.2}  max {:>9.2}  (n={}) {unit}",
            s.p50, s.p90, s.min, s.max, s.n
        );
    }
}

/// The human-readable report.
pub fn render_table(r: &BenchReport) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "harness bench: mode {} model {} config {} | {} {} cpus, {} build, harness {}",
        r.mode,
        r.model,
        r.config,
        r.machine.os,
        r.machine.cpus,
        r.machine.profile,
        r.machine.harness_version
    );
    for run in &r.runs {
        let _ = writeln!(
            out,
            "\nrun {}: {:.0} ms wall, {} calls, {} events, ${:.4}, session start {:.1} ms{}",
            run.label,
            run.wall_ms,
            run.calls.len(),
            run.events,
            run.cost_usd,
            run.session_start_ms,
            match (run.prewarm_ms, run.prewarm_before_first_call) {
                (Some(p), Some(b)) => format!(", prewarm {p:.1} ms (done before first call: {b})"),
                (Some(p), None) => format!(", prewarm {p:.1} ms"),
                _ => String::new(),
            }
        );
        let _ = writeln!(
            out,
            "  {:>3} {:>2} {:<10} {:>9} {:>9} {:>9} {:>7} {:>6} {:>7} {:>7} {:>7} {:>8} {:>7} {:>6}",
            "#",
            "t",
            "kind",
            "ttfb ms",
            "ttft ms",
            "call ms",
            "tok/s",
            "out",
            "in",
            "cache_r",
            "cache_w",
            "body B",
            "prefix%",
            "early"
        );
        for c in &run.calls {
            let (bytes, prefix) = c.body.as_ref().map_or((0, None), |b| {
                (
                    b.total,
                    (b.prefix_prev_total > 0)
                        .then(|| 100.0 * b.prefix_shared as f64 / b.prefix_prev_total as f64),
                )
            });
            let _ = writeln!(
                out,
                "  {:>3} {:>2} {:<10} {:>9} {:>9} {:>9} {:>7} {:>6} {:>7} {:>7} {:>7} {:>8} {:>7} {:>6}{}",
                c.index,
                c.turn,
                c.kind,
                opt(c.ttfb_ms),
                opt(c.ttft_ms),
                opt(c.total_ms),
                opt(c.output_tps),
                c.output_tokens,
                c.input_tokens,
                c.cache_read_tokens,
                c.cache_write_tokens,
                bytes,
                opt(prefix),
                opt(c.tool_early_start_ms),
                c.error
                    .as_ref()
                    .map_or(String::new(), |e| format!("  error: {e}")),
            );
        }
        let _ = writeln!(
            out,
            "  gaps (ms): {:>5} {:>8} {:>8} {:>8} {:>6} {:>8} {:>8} {:>9} {:>8} {:>8} {:>8}",
            "call",
            "gap",
            "tools",
            "toolsum",
            "calls",
            "verify",
            "hooks",
            "overhead",
            "finish",
            "build",
            "send"
        );
        for g in &run.gaps {
            let _ = writeln!(
                out,
                "  {:<10} {:>5} {:>8.2} {:>8.2} {:>8.2} {:>6} {:>8.2} {:>8.2} {:>9.2} {:>8} {:>8} {:>8}",
                if g.turn_start { "turn-start" } else { "" },
                g.before_call,
                g.gap_ms,
                g.tools_ms,
                g.tool_sum_ms,
                g.tool_calls,
                g.verify_ms,
                g.hooks_ms,
                g.overhead_ms,
                g.finish_ms.map_or("-".into(), |v| format!("{v:.2}")),
                g.build_ms.map_or("-".into(), |v| format!("{v:.2}")),
                g.send_ms.map_or("-".into(), |v| format!("{v:.2}")),
            );
        }
        for t in &run.turns {
            let _ = writeln!(
                out,
                "  turn {}: wall {:.1} ms = model {:.1} + tools {:.1} + verify {:.1} + hooks {:.1} + overhead {:.1} (start {:.1}, end {:.1}), {} calls",
                t.turn,
                t.wall_ms,
                t.model_ms,
                t.tools_ms,
                t.verify_ms,
                t.hooks_ms,
                t.overhead_ms,
                t.start_ms,
                t.end_ms,
                t.calls
            );
        }
        let tools: Vec<String> = run
            .tools
            .iter()
            .map(|t| {
                format!(
                    "{} {}x {:.1}ms (max {:.1})",
                    t.tool, t.calls, t.total_ms, t.max_ms
                )
            })
            .collect();
        let _ = writeln!(out, "  tools: {}", tools.join(", "));
        let hooks: Vec<String> = run
            .hooks
            .iter()
            .map(|h| {
                format!(
                    "{} {}x {:.2}ms (max {:.2})",
                    h.hook, h.calls, h.total_ms, h.max_ms
                )
            })
            .collect();
        let _ = writeln!(out, "  hooks: {}", hooks.join(", "));
    }
    let s = &r.summary;
    let _ = writeln!(out, "\nsummary over {} run(s):", r.runs.len());
    stat_line(&mut out, "ttfb", &s.ttfb_ms, "ms");
    stat_line(&mut out, "ttfb first call", &s.first_call_ttfb_ms, "ms");
    stat_line(&mut out, "ttfb later calls", &s.later_call_ttfb_ms, "ms");
    stat_line(&mut out, "ttft", &s.ttft_ms, "ms");
    stat_line(&mut out, "call", &s.call_ms, "ms");
    stat_line(&mut out, "output", &s.output_tps, "tok/s");
    stat_line(&mut out, "overhead per gap", &s.overhead_ms, "ms");
    stat_line(
        &mut out,
        "overhead per call (run p50)",
        &s.overhead_per_call_ms,
        "ms",
    );
    stat_line(&mut out, "turn start", &s.turn_start_ms, "ms");
    stat_line(&mut out, "tool dispatch", &s.tools_ms, "ms");
    stat_line(&mut out, "hooks per gap", &s.hooks_ms, "ms");
    stat_line(&mut out, "run wall", &s.run_wall_ms, "ms");
    stat_line(&mut out, "session start", &s.session_start_ms, "ms");
    stat_line(&mut out, "run overhead", &s.run_overhead_ms, "ms");
    stat_line(&mut out, "prev request reused", &s.prefix_shared_pct, "%");
    let _ = writeln!(out, "  prefix breaks: {}", s.prefix_breaks);
    stat_line(&mut out, "cache hit (calls 2+)", &s.cache_hit_pct, "%");
    stat_line(&mut out, "tool early start", &s.tool_early_start_ms, "ms");
    stat_line(&mut out, "compaction call", &s.compaction_call_ms, "ms");
    stat_line(
        &mut out,
        "compaction wait per run",
        &s.compaction_blocking_ms,
        "ms",
    );
    let _ = writeln!(out, "  cost: ${:.4}", s.cost_usd);
    if !r.tool_schemas.is_empty() {
        let total: u64 = r.tool_schemas.iter().map(|t| t.bytes).sum();
        let list: Vec<String> = r
            .tool_schemas
            .iter()
            .map(|t| format!("{} {}B", t.name, t.bytes))
            .collect();
        let _ = writeln!(
            out,
            "\ntool schemas: {} tools, {total} bytes (~{} tokens): {}",
            r.tool_schemas.len(),
            total / 4,
            list.join(", ")
        );
    }
    if !r.emit.is_empty() {
        let total: u64 = r.emit.iter().map(|e| e.total_us).sum();
        let _ = writeln!(out, "event recording (replayed): {:.2} ms total", ms(total));
        for e in r.emit.iter().take(8) {
            let _ = writeln!(
                out,
                "  {:<22} {:>5}x  total {:>8.3} ms  mean {:>7.1} us  max {:>7} us",
                e.event,
                e.count,
                ms(e.total_us),
                e.total_us as f64 / e.count.max(1) as f64,
                e.max_us
            );
        }
    }
    if let Some(rs) = &r.render {
        let _ = writeln!(
            out,
            "tui render ({}x{}): {} events, {} frames",
            rs.size.0, rs.size.1, rs.events, rs.frames
        );
        stat_line(&mut out, "frame draw", &rs.frame_ms, "ms");
        stat_line(&mut out, "event handle", &rs.handle_ms, "ms");
    }
    if let Some(n) = &r.net {
        let _ = writeln!(
            out,
            "connection {} ({}): cold {:?} ms, warm {:?} ms, new provider client {:?} ms",
            n.url, n.version, n.cold_ms, n.warm_ms, n.new_client_ms
        );
    }
    out
}

#[cfg(test)]
mod tests;
