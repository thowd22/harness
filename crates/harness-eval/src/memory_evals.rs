//! Memory evals: memory only helps if corrections stick and retrieval is
//! relevant, and dreaming must never lose what users told it.
//!
//! - [`correction_eval`]: a correction given in session 1 is applied in
//!   session 2. Session 1 runs the agent with the memory tool; session 2
//!   starts fresh with the memory index in its system prompt, and its output
//!   is checked for the corrected behaviour.
//! - [`retrieval_eval`]: seeded notes, seeded queries; recall@k and MRR of
//!   `memory search`.
//! - [`dream_preservation_eval`]: after a dreaming pass over a seeded drive,
//!   every explicit preference and every source link of a kept memory is
//!   still there.
//!
//! All three take a [`Provider`] or work without a model, so they run with
//! the fake provider in CI and against real models on demand.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use harness_agent::agent_loop::{AgentLoop, Conversation, LoopConfig, LoopOutcome};
use harness_agent::dispatch::{AgentTool, AllowAll, Dispatcher, ToolSet};
use harness_config::agents::MemoryAccess;
use harness_events::redact::Redactor;
use harness_events::schema::{EventKind, RunId};
use harness_events::store::{EventDraft, RecorderOptions, RunRecorder};
use harness_memory::dream::consolidate::{is_preference, normalize, sources};
use harness_memory::dream::{DreamSettings, Dreamer};
use harness_memory::drive::{Drive, Note, list_notes, read_note_at};
use harness_memory::index::{SearchIndex, SessionIndex};
use harness_memory::sync::SessionCheckout;
use harness_memory::tool::{MemoryTool, MemoryToolConfig};
use harness_providers::provider::Provider;
use harness_tools::tool::{ToolContext, ToolError, ToolOutput};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

/// The memory tool as an agent tool.
struct MemoryAdapter(MemoryTool);

#[async_trait]
impl AgentTool for MemoryAdapter {
    fn name(&self) -> &str {
        harness_memory::tool::NAME
    }
    fn description(&self) -> &str {
        harness_memory::tool::DESCRIPTION
    }
    fn schema(&self) -> Value {
        harness_memory::tool::schema(self.0.access()).unwrap_or(Value::Null)
    }
    fn read_only(&self) -> bool {
        false
    }
    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        self.0.call(ctx, args).await
    }
}

/// A correction scenario.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorrectionCase {
    /// Session 1: the user's correction.
    pub correction: String,
    /// Session 2: a task where the correction applies.
    pub task: String,
    /// Session 2's output must contain each of these.
    pub expect: Vec<String>,
    /// ... and none of these.
    pub forbid: Vec<String>,
}

/// What happened in a correction scenario.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorrectionResult {
    /// Session 1 saved something to memory.
    pub memory_written: bool,
    /// Session 2 saw the saved memory: in its system prompt (the memory
    /// index) or in a result of its read-only memory tool.
    pub injected: bool,
    /// Session 2's output followed the correction.
    pub applied: bool,
    /// Session 2's output (final text plus tool arguments).
    pub output: String,
}

impl CorrectionResult {
    /// The eval passes when the correction was saved, loaded and applied.
    pub fn passed(&self) -> bool {
        self.memory_written && self.injected && self.applied
    }
}

async fn session(
    provider: Arc<dyn Provider>,
    model: &str,
    system: String,
    prompt: &str,
    tools: ToolSet,
    data: &Path,
) -> Result<(LoopOutcome, String, String), String> {
    let recorder = RunRecorder::create(
        data,
        RunId::new(),
        Arc::new(Redactor::default()),
        RecorderOptions::default(),
    )
    .await
    .map_err(|e| e.to_string())?;
    let _ = recorder.emit(EventDraft::run(EventKind::RunStarted {
        name: "memory-eval".into(),
        workflow: None,
        host: None,
    }));
    let dispatcher = Dispatcher::new(tools, Arc::new(AllowAll));
    let mut lp = AgentLoop::new(
        provider,
        dispatcher,
        recorder.clone(),
        LoopConfig::new("memory-eval", model),
    );
    let mut conv = Conversation::new(Some(system));
    conv.push_user(prompt);
    let outcome = lp.run(&mut conv, &CancellationToken::new()).await;
    drop(lp);
    let mut output = outcome.final_text.clone();
    let mut seen = String::new();
    for m in &conv.messages {
        for (_, name, input) in m.tool_uses() {
            output.push_str(&format!("\n[{name}] {input}"));
        }
        for b in &m.content {
            if let harness_providers::model::ContentBlock::ToolResult { content, .. } = b {
                for part in content {
                    if let harness_providers::model::ToolResultPart::Text { text } = part {
                        seen.push_str(text);
                        seen.push('\n');
                    }
                }
            }
        }
    }
    recorder.finish().await.map_err(|e| e.to_string())?;
    Ok((outcome, output, seen))
}

