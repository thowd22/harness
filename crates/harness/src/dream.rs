//! Dreaming: memory consolidation passes.
//!
//! A pass runs in its own `dream` checkout of the memory drive, with the
//! settings of the `dream` workflow's step (`with:`), and adds lessons from
//! recent runs with `[memory] dream_model`. It runs:
//!
//! - at TUI start when [`due`] says so (`[memory] dream`, about daily when
//!   there was activity since the last pass),
//! - on `/memory dream` (always), and
//! - as the `dream` step of a workflow (`/dream`, `harness wf run dream`).

use std::sync::Arc;

use async_trait::async_trait;
use harness_config::discovery::Catalog;
use harness_config::ini::Config;
use harness_config::paths::DataDir;
use harness_events::redact::Redactor;
use harness_memory::dream::schedule::{DreamLog, Due};
use harness_memory::dream::{DreamReport, DreamSettings, Dreamer};
use harness_memory::drive::Drive;
use harness_memory::sync::SessionCheckout;
use harness_providers::provider::Provider;
use harness_workflow::executor::{StepAction, StepFailure, StepOutput, StepRequest, StepRunner};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

/// The checkout session dreaming works in.
pub const SESSION: &str = "dream";

/// The `dream` workflow's settings (built-in defaults when it is missing).
pub fn settings(catalog: &Catalog) -> Result<DreamSettings, String> {
    match catalog
        .workflows
        .get(harness_config::builtins::DREAM_WORKFLOW)
    {
        Some(w) => DreamSettings::from_workflow(&w.document),
        None => Ok(DreamSettings::default()),
    }
}

/// Whether an automatic pass is due now.
pub fn due(cfg: &Config, data: &DataDir, settings: &DreamSettings) -> Result<Due, String> {
    let drive = Drive::from_config(cfg).map_err(|e| e.to_string())?;
    let log = DreamLog::new(&drive.internal_dir());
    Ok(harness_memory::dream::schedule::due(
        cfg.memory.dream,
        false,
        log.last().map(|e| e.at),
        harness_memory::dream::runs::last_activity(data.root()),
        time::OffsetDateTime::now_utc(),
        settings.interval(),
    ))
}

/// The provider and model id for lessons: `[memory] dream_model`, or the
/// fake provider in scripted sessions. `None` when no key is configured
/// (the pass then only merges, prunes and reindexes).
pub fn lesson_model(
    cfg: &Config,
    fake: Option<Arc<dyn Provider>>,
) -> Option<(Arc<dyn Provider>, String)> {
    if let Some(f) = fake {
        return Some((f, cfg.memory.dream_model.clone()));
    }
    let registry = crate::wiring::registry(cfg).ok()?;
    let resolved = registry.resolve(&cfg.memory.dream_model).ok()?;
    let provider = crate::wiring::provider_sync(cfg, &registry, &resolved)
        .map_err(
            |e| tracing::info!(target: "harness::dream", error = %e, "dreaming without lessons"),
        )
        .ok()?;
    Some((provider, resolved.reference.model.clone()))
}

/// One consolidation pass.
pub struct Pass<'a> {
    /// harness.ini (the drive).
    pub cfg: &'a Config,
    /// Data directory (recent runs).
    pub data: &'a DataDir,
    /// The dream workflow's settings.
    pub settings: DreamSettings,
    /// Secret scanner for lessons.
    pub redactor: Arc<Redactor>,
    /// Provider and model for lessons (`None`: no new lessons).
    pub lessons: Option<(Arc<dyn Provider>, String)>,
    /// Project whose notes `project/<topic>` lessons go to.
    pub project: Option<&'a std::path::Path>,
    /// Asked for (`/memory dream`) rather than scheduled.
    pub manual: bool,
}

/// Runs one pass in the `dream` checkout; the result is one commit.
pub async fn run_pass(pass: Pass<'_>, cancel: &CancellationToken) -> Result<DreamReport, String> {
    let drive = Drive::from_config(pass.cfg).map_err(|e| e.to_string())?;
    let checkout = SessionCheckout::open(&drive, SESSION).map_err(|e| e.to_string())?;
    let mut dreamer = Dreamer::new(pass.settings, pass.redactor.clone());
    if let Some((provider, model)) = pass.lessons {
        dreamer = dreamer.with_lessons(harness_memory::extract::Extractor::new(
            provider,
            harness_memory::extract::ExtractConfig::new(model),
            pass.redactor,
        ));
    }
    if let Some(p) = pass.project {
        dreamer = dreamer.with_project_key(harness_memory::drive::project::project_key(p).key);
    }
    let runs = dreamer.select_runs(pass.data.root(), &DreamLog::new(&drive.internal_dir()));
    dreamer
        .dream(&checkout, &runs, pass.manual, cancel)
        .await
        .map_err(|e| e.to_string())
}

/// The `dream` workflow step.
pub struct DreamStepRunner {
    cfg: Config,
    data: DataDir,
    redactor: Arc<Redactor>,
    fake: Option<Arc<dyn Provider>>,
    headless: bool,
}

impl DreamStepRunner {
    /// Passes for `cfg`'s drive; lessons from `fake` when given. Headless
    /// runs need `[memory] headless_access`.
    pub fn new(
        cfg: Config,
        data: DataDir,
        redactor: Arc<Redactor>,
        fake: Option<Arc<dyn Provider>>,
        headless: bool,
    ) -> Self {
        Self {
            cfg,
            data,
            redactor,
            fake,
            headless,
        }
    }
}

#[async_trait]
impl StepRunner for DreamStepRunner {
    async fn run(&self, req: StepRequest) -> Result<StepOutput, StepFailure> {
        let StepAction::Dream { settings } = &req.action else {
            return Err(StepFailure::new("internal: not a dream step"));
        };
        if !self.cfg.memory.enabled {
            return Err(StepFailure::new(
                "memory is off ([memory] enabled = false); nothing to consolidate",
            ));
        }
        if self.headless && !self.cfg.memory.headless_access {
            return Err(StepFailure::new(
                "headless runs do not access memory; set [memory] headless_access = true to dream headlessly",
            ));
        }
        let settings: DreamSettings = serde_json::from_value(Value::Object(settings.clone()))
            .map_err(|e| StepFailure::new(format!("dream settings: {e}")))?;
        let report = run_pass(
            Pass {
                cfg: &self.cfg,
                data: &self.data,
                settings,
                redactor: self.redactor.clone(),
                lessons: lesson_model(&self.cfg, self.fake.clone()),
                project: None,
                manual: true,
            },
            &req.cancel,
        )
        .await
        .map_err(StepFailure::new)?;
        let mut outputs = serde_json::Map::new();
        outputs.insert("summary".into(), json!(report.summary()));
        outputs.insert(
            "commit".into(),
            json!(report.commit.as_ref().map(|c| c.commit.clone())),
        );
        outputs.insert("added".into(), json!(report.added.len()));
        use harness_memory::dream::consolidate::RemovalReason;
        outputs.insert(
            "merged".into(),
            json!(report.pass.count(RemovalReason::Merged)),
        );
        outputs.insert(
            "pruned".into(),
            json!(
                report.pass.count(RemovalReason::Stale)
                    + report.pass.count(RemovalReason::Transient)
            ),
        );
        Ok(StepOutput {
            outputs,
            ..StepOutput::default()
        })
    }
}
