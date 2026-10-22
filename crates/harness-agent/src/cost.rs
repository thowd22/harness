//! Token and dollar accounting per call, node, run, session and day.
//!
//! Everything is derived from the run event log, so live views, `/cost`,
//! resumed sessions and the daily budget all agree:
//!
//! - `model_call_started` opens a [`CallRecord`] (provider, model, agent
//!   from the node's `node_started`),
//! - `model_call_finished` fills in the provider-reported [`Usage`],
//! - the `cost` event that follows it on the same node sets the price.
//!
//! A [`UsageLedger`] folds events of one or more runs (a session is the
//! list of its runs) and groups them with [`UsageLedger::by_node`],
//! [`UsageLedger::by_run`], [`UsageLedger::by_day`],
//! [`UsageLedger::by_model`] and [`UsageLedger::by_agent`].
//! [`daily_spend`] reads every run of the data dir for one UTC day.
//! [`CostReport`] is the `/cost` breakdown.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use harness_events::schema::{EventKind, RunEvent, RunId, Usage};
use harness_events::store::{StoreError, list_runs, read_run, run_dir};
use time::{Date, OffsetDateTime};

/// One model call as recorded in the event log.
#[derive(Debug, Clone, PartialEq)]
pub struct CallRecord {
    /// The run.
    pub run: RunId,
    /// The node that made the call (`None` for run-level calls).
    pub node: Option<String>,
    /// Display name of the node (its `node_started.name`).
    pub node_name: Option<String>,
    /// Agent of the node.
    pub agent: Option<String>,
    /// Call id.
    pub call_id: String,
    /// Provider name.
    pub provider: String,
    /// Model id.
    pub model: String,
    /// Provider-reported usage (zero until `model_call_finished`).
    pub usage: Usage,
    /// Known cost in USD; `None` = unknown (never treated as zero).
    pub cost_usd: Option<f64>,
    /// When the call started.
    pub started_at: OffsetDateTime,
    /// Duration once finished.
    pub duration_ms: Option<u64>,
    /// True once `model_call_finished` was seen.
    pub finished: bool,
}

impl CallRecord {
    /// Tokens of the request context: uncached plus cached input.
    pub fn context_tokens(&self) -> u64 {
        self.usage.input + self.usage.cached_input
    }

    /// Share of input tokens served from the prompt cache, `None` when the
    /// call had no input.
    pub fn cache_hit_rate(&self) -> Option<f64> {
        cache_hit_rate(&self.usage)
    }

    /// The UTC day the call started.
    pub fn day(&self) -> Date {
        self.started_at.to_offset(time::UtcOffset::UTC).date()
    }
}

/// `cached_input / (input + cached_input)`, `None` with no input.
pub fn cache_hit_rate(usage: &Usage) -> Option<f64> {
    let total = usage.input + usage.cached_input;
    (total > 0).then(|| usage.cached_input as f64 / total as f64)
}

/// Summed usage and cost of a group of calls.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Totals {
    /// Calls in the group.
    pub calls: u64,
    /// Summed usage.
    pub usage: Usage,
    /// Summed known cost in USD.
    pub cost_usd: f64,
    /// Calls whose cost is unknown.
    pub unknown_cost_calls: u64,
}

impl Totals {
    /// Adds one call.
    pub fn add(&mut self, call: &CallRecord) {
        self.calls += 1;
        self.usage.add(&call.usage);
        match call.cost_usd {
            Some(usd) => self.cost_usd += usd,
            None if call.finished => self.unknown_cost_calls += 1,
            None => {}
        }
    }

    /// Input (uncached + cached) plus output tokens.
    pub fn tokens(&self) -> u64 {
        self.usage.input + self.usage.cached_input + self.usage.output
    }

    /// Cache hit rate over the group.
    pub fn cache_hit_rate(&self) -> Option<f64> {
        cache_hit_rate(&self.usage)
    }

    /// True when some call's cost is unknown.
    pub fn cost_unknown(&self) -> bool {
        self.unknown_cost_calls > 0
    }