/// Runs a correction scenario on a fresh drive under `dir`. `first` and
/// `second` are the providers for the two sessions (the same real provider,
/// or two fake scripts).
pub async fn correction_eval(
    case: &CorrectionCase,
    first: Arc<dyn Provider>,
    second: Arc<dyn Provider>,
    model: &str,
    dir: &Path,
) -> Result<CorrectionResult, String> {
    let drive = Drive::open_or_init(dir.join("memory")).map_err(|e| e.to_string())?;
    let data = dir.join("data");
    let base = "You are a coding agent. Follow the user's instructions.";

    // Session 1: the user corrects the agent; it may save a memory.
    let co1 = Arc::new(SessionCheckout::open(&drive, "eval-s1").map_err(|e| e.to_string())?);
    let index1 = SessionIndex::load(co1.path(), None, 1500).map_err(|e| e.to_string())?;
    let tool = MemoryTool::new(
        co1.clone(),
        MemoryToolConfig {
            access: MemoryAccess::ReadWrite,
            run_id: "eval-s1".into(),
            project_key: None,
            redactor: Arc::new(Redactor::default()),
            date: None,
        },
    );
    let before = drive.head().map_err(|e| e.to_string())?;
    let system1 = format!(
        "{base}\n\n{}\n\n{}",
        harness_memory::extract::PROMPT_SECTION,
        index1.text()
    );
    session(
        first,
        model,
        system1,
        &case.correction,
        ToolSet::new().with(Arc::new(MemoryAdapter(tool))),
        &data,
    )
    .await?;
    let after = drive.head().map_err(|e| e.to_string())?;
    let memory_written = before != after;

    // Session 2: a fresh session; memory is loaded into the stable prefix.
    let co2 = SessionCheckout::open(&drive, "eval-s2").map_err(|e| e.to_string())?;
    let index2 = SessionIndex::load(co2.path(), None, 1500).map_err(|e| e.to_string())?;
    let saved: Vec<String> = list_notes(co2.path())
        .unwrap_or_default()
        .iter()
        .filter_map(|p| read_note_at(co2.path(), p).ok().flatten())
        .flat_map(|t| {
            Note::parse(&t)
                .bullets()
                .map(|b| b.text.clone())
                .collect::<Vec<_>>()
        })
        .collect();
    let reader = MemoryTool::new(
        Arc::new(co2),
        MemoryToolConfig {
            access: MemoryAccess::Read,
            run_id: "eval-s2".into(),
            project_key: None,
            redactor: Arc::new(Redactor::default()),
            date: None,
        },
    );
    let system2 = format!("{base}\n\n{}", index2.text());
    let (_, output, seen) = session(
        second,
        model,
        system2.clone(),
        &case.task,
        ToolSet::new().with(Arc::new(MemoryAdapter(reader))),
        &data,
    )
    .await?;
    let injected = memory_written
        && saved
            .iter()
            .any(|b| system2.contains(b.as_str()) || seen.contains(b.as_str()));
    let lower = output.to_lowercase();
    let applied = case
        .expect
        .iter()
        .all(|e| lower.contains(&e.to_lowercase()))
        && !case
            .forbid
            .iter()
            .any(|f| lower.contains(&f.to_lowercase()));
    Ok(CorrectionResult {
        memory_written,
        injected,
        applied,
        output,
    })
}

/// A retrieval query with the notes that should be found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrievalCase {
    /// The search query (as an agent would phrase it for the task).
    pub query: String,
    /// Drive-relative paths of relevant notes.
    pub relevant: Vec<String>,
}

