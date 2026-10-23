//! `harness runs show <id> [--json]`.
//!
//! [`tree_text`] prints a run as an indented tree (steps in order, spawned
//! subagents beneath their parent) with status, agent and model, host,
//! duration, cost, tokens and errors. [`state_json`] is the reducer state
//! itself, so headless users see exactly what the TUI folds. [`run_show`]
//! resolves an id or prefix (or `last`) under the data dir and prints
//! either form; it reads live runs too (a torn last line is skipped).

use std::io::Write;
use std::path::Path;

use harness_events::names;
use harness_events::reducer::{NodeState, RunState};
use harness_events::schema::{NodeStatus, RunId, RunStatus};
use time::{OffsetDateTime, UtcOffset};

use crate::chat::text::{fmt_cost, fmt_ms, fmt_tokens};
use crate::map::metrics::{critical_path, elapsed_ms, run_elapsed_ms};

fn icon(s: NodeStatus) -> char {
    match s {
        NodeStatus::Queued => '○',
        NodeStatus::Running => '◐',
        NodeStatus::WaitingApproval => '⏸',
        NodeStatus::Succeeded => '✔',
        NodeStatus::Failed => '✗',
        NodeStatus::Skipped | NodeStatus::Cancelled => '⊘',
    }
}

fn node_line(run: &RunState, n: &NodeState, now: OffsetDateTime) -> String {
    let mut parts = vec![format!("{} {}", icon(n.status), run.node_label(n))];
    let who: Vec<String> = [run.node_agent_detail(n), n.model.clone()]
        .into_iter()
        .flatten()
        .collect();
    if !who.is_empty() {
        parts.push(who.join(" · "));
    }
    if let Some(h) = &n.host {
        parts.push(format!("⧉ {h}"));
    }
    if let Some(ms) = elapsed_ms(n, now) {
        parts.push(fmt_ms(ms));
    }
    parts.push(fmt_cost(Some(n.cost_usd)));
    if n.model_calls > 0 {
        parts.push(format!(
            "↑{} ↓{}",
            fmt_tokens(n.tokens.input + n.tokens.cached_input),
            fmt_tokens(n.tokens.output)
        ));
    }
    if n.retries > 0 {
        parts.push(format!("↻{}", n.retries));
    }
    if n.pending_approvals > 0 {
        parts.push("waiting for approval".into());
    }
    let mut line = parts.join("  ");
    if let Some(e) = &n.error {
        line.push_str(&format!("  error: {}", e.lines().next().unwrap_or("")));
    }
    line
}

/// The run as a text tree.
pub fn tree_text(run: &RunState, now: OffsetDateTime) -> String {
    let status = match run.status {
        RunStatus::Running => "running",
        RunStatus::Succeeded => "succeeded",
        RunStatus::Failed => "failed",
        RunStatus::Cancelled => "cancelled",
    };
    let mut out = String::new();
    out.push_str(&format!("{}  {status}", run.label()));
    if let Some(ms) = run_elapsed_ms(run, now) {
        out.push_str(&format!("  {}", fmt_ms(ms)));
    }
    out.push_str(&format!(
        "  {}  ↑{} ↓{}",
        fmt_cost(Some(run.cost_usd)),
        fmt_tokens(run.tokens.input + run.tokens.cached_input),
        fmt_tokens(run.tokens.output)
    ));
    let (cp, _) = critical_path(run, now);
    if cp > 0 {
        out.push_str(&format!("  critical path {}", fmt_ms(cp)));
    }
    out.push('\n');
    if let Some(id) = run.run_id.map(|i| i.to_string()) {
        let tag = names::short_tag(&id);
        out.push_str(&format!("id {id} ({tag})"));
        if let Some(t) = run.started_at {
            out.push_str(&format!(
                "  started {}",
                names::when_label(t, UtcOffset::UTC, now)
            ));
        }
        out.push('\n');
    }
    if let Some(e) = &run.error {
        out.push_str(&format!("error: {e}\n"));
    }
    let roots: Vec<&NodeState> = run.roots().collect();
    for (i, n) in roots.iter().enumerate() {
        write_node(run, n, "", i + 1 == roots.len(), now, &mut out);
    }
    out
}