    /// `$1.2345`, `$1.2345+?` when some cost is unknown, `$?` when nothing
    /// is known.
    pub fn cost_label(&self) -> String {
        match (self.cost_unknown(), self.cost_usd) {
            (true, 0.0) => "$?".to_owned(),
            (true, c) => format!("${c:.4}+?"),
            (false, c) => format!("${c:.4}"),
        }
    }
}

/// Context use of one node, from its latest call.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeContext {
    /// Node id.
    pub node: String,
    /// Context tokens of the latest call.
    pub last_tokens: u64,
    /// Largest context of any call of the node.
    pub peak_tokens: u64,
    /// Calls of the node.
    pub calls: u64,
}

impl NodeContext {
    /// Fraction of `window` used by the latest call.
    pub fn fraction(&self, window: u32) -> Option<f64> {
        (window > 0).then(|| self.last_tokens as f64 / f64::from(window))
    }
}

#[derive(Debug, Clone, Default)]
struct NodeInfo {
    name: Option<String>,
    agent: Option<String>,
}

/// Folds `model_call_*` and `cost` events into [`CallRecord`]s.
#[derive(Debug, Clone, Default)]
pub struct UsageLedger {
    calls: Vec<CallRecord>,
    open: HashMap<(String, String), usize>,
    last_finished: HashMap<(String, Option<String>), usize>,
    nodes: HashMap<(String, String), NodeInfo>,
}

impl UsageLedger {
    /// An empty ledger.
    pub fn new() -> Self {
        Self::default()
    }