/// Retrieval quality.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrievalReport {
    /// Cut-off.
    pub k: usize,
    /// Mean fraction of relevant notes in the top k.
    pub recall_at_k: f64,
    /// Mean reciprocal rank of the first relevant note.
    pub mrr: f64,
    /// Per query: the top-k paths.
    pub results: BTreeMap<String, Vec<String>>,
}

/// The seeded notes used by [`retrieval_cases`].
pub fn retrieval_seed() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "MEMORY.md",
            "# Memory\n- The user prefers short answers and small commits.\n",
        ),
        (
            "tools/cargo.md",
            "# Cargo\n- Use cargo nextest; plain cargo test hangs on the integration suite.\n- Build with --locked in CI so the lockfile is respected.\n",
        ),
        (
            "tools/docker.md",
            "# Docker\n- Run docker compose with the dev profile; the default profile needs production secrets.\n",
        ),
        (
            "tools/pnpm.md",
            "# pnpm\n- This user's JavaScript repos use pnpm; npm install breaks the workspace lockfile.\n",
        ),
        (
            "projects/app-1234abcd/testing.md",
            "# Testing (app)\n- Integration tests need the local Postgres from just db-up first.\n- Snapshot tests are reviewed with cargo insta review, never auto-accepted.\n",
        ),
        (
            "projects/app-1234abcd/style.md",
            "# Style (app)\n- Error messages say what to do next; the user rejected bare error codes.\n",
        ),
        (
            "projects/app-1234abcd/release.md",
            "# Release (app)\n- Releases are cut from the release branch after the changelog entry is merged.\n",
        ),
    ]
}

/// Seeded tasks and the notes that matter for them.
pub fn retrieval_cases() -> Vec<RetrievalCase> {
    let c = |q: &str, r: &[&str]| RetrievalCase {
        query: q.into(),
        relevant: r.iter().map(|s| (*s).to_owned()).collect(),
    };
    vec![
        c(
            "run the integration tests",
            &["projects/app-1234abcd/testing.md", "tools/cargo.md"],
        ),
        c(
            "install a javascript dependency with npm",
            &["tools/pnpm.md"],
        ),
        c(
            "start the services with docker compose",
            &["tools/docker.md"],
        ),
        c(
            "write an error message",
            &["projects/app-1234abcd/style.md"],
        ),
        c(
            "prepare the release changelog",
            &["projects/app-1234abcd/release.md"],
        ),
        c(
            "update snapshot tests",
            &["projects/app-1234abcd/testing.md"],
        ),
    ]
}

fn seed_err(source: std::io::Error) -> harness_memory::drive::DriveError {
    harness_memory::drive::DriveError::Io {
        context: "seeding the drive".into(),
        source,
    }
}

/// Writes `seed` into a fresh drive under `dir` (one commit).
pub fn seed_drive(dir: &Path, seed: &[(&str, &str)]) -> Result<Drive, String> {
    let drive = Drive::open_or_init(dir.join("memory")).map_err(|e| e.to_string())?;
    let co = SessionCheckout::open(&drive, "seed").map_err(|e| e.to_string())?;
    co.write("memory: write seed", |d| {
        for (rel, text) in seed {
            let p = d.join(rel);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).map_err(seed_err)?;
            }
            std::fs::write(&p, text).map_err(seed_err)?;
        }
        Ok(())
    })
    .map_err(|e| e.to_string())?;
    Ok(drive)
}

/// Runs `cases` against the notes of `drive` with `memory search`.
pub fn retrieval_eval(
    drive: &Drive,
    cases: &[RetrievalCase],
    k: usize,
) -> Result<RetrievalReport, String> {
    let mut idx = SearchIndex::new();
    idx.update(drive.root()).map_err(|e| e.to_string())?;
    let mut recall = 0.0;
    let mut mrr = 0.0;
    let mut results = BTreeMap::new();
    for case in cases {
        let hits: Vec<String> = idx
            .search(&case.query, k)
            .into_iter()
            .map(|h| h.path)
            .collect();
        let relevant: BTreeSet<&str> = case.relevant.iter().map(String::as_str).collect();
        let found = hits
            .iter()
            .filter(|h| relevant.contains(h.as_str()))
            .count();
        recall += found as f64 / relevant.len().max(1) as f64;
        if let Some(rank) = hits.iter().position(|h| relevant.contains(h.as_str())) {
            mrr += 1.0 / (rank + 1) as f64;
        }
        results.insert(case.query.clone(), hits);
    }
    let n = cases.len().max(1) as f64;
    Ok(RetrievalReport {
        k,
        recall_at_k: recall / n,
        mrr: mrr / n,
        results,
    })
}