fn write_node(
    run: &RunState,
    n: &NodeState,
    prefix: &str,
    last: bool,
    now: OffsetDateTime,
    out: &mut String,
) {
    let branch = if last { "└─ " } else { "├─ " };
    out.push_str(&format!("{prefix}{branch}{}", node_line(run, n, now)));
    let needs: Vec<&str> = run
        .edges
        .iter()
        .filter(|e| e.kind == harness_events::reducer::EdgeKind::Needs && e.to == n.id)
        .map(|e| e.from.as_str())
        .collect();
    if !needs.is_empty() {
        out.push_str(&format!("  needs: {}", needs.join(", ")));
    }
    out.push('\n');
    let child_prefix = format!("{prefix}{}", if last { "   " } else { "│  " });
    let kids: Vec<&NodeState> = n.children.iter().filter_map(|c| run.node(c)).collect();
    for (i, c) in kids.iter().enumerate() {
        write_node(run, c, &child_prefix, i + 1 == kids.len(), now, out);
    }
}

/// The reducer state as JSON (`--json`).
pub fn state_json(run: &RunState) -> serde_json::Value {
    serde_json::to_value(run).unwrap_or_default()
}

/// Resolves `id` (full id, unique prefix, `last`, or the `#tag` shown
/// next to colliding titles) under `data_dir`.
pub fn resolve_run(data_dir: &Path, id: &str) -> Result<RunId, String> {
    let mut ids = harness_events::store::list_runs(data_dir).map_err(|e| e.to_string())?;
    if ids.is_empty() {
        return Err("no runs recorded yet".into());
    }
    if id == "last" {
        ids.sort_by_key(|r| r.0);
        return Ok(*ids.last().expect("non-empty"));
    }
    let mut matches: Vec<RunId> = ids
        .iter()
        .copied()
        .filter(|r| r.to_string().starts_with(id))
        .collect();
    if matches.is_empty() {
        matches = ids
            .into_iter()
            .filter(|r| names::matches_tag(&r.to_string(), id))
            .collect();
    }
    match matches.as_slice() {
        [one] => Ok(*one),
        [] => Err(format!("no run matches `{id}`")),
        many => Err(format!(
            "`{id}` matches {} runs; use a longer prefix",
            many.len()
        )),
    }
}