    /// A ledger of `events`.
    pub fn from_events<'a>(events: impl IntoIterator<Item = &'a RunEvent>) -> Self {
        let mut ledger = Self::new();
        for e in events {
            ledger.apply(e);
        }
        ledger
    }

    /// A ledger of the given runs under `data_dir` (e.g. a session's runs).
    /// Missing runs are skipped.
    pub fn from_runs(data_dir: &Path, runs: &[RunId]) -> Result<Self, StoreError> {
        let mut ledger = Self::new();
        for run in runs {
            ledger.add_run(&run_dir(data_dir, *run))?;
        }
        Ok(ledger)
    }

    /// Adds every event of the run stored in `dir`. A missing run is
    /// skipped.
    pub fn add_run(&mut self, dir: &Path) -> Result<(), StoreError> {
        match read_run(dir) {
            Ok(replay) => {
                for e in &replay.events {
                    self.apply(e);
                }
                Ok(())
            }
            Err(StoreError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Folds one event.
    pub fn apply(&mut self, event: &RunEvent) {
        let run = event.run_id.to_string();
        let node = event.node_id.as_ref().map(|n| n.as_str().to_owned());
        match &event.kind {
            EventKind::NodeStarted { name, agent, .. } => {
                if let Some(n) = &node {
                    self.nodes.insert(
                        (run, n.clone()),
                        NodeInfo {
                            name: Some(name.clone()),
                            agent: agent.clone(),
                        },
                    );
                }
            }
            EventKind::ModelCallStarted {
                call_id,
                provider,
                model,
                ..
            } => {
                let info = node
                    .as_ref()
                    .and_then(|n| self.nodes.get(&(run.clone(), n.clone())))
                    .cloned()
                    .unwrap_or_default();
                self.open
                    .insert((run.clone(), call_id.clone()), self.calls.len());
                self.calls.push(CallRecord {
                    run: event.run_id,
                    node,
                    node_name: info.name,
                    agent: info.agent,
                    call_id: call_id.clone(),
                    provider: provider.clone(),
                    model: model.clone(),
                    usage: Usage::default(),
                    cost_usd: None,
                    started_at: event.ts,
                    duration_ms: None,
                    finished: false,
                });
            }
            EventKind::ModelCallFinished {
                call_id,
                usage,
                duration_ms,
                ..
            } => {
                if let Some(i) = self.open.remove(&(run.clone(), call_id.clone())) {
                    let call = &mut self.calls[i];
                    call.usage = *usage;
                    call.duration_ms = Some(*duration_ms);
                    call.finished = true;
                    self.last_finished.insert((run, node), i);
                }
            }
            EventKind::Cost { usd } => {
                if let Some(i) = self.last_finished.remove(&(run, node)) {
                    let call = &mut self.calls[i];
                    *call.cost_usd.get_or_insert(0.0) += usd;
                }
            }
            _ => {}
        }
    }

    /// Every call, in log order.
    pub fn calls(&self) -> &[CallRecord] {
        &self.calls
    }

    /// Totals over every call.
    pub fn total(&self) -> Totals {
        let mut t = Totals::default();
        for c in &self.calls {
            t.add(c);
        }
        t
    }

    fn group<K: Ord>(&self, key: impl Fn(&CallRecord) -> K) -> BTreeMap<K, Totals> {
        let mut out: BTreeMap<K, Totals> = BTreeMap::new();
        for c in &self.calls {
            out.entry(key(c)).or_default().add(c);
        }
        out
    }

    /// Totals per node id (`""` for run-level calls).
    pub fn by_node(&self) -> BTreeMap<String, Totals> {
        self.group(|c| c.node.clone().unwrap_or_default())
    }

    /// Totals per run id.
    pub fn by_run(&self) -> BTreeMap<String, Totals> {
        self.group(|c| c.run.to_string())
    }

    /// Totals per UTC day.
    pub fn by_day(&self) -> BTreeMap<Date, Totals> {
        self.group(CallRecord::day)
    }

    /// Totals per `provider/model`.
    pub fn by_model(&self) -> BTreeMap<String, Totals> {
        self.group(|c| format!("{}/{}", c.provider, c.model))
    }

    /// Totals per agent (`"-"` when the node has no agent).
    pub fn by_agent(&self) -> BTreeMap<String, Totals> {
        self.group(|c| c.agent.clone().unwrap_or_else(|| "-".into()))
    }

    /// Totals of the calls on `day`.
    pub fn day_total(&self, day: Date) -> Totals {
        self.by_day().remove(&day).unwrap_or_default()
    }

    /// Context use per node, from the provider-reported input of each call.
    pub fn context_by_node(&self) -> BTreeMap<String, NodeContext> {
        let mut out: BTreeMap<String, NodeContext> = BTreeMap::new();
        for c in self.calls.iter().filter(|c| c.finished) {
            let key = c.node.clone().unwrap_or_default();
            let entry = out.entry(key.clone()).or_insert(NodeContext {
                node: key,
                last_tokens: 0,
                peak_tokens: 0,
                calls: 0,
            });
            entry.last_tokens = c.context_tokens();
            entry.peak_tokens = entry.peak_tokens.max(c.context_tokens());
            entry.calls += 1;
        }
        out
    }

    /// Display name per node id.
    fn node_labels(&self) -> BTreeMap<String, String> {
        self.calls
            .iter()
            .filter_map(|c| {
                let id = c.node.clone()?;
                let name = c.node_name.clone().unwrap_or_else(|| id.clone());
                Some((id, name))
            })
            .collect()
    }
}

/// Spend of every run under `data_dir` on the UTC day `day`. Runs are
/// pre-filtered by the timestamp in their v7 id (runs started the day
/// before are included, since a run can cross midnight).
pub fn daily_spend(data_dir: &Path, day: Date) -> Result<Totals, StoreError> {
    let mut ledger = UsageLedger::new();
    for run in list_runs(data_dir)? {
        if let Some(started) = run_started(run)
            && (started > day || started.next_day().is_some_and(|d| d < day))
        {
            continue;
        }
        ledger.add_run(&run_dir(data_dir, run))?;
    }
    Ok(ledger.day_total(day))
}

/// UTC day encoded in a v7 run id.
fn run_started(run: RunId) -> Option<Date> {
    let ts = run.0.get_timestamp()?;
    let (secs, _) = ts.to_unix();
    let secs = i64::try_from(secs).ok()?;
    Some(OffsetDateTime::from_unix_timestamp(secs).ok()?.date())
}

/// What `/cost` groups by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CostGroup {
    /// Per `provider/model`.
    Model,
    /// Per agent.
    Agent,
    /// Per node.
    Node,
}

impl CostGroup {
    /// Every grouping, in `/cost` order.
    pub const ALL: [CostGroup; 3] = [CostGroup::Model, CostGroup::Agent, CostGroup::Node];

    /// `model`, `agent` or `node`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Agent => "agent",
            Self::Node => "node",
        }
    }

    /// Parses `model`, `agent` or `node`.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|g| g.as_str() == s)
    }
}