/// What a dreaming pass kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preservation {
    /// Explicit preferences before the pass.
    pub preferences: usize,
    /// ... still present after it.
    pub preferences_kept: usize,
    /// Source links of memories that were kept or merged (not pruned).
    pub sources: usize,
    /// ... still present after the pass.
    pub sources_kept: usize,
    /// The pass summary.
    pub summary: String,
}

impl Preservation {
    /// Every preference and every source link survived.
    pub fn passed(&self) -> bool {
        self.preferences == self.preferences_kept && self.sources == self.sources_kept
    }
}

/// The seeded drive for [`dream_preservation_eval`]: preferences,
/// duplicates (with different sources), transient task state and a stale
/// note.
pub fn dream_seed() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "MEMORY.md",
            "# Memory\n- The user prefers tabs over spaces. (src: harness://run/p1 · 2026-01-01)\n- Never force-push to shared branches. (src: harness://run/p2 · 2026-01-01)\n",
        ),
        (
            "tools/cargo.md",
            "# Cargo\n- Use cargo nextest for the integration suite (src: harness://run/c1 · 2026-10-01)\n- use cargo nextest for the integration suite (src: harness://run/c2 · 2026-10-05)\n- PR #77 is blocked on the nextest upgrade (src: harness://run/c3 · 2026-10-06)\n",
        ),
        (
            "tools/legacy.md",
            "# Legacy\n- The v1 importer needed --compat (src: harness://run/l1 · 2025-12-01)\n- The user dislikes the legacy importer; do not suggest it (src: harness://run/l2 · 2025-12-01)\n",
        ),
        (
            "projects/app-1234abcd/style.md",
            "# Style\n- Prefer early returns over nested ifs (src: harness://run/s1 · 2026-10-10)\n- prefer early returns over nested ifs (src: harness://run/s2 · 2026-10-12)\n",
        ),
    ]
}

fn bullets(drive: &Drive) -> Vec<(String, Vec<String>)> {
    list_notes(drive.root())
        .unwrap_or_default()
        .iter()
        .filter_map(|p| read_note_at(drive.root(), p).ok().flatten())
        .flat_map(|t| {
            Note::parse(&t)
                .bullets()
                .map(|b| (b.text.clone(), sources(b)))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Runs a dreaming pass (no model) over [`dream_seed`] at `now` and
/// measures what was preserved.
pub async fn dream_preservation_eval(
    dir: &Path,
    now: OffsetDateTime,
) -> Result<Preservation, String> {
    let drive = seed_drive(dir, &dream_seed())?;
    let before = bullets(&drive);
    let co = SessionCheckout::open(&drive, "dream-eval").map_err(|e| e.to_string())?;
    let report = Dreamer::new(DreamSettings::default(), Arc::new(Redactor::default()))
        .with_now(now)
        .dream(&co, &[], false, &CancellationToken::new())
        .await
        .map_err(|e| e.to_string())?;
    let after = bullets(&drive);
    let after_prefs: BTreeSet<String> = after.iter().map(|(t, _)| normalize(t)).collect();
    let after_sources: BTreeSet<&String> = after.iter().flat_map(|(_, s)| s).collect();
    let prefs: Vec<&(String, Vec<String>)> =
        before.iter().filter(|(t, _)| is_preference(t)).collect();
    let pruned: BTreeSet<&str> = report
        .pass
        .removed
        .iter()
        .filter(|r| r.reason != harness_memory::dream::consolidate::RemovalReason::Merged)
        .map(|r| r.text.as_str())
        .collect();
    let kept_sources: Vec<&String> = before
        .iter()
        .filter(|(t, _)| !pruned.contains(t.as_str()))
        .flat_map(|(_, s)| s)
        .collect();
    Ok(Preservation {
        preferences: prefs.len(),
        preferences_kept: prefs
            .iter()
            .filter(|(t, _)| after_prefs.contains(&normalize(t)))
            .count(),
        sources: kept_sources.len(),
        sources_kept: kept_sources
            .iter()
            .filter(|s| after_sources.contains(*s))
            .count(),
        summary: report.summary(),
    })
}