/// `harness runs show`: prints the tree (or JSON with `json`) of run `id`
/// to `out`, errors to `err`. Returns the exit code (0 ok, 1 error).
pub fn run_show(
    data_dir: &Path,
    id: &str,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let run_id = match resolve_run(data_dir, id) {
        Ok(r) => r,
        Err(e) => {
            let _ = writeln!(err, "harness runs show: {e}");
            return 1;
        }
    };
    let (events, truncated) = match super::load_run(data_dir, run_id) {
        Ok(v) => v,
        Err(e) => {
            let _ = writeln!(err, "harness runs show: {e}");
            return 1;
        }
    };
    let state = RunState::from_events(&events);
    let res = if json {
        writeln!(
            out,
            "{}",
            serde_json::to_string_pretty(&state_json(&state)).unwrap_or_default()
        )
    } else {
        let mut text = tree_text(&state, OffsetDateTime::now_utc());
        if truncated {
            text.push_str("(log ends in an incomplete line: run still writing or crashed)\n");
        }
        write!(out, "{text}")
    };
    if res.is_err() { 1 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo::events::{EventBuilder, NodeSpec, usage};
    use harness_events::redact::Redactor;
    use harness_events::store::{EventDraft, RecorderOptions, RunRecorder};
    use std::sync::Arc;

    fn events() -> EventBuilder {
        let mut b = EventBuilder::new();
        b.run_started("fix-bug", Some("fix-bug"));
        b.start(NodeSpec::step("repro").agent("implementer").model("sonnet"));
        b.advance(41_000);
        b.cost("repro", 0.08);
        b.finish("repro", NodeStatus::Succeeded, None);
        b.start(
            NodeSpec::step("fix")
                .agent("implementer")
                .host("gpu-box")
                .needs(&["repro"]),
        );
        b.model_call("fix", "c", "sonnet");
        b.finish_call("fix", "c", usage(100, 900, 50, 0), 1);
        b.start(
            NodeSpec::subagent("r1", "fix")
                .name("researcher")
                .model("haiku"),
        );
        b
    }

    #[test]
    fn prints_a_tree_and_the_reducer_json() {
        let b = events();
        let run = RunState::from_events(b.events());
        let text = tree_text(&run, b.now());
        insta::assert_snapshot!("runs_show_tree", text);
        let v = state_json(&run);
        assert_eq!(v["nodes"].as_array().unwrap().len(), 3);
        assert_eq!(v, serde_json::to_value(&run).unwrap());
    }

    #[test]
    fn agent_runs_show_their_title_and_display_names() {
        let mut b = EventBuilder::new();
        b.run_started("coder", None);
        b.start(NodeSpec::step("main").agent("coder").model("sonnet"));
        b.user_message("main", "Fix the header parser");
        b.start(
            NodeSpec::subagent("r1", "main")
                .name("researcher")
                .agent("researcher")
                .model("haiku"),
        );
        let run = RunState::from_events(b.events());
        let text = tree_text(&run, b.now());
        let mut lines = text.lines();
        let head = lines.next().unwrap();
        assert!(
            head.starts_with("Coder · Fix the header parser  running"),
            "{text}"
        );
        let id = lines.next().unwrap();
        assert!(
            id.starts_with("id 00000000-0000-0000-0000-000000000000 (#000000)"),
            "{text}"
        );
        let rest: Vec<&str> = lines.collect();
        assert!(rest[0].contains("◐ main  Coder · sonnet"), "{text}");
        assert!(rest[1].contains("◐ Researcher  haiku"), "{text}");
        // Only the id line carries the full id.
        assert!(names::find_raw_id(head).is_none());
        assert!(
            rest.iter().all(|l| names::find_raw_id(l).is_none()),
            "{text}"
        );
    }

    async fn record(
        dir: &Path,
        events: &[harness_events::schema::RunEvent],
        finish: bool,
    ) -> RunId {
        let id = RunId::new();
        let rec = RunRecorder::create(
            dir,
            id,
            Arc::new(Redactor::default()),
            RecorderOptions::default(),
        )
        .await
        .unwrap();
        for e in events {
            rec.emit(EventDraft {
                node_id: e.node_id.clone(),
                parent_node_id: e.parent_node_id.clone(),
                kind: e.kind.clone(),
            })
            .unwrap();
        }
        rec.flush().await.unwrap();
        if finish {
            rec.finish().await.unwrap();
        } else {
            // A live run: a torn line from a write in progress.
            use std::io::Write as _;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(rec.dir().join(harness_events::store::EVENTS_FILE))
                .unwrap();
            f.write_all(b"{\"seq\": 99, \"ts\"").unwrap();
            std::mem::forget(rec);
        }
        id
    }

    #[tokio::test]
    async fn works_on_live_and_finished_runs() {
        let dir = tempfile::tempdir().unwrap();
        let mut b = events();
        let live = record(dir.path(), b.events(), false).await;
        b.finish("fix", NodeStatus::Succeeded, None);
        b.run_finished(RunStatus::Succeeded);
        let done = record(dir.path(), b.events(), true).await;

        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(
            run_show(
                dir.path(),
                &live.to_string()[..13],
                false,
                &mut out,
                &mut err
            ),
            0,
            "{}",
            String::from_utf8_lossy(&err)
        );
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("fix-bug"), "{text}");
        assert!(text.contains("running"), "{text}");
        assert!(text.contains("incomplete line"), "{text}");

        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(
            run_show(dir.path(), &done.to_string(), true, &mut out, &mut err),
            0
        );
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["status"], "succeeded");

        // The `#tag` shown on colliding titles works too, with or without `#`.
        let tag = names::short_tag(&done.to_string());
        for q in [tag.clone(), tag.trim_start_matches('#').to_owned()] {
            assert_eq!(resolve_run(dir.path(), &q), Ok(done), "{q}");
        }
        assert_eq!(resolve_run(dir.path(), &done.to_string()), Ok(done));

        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(run_show(dir.path(), "zzzz", false, &mut out, &mut err), 1);
        assert!(String::from_utf8(err).unwrap().contains("no run matches"));
    }
}