/// One row of a [`CostReport`].
#[derive(Debug, Clone, PartialEq)]
pub struct CostRow {
    /// Model, agent or node label.
    pub label: String,
    /// Its totals.
    pub totals: Totals,
}

/// The `/cost` breakdown: totals plus rows by model, agent and node,
/// most expensive first.
#[derive(Debug, Clone, PartialEq)]
pub struct CostReport {
    /// Totals over everything.
    pub total: Totals,
    /// Rows per `provider/model`.
    pub by_model: Vec<CostRow>,
    /// Rows per agent.
    pub by_agent: Vec<CostRow>,
    /// Rows per node (labelled by node name).
    pub by_node: Vec<CostRow>,
}

fn rows(map: BTreeMap<String, Totals>, labels: Option<&BTreeMap<String, String>>) -> Vec<CostRow> {
    let mut rows: Vec<CostRow> = map
        .into_iter()
        .map(|(k, totals)| {
            let label = match labels.and_then(|l| l.get(&k)) {
                Some(name) if *name != k => format!("{name} ({})", short_id(&k)),
                _ if k.is_empty() => "(run)".to_owned(),
                _ => k,
            };
            CostRow { label, totals }
        })
        .collect();
    rows.sort_by(|a, b| {
        b.totals
            .cost_usd
            .total_cmp(&a.totals.cost_usd)
            .then(b.totals.tokens().cmp(&a.totals.tokens()))
            .then(a.label.cmp(&b.label))
    });
    rows
}

fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

impl CostReport {
    /// The report of `ledger`.
    pub fn new(ledger: &UsageLedger) -> Self {
        let labels = ledger.node_labels();
        Self {
            total: ledger.total(),
            by_model: rows(ledger.by_model(), None),
            by_agent: rows(ledger.by_agent(), None),
            by_node: rows(ledger.by_node(), Some(&labels)),
        }
    }

    /// Rows for `group`.
    pub fn rows(&self, group: CostGroup) -> &[CostRow] {
        match group {
            CostGroup::Model => &self.by_model,
            CostGroup::Agent => &self.by_agent,
            CostGroup::Node => &self.by_node,
        }
    }

    /// Plain-text lines: a total line, then one section per group (or only
    /// `only`).
    pub fn lines(&self, only: Option<CostGroup>) -> Vec<String> {
        let mut out = vec![format!(
            "total: {} · {} tokens · {} calls{}",
            self.total.cost_label(),
            self.total.tokens(),
            self.total.calls,
            hit_suffix(&self.total)
        )];
        for group in CostGroup::ALL {
            if only.is_some_and(|g| g != group) {
                continue;
            }
            out.push(format!("by {}:", group.as_str()));
            for row in self.rows(group) {
                out.push(format!(
                    "  {:<32} {:>12} {:>10} tok {:>4} calls{}",
                    row.label,
                    row.totals.cost_label(),
                    row.totals.tokens(),
                    row.totals.calls,
                    hit_suffix(&row.totals)
                ));
            }
        }
        out
    }
}

fn hit_suffix(t: &Totals) -> String {
    match t.cache_hit_rate() {
        Some(r) => format!(" · cache {:.0}%", r * 100.0),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests;
