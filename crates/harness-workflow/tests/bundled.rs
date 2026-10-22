//! Runs each bundled workflow's eval task in-process: built-in agents with
//! real read/write/edit/bash tools, the task's scripted fake provider,
//! shadow-git checkpoints for the bug-fix proof, then the task's checks.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use harness_agent::dispatch::{AllowAll, StandardTools, ToolSet};
use harness_agent::scheduler::{Scheduler, SchedulerConfig};
use harness_agent::subagents::{CatalogChildBuilder, SubagentLimits, SubagentRuntime};
use harness_agent::todo::{TaskBoard, TaskStatus};
use harness_config::builtins::BUILTINS;
use harness_config::discovery::{Catalog, Roots};
use harness_config::prompts::PromptVars;
use harness_eval::suite::{Check, Expect, Suite};
use harness_events::redact::Redactor;
use harness_events::schema::RunId;
use harness_events::store::{RecorderOptions, RunRecorder};
use harness_hygiene::checkpoints::CheckpointStore;
use harness_providers::fake::{FakeProvider, FakeScript};
use harness_providers::provider::Provider;
use harness_tools::bash::EnvScrubber;
use harness_tools::config::ToolsConfig;
use harness_tools::local::{LocalExec, LocalWorkspace};
use harness_tools::spill::MemoryBlobSink;
use harness_workflow::bundled::{NAMES, with_builtins};
use harness_workflow::executor::{Executor, Runners};
use harness_workflow::gates::HeadlessGates;
use harness_workflow::rerun::WorkflowStatus;
use harness_workflow::runners::{
    AgentStepRunner, ProofStepRunner, ShellStepRunner, StoreCheckpointer, VerifyStepRunner,
};
use harness_workflow::schema::{CheckContext, from_entry};
use tokio_util::sync::CancellationToken;

fn suite_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("evals/workflows/suite.yaml")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dest = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dest);
        } else {
            std::fs::copy(e.path(), dest).unwrap();
        }
    }
}

fn sh(cwd: &Path, cmd: &str) -> bool {
    std::process::Command::new("sh")
        .args(["-c", cmd])
        .current_dir(cwd)
        .output()
        .unwrap()
        .status
        .success()
}

#[test]
fn every_bundled_workflow_has_an_eval_task() {
    let suite = Suite::load(&suite_path()).unwrap();
    for name in NAMES {
        let task = suite
            .tasks
            .iter()
            .find(|t| t.workflow.as_ref().is_some_and(|w| w.name == *name))
            .unwrap_or_else(|| panic!("no eval task for bundled workflow `{name}`"));
        let script = suite.resolve_path(task.provider.as_ref().unwrap().fake.as_ref().unwrap());
        FakeScript::from_yaml_file(&script).unwrap();
        assert!(!task.checks.is_empty());
    }
}

#[tokio::test]
async fn bundled_workflows_pass_their_eval_tasks() {
    let suite = Suite::load(&suite_path()).unwrap();
    let catalog = Arc::new(Catalog::load(&Roots::new(with_builtins(BUILTINS))));
    let ctx = CheckContext::from_catalog(&catalog, None);
    for task in &suite.tasks {
        let spec = task.workflow.clone().unwrap();
        let wf = from_entry(catalog.workflows.get_entry(&spec.name).unwrap(), &ctx).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        copy_dir(&suite.resolve_path(task.fixture.as_ref().unwrap()), &work);
        for (path, text) in &task.files {
            let p = work.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        let script = suite.resolve_path(task.provider.as_ref().unwrap().fake.as_ref().unwrap());
        let fake = FakeProvider::new(FakeScript::from_yaml_file(&script).unwrap());

        let recorder = RunRecorder::create(
            &dir.path().join("data"),
            RunId::new(),
            Arc::new(Redactor::default()),
            RecorderOptions::default(),
        )
        .await
        .unwrap();
        let exec = Arc::new(LocalExec::new());
        let root = work.clone();
        let tools = {
            let exec = exec.clone();
            Arc::new(move |_: &harness_config::agents::Agent| {
                ToolSet::standard(StandardTools {
                    workspace: Arc::new(LocalWorkspace::new(root.clone())),
                    exec: exec.clone(),
                    cwd: root.clone(),
                    config: ToolsConfig::default(),
                    scrubber: EnvScrubber::new(Vec::new(), Vec::new()),
                    spill: Arc::new(MemoryBlobSink::new()),
                })
            })
        };
        let provider: Arc<dyn Provider> = Arc::new(fake.clone());
        let builder = CatalogChildBuilder::new(
            catalog.clone(),
            PromptVars::detect(&work),
            "fake-model",
            Arc::new(move |_| Ok((provider.clone(), "fake-model".to_owned()))),
            tools,
            Arc::new(AllowAll),
        )
        .with_root(&work);
        let scheduler = Scheduler::new(SchedulerConfig::default());
        let board = Arc::new(TaskBoard::new());
        let runtime = SubagentRuntime::new(
            recorder.clone(),
            Arc::new(builder),
            SubagentLimits::default(),
        )
        .with_scheduler(scheduler.clone())
        .with_board(board.clone());
        let store =
            Arc::new(CheckpointStore::open(&dir.path().join("checkpoints"), &work).unwrap());
        let verify = VerifyStepRunner::new(exec.clone(), &work, recorder.clone());
        let runners = Runners::new(Arc::new(HeadlessGates::from_flags(true, [], [])))
            .with_agent(Arc::new(AgentStepRunner::new(runtime)))
            .with_shell(Arc::new(ShellStepRunner::new(exec.clone(), &work)))
            .with_verify(Arc::new(verify.clone()))
            .with_proof(Arc::new(ProofStepRunner::new(verify, store.clone())))
            .with_checkpoints(Arc::new(StoreCheckpointer(store)));
        let executor = Executor::new(recorder, runners, scheduler).with_board(board.clone());
        let inputs = spec.inputs.clone().into_iter().collect();
        let out = executor
            .run(&wf, &inputs, None, &CancellationToken::new())
            .await;
        assert_eq!(
            out.status,
            WorkflowStatus::Succeeded,
            "{}: {:?}\n{:#?}",
            task.id,
            out.error,
            out.state.steps
        );
        assert_eq!(
            fake.remaining(),
            0,
            "{}: the whole script was used",
            task.id
        );
        let snap = board.snapshot();
        assert!(
            snap.tasks.iter().all(|t| t.status == TaskStatus::Done),
            "{}: {:?}",
            task.id,
            snap.tasks
        );
        for check in &task.checks {
            if let Check::Command(c) = check {
                assert_eq!(
                    sh(&work, &c.command),
                    c.expect == Expect::Pass,
                    "{}: {check}",
                    task.id
                );
            }
        }
        if spec.name == "fix-bug" {
            assert_eq!(out.state.steps["proof"].outputs["proven"], true);
        }
    }
}
