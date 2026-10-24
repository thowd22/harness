use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use super::*;
use crate::bash::{BashTool, EnvScrubber};
use crate::local::LocalExec;
use crate::spill::MemoryBlobSink;
use crate::tool::RecordingEvents;

struct Fx {
    dir: tempfile::TempDir,
    tool: Option<BashTool>,
    jobs_tool: JobsTool,
    events: Arc<RecordingEvents>,
    ctx: ToolContext,
}

fn fx_with(config: ToolsConfig) -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let events = Arc::new(RecordingEvents::new());
    let spiller = Spiller::new(Arc::new(MemoryBlobSink::new()), &config);
    let tool = BashTool::new(
        Arc::new(LocalExec::new()),
        dir.path(),
        config,
        EnvScrubber::default(),
        spiller.clone(),
    );
    let jobs_tool = JobsTool::new(tool.jobs().clone(), spiller);
    Fx {
        dir,
        tool: Some(tool),
        jobs_tool,
        ctx: ToolContext::new("b1").with_events(events.clone()),
        events,
    }
}

fn fx() -> Fx {
    fx_with(ToolsConfig::default())
}

impl Fx {
    fn tool(&self) -> &BashTool {
        self.tool.as_ref().expect("tool")
    }

    fn jobs(&self) -> JobManager {
        self.tool().jobs().clone()
    }

    async fn bg(&self, command: &str) -> String {
        self.tool()
            .call(
                &self.ctx,
                json!({"command": command, "run_in_background": true}),
            )
            .await
            .unwrap()
            .text()
            .to_string()
    }

    async fn jobs_call(&self, args: serde_json::Value) -> Result<String, ToolError> {
        self.jobs_tool
            .call(&self.ctx, args)
            .await
            .map(|o| o.text().to_string())
    }

    /// Waits until `file` in the project dir holds a full line.
    async fn wait_file(&self, file: &str) -> String {
        let path = self.dir.path().join(file);
        for _ in 0..200 {
            if let Ok(s) = std::fs::read_to_string(&path)
                && s.ends_with('\n')
            {
                return s.trim().to_owned();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("{file} never written");
    }
}

/// Polls until a finished job is handed out (or fails after 20 s).
async fn finished(jobs: &JobManager) -> Vec<(JobInfo, String)> {
    for _ in 0..400 {
        let done = jobs.take_finished();
        if !done.is_empty() {
            return done;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("no job finished: {:?}", jobs.list());
}

async fn assert_dead(pid: &str) {
    let pid: u32 = pid.parse().expect("pid file holds a pid");
    for _ in 0..100 {
        if crate::conformance::local_process_gone(pid) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("process {pid} survived");
}

#[tokio::test]
async fn background_call_returns_at_once_and_the_exit_is_handed_out_once() {
    let f = fx();
    let started = std::time::Instant::now();
    let out = f.bg("sleep 2; echo done; echo oops >&2; exit 3").await;
    assert!(
        started.elapsed() < Duration::from_millis(1800),
        "returned before the command ended: {:?}",
        started.elapsed()
    );
    assert!(out.contains("background job j1 started"), "{out}");
    assert!(out.contains("bash_jobs"), "{out}");
    let log = f.jobs().get("j1").unwrap().output.unwrap();
    assert!(out.contains(&log.display().to_string()), "{out}");
    assert!(f.jobs().get("j1").unwrap().status.is_running());
    assert_eq!(f.jobs().running(), 1);

    let done = finished(&f.jobs()).await;
    assert_eq!(done.len(), 1);
    let (info, tail) = &done[0];
    assert_eq!(info.status, JobStatus::Exited(3));
    assert_eq!(info.status.state(), "failed");
    assert!(info.elapsed >= Duration::from_millis(1900), "{info:?}");
    assert!(tail.contains("done\n") && tail.contains("oops\n"), "{tail}");
    assert_eq!(std::fs::read_to_string(&log).unwrap(), tail.as_str());
    assert!(f.jobs().take_finished().is_empty(), "reported once");
    assert_eq!(f.jobs().running(), 0);

    // Started and ended are reported with the starting call's id.
    let updates: Vec<JobInfo> = f
        .events
        .events()
        .into_iter()
        .filter_map(|(id, e)| match e {
            ToolEvent::Job(j) => {
                assert_eq!(id, "b1");
                Some(j)
            }
            _ => None,
        })
        .collect();
    assert_eq!(updates.len(), 2, "{updates:?}");
    assert_eq!(updates[0].status, JobStatus::Running);
    assert_eq!(
        updates[0].command,
        "sleep 2; echo done; echo oops >&2; exit 3"
    );
    assert_eq!(updates[1].status, JobStatus::Exited(3));
}

#[tokio::test]
async fn output_reads_only_what_is_new() {
    let f = fx();
    f.bg("echo first; echo go > ready; while [ ! -e next ]; do sleep 0.05; done; echo second")
        .await;
    f.wait_file("ready").await;
    // `first` was written before `ready`.
    let out = f
        .jobs_call(json!({"action": "output", "id": "j1"}))
        .await
        .unwrap();
    assert!(out.starts_with("first\n"), "{out}");
    assert!(out.contains("[job j1 running after"), "{out}");
    let out = f
        .jobs_call(json!({"action": "output", "id": "j1"}))
        .await
        .unwrap();
    assert!(out.starts_with("[no new output]"), "{out}");
    std::fs::write(f.dir.path().join("next"), "").unwrap();
    finished(&f.jobs()).await;
    let out = f
        .jobs_call(json!({"action": "output", "id": "j1"}))
        .await
        .unwrap();
    assert!(out.starts_with("second\n"), "{out}");
    assert!(out.contains("[job j1 exit code 0 after"), "{out}");
    let list = f.jobs_call(json!({"action": "list"})).await.unwrap();
    assert!(list.starts_with("j1 exit code 0 "), "{list}");
    assert!(list.contains(" · log "), "{list}");
}

#[tokio::test]
async fn wait_returns_the_exit_and_its_notice_is_not_repeated() {
    let f = fx();
    f.bg("while [ ! -e go ]; do sleep 0.05; done; echo built; exit 3")
        .await;
    // Still running: a short wait says so and the job is not reported.
    let out = f
        .jobs_call(json!({"action": "wait", "id": "j1", "timeout": 1}))
        .await
        .unwrap();
    assert!(out.contains("[job j1 still running after"), "{out}");
    std::fs::write(f.dir.path().join("go"), "").unwrap();
    let out = f
        .jobs_call(json!({"action": "wait", "id": "j1", "timeout": 30}))
        .await
        .unwrap();
    assert!(out.starts_with("built\n"), "{out}");
    assert!(out.contains("[job j1 exit code 3 after"), "{out}");
    // The model saw the exit, so no later notice repeats it.
    assert!(f.jobs().take_finished().is_empty());
    let err = f
        .jobs_call(json!({"action": "wait", "id": "j9"}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("j1"), "{err}");
}

#[tokio::test]
async fn stop_kills_the_whole_process_group() {
    let f = fx();
    f.bg("sleep 60 & echo $! > pid; wait").await;
    let pid = f.wait_file("pid").await;
    let out = f
        .jobs_call(json!({"action": "stop", "id": "j1"}))
        .await
        .unwrap();
    assert!(out.contains("job j1 stopped"), "{out}");
    assert_eq!(f.jobs().get("j1").unwrap().status, JobStatus::Stopped);
    assert_dead(&pid).await;
    let again = f
        .jobs_call(json!({"action": "stop", "id": "j1"}))
        .await
        .unwrap();
    assert!(again.contains("had already ended"), "{again}");
    // A stopped job is still reported once.
    let done = f.jobs().take_finished();
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].0.status, JobStatus::Stopped);
}

#[tokio::test]
async fn shutdown_and_drop_leave_no_process_behind() {
    let mut f = fx();
    f.bg("sleep 60 & echo $! > pid1; wait").await;
    f.bg("sleep 60 & echo $! > pid2; wait").await;
    let p1 = f.wait_file("pid1").await;
    let p2 = f.wait_file("pid2").await;
    f.jobs().shutdown().await;
    assert!(
        f.jobs()
            .list()
            .iter()
            .all(|j| j.status == JobStatus::Stopped),
        "{:?}",
        f.jobs().list()
    );
    assert_dead(&p1).await;
    assert_dead(&p2).await;

    // Dropping the session's tools stops a job that is still running.
    f.bg("sleep 60 & echo $! > pid3; wait").await;
    let p3 = f.wait_file("pid3").await;
    let log_dir = f.jobs().get("j3").unwrap().output.unwrap();
    let log_dir = log_dir.parent().unwrap().to_path_buf();
    drop(f.tool.take());
    // The jobs tool still holds the manager; drop it too.
    drop(f.jobs_tool);
    assert_dead(&p3).await;
    for _ in 0..100 {
        if !log_dir.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!log_dir.exists(), "the session's log directory is removed");
}

/// The `ToolEvent::Job` updates recorded so far.
fn job_events(events: &RecordingEvents) -> Vec<JobInfo> {
    events
        .events()
        .into_iter()
        .filter_map(|(_, e)| match e {
            ToolEvent::Job(j) => Some(j),
            _ => None,
        })
        .collect()
}

/// Records events, taking a while over each job's end (a busy run log).
struct SlowEnds(RecordingEvents);

impl ToolEventSink for SlowEnds {
    fn emit(&self, call_id: &str, event: ToolEvent) {
        if matches!(&event, ToolEvent::Job(j) if !j.status.is_running()) {
            std::thread::sleep(Duration::from_millis(300));
        }
        self.0.emit(call_id, event);
    }
}

// Multi-threaded, as harness runs: the job driver can emit on another
// worker while shutdown resumes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_returns_only_after_every_end_is_recorded() {
    // The run log closes right after shutdown, so an end event emitted
    // after it returns would be lost.
    let mut f = fx();
    let slow = Arc::new(SlowEnds(RecordingEvents::new()));
    f.ctx = ToolContext::new("b1").with_events(slow.clone());
    f.bg("sleep 60 & echo $! > pid; wait").await;
    f.wait_file("pid").await;
    f.jobs().shutdown().await;
    let ends: Vec<JobInfo> = job_events(&slow.0)
        .into_iter()
        .filter(|j| !j.status.is_running())
        .collect();
    assert_eq!(ends.len(), 1, "{ends:?}");
    assert_eq!(ends[0].status, JobStatus::Stopped);
}

#[tokio::test]
async fn dropping_the_manager_still_reports_the_end_of_a_running_job() {
    let mut f = fx();
    f.bg("echo hi; sleep 60 & echo $! > pid; wait").await;
    let pid = f.wait_file("pid").await;
    drop(f.tool.take());
    drop(f.jobs_tool);
    assert_dead(&pid).await;
    let mut updates = job_events(&f.events);
    for _ in 0..200 {
        if updates.len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        updates = job_events(&f.events);
    }
    assert_eq!(updates.len(), 2, "started and ended: {updates:?}");
    let end = &updates[1];
    assert_eq!(end.id, "j1");
    assert_eq!(end.call_id, "b1");
    assert_eq!(end.status, JobStatus::Stopped);
    assert_eq!(end.command, updates[0].command);
    assert!(end.bytes >= 3, "{end:?}");
    assert_eq!(end.output, None, "the log went with the manager");
}

#[tokio::test]
async fn shared_managers_hand_each_agent_its_own_jobs() {
    let dir = tempfile::tempdir().unwrap();
    let config = ToolsConfig::default();
    let spiller = Spiller::new(Arc::new(MemoryBlobSink::new()), &config);
    let session = JobManager::new(JobLimits::from_config(&config));
    let bash = || {
        BashTool::new(
            Arc::new(LocalExec::new()),
            dir.path(),
            config.clone(),
            EnvScrubber::default(),
            spiller.clone(),
        )
        .with_jobs(session.clone())
    };
    let top = bash();
    let child = bash();
    assert!(top.jobs().same_jobs(child.jobs()));
    assert_ne!(top.job_owner(), child.job_owner());
    let ctx = ToolContext::new("b1");
    let bg = |cmd: &str| json!({"command": cmd, "run_in_background": true});
    let out = top.call(&ctx, bg("exit 1")).await.unwrap();
    assert!(out.text().contains("job j1 "), "{}", out.text());
    let out = child.call(&ctx, bg("exit 2")).await.unwrap();
    assert!(
        out.text().contains("job j2 "),
        "ids are unique in the session"
    );
    for _ in 0..400 {
        if session.running() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    // The child hears only about its own job; the session agent does not
    // take it while the child lives.
    let got = session.take_finished_for(top.job_owner(), true);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].0.id, "j1");
    let got = session.take_finished_for(child.job_owner(), false);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].0.id, "j2");
}

#[tokio::test]
async fn orphaned_jobs_are_reported_to_the_session_agent() {
    let dir = tempfile::tempdir().unwrap();
    let config = ToolsConfig::default();
    let spiller = Spiller::new(Arc::new(MemoryBlobSink::new()), &config);
    let session = JobManager::new(JobLimits::from_config(&config));
    let mk = || {
        BashTool::new(
            Arc::new(LocalExec::new()),
            dir.path(),
            config.clone(),
            EnvScrubber::default(),
            spiller.clone(),
        )
        .with_jobs(session.clone())
    };
    let top = mk();
    let child = mk();
    let ctx = ToolContext::new("b1");
    child
        .call(
            &ctx,
            json!({"command": "exit 4", "run_in_background": true}),
        )
        .await
        .unwrap();
    drop(child);
    for _ in 0..400 {
        if session.running() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    // Not the session agent's own job, but its agent is gone.
    assert!(session.take_finished_for(top.job_owner(), false).is_empty());
    let got = session.take_finished_for(top.job_owner(), true);
    assert_eq!(got.len(), 1, "{:?}", session.list());
    assert_eq!(got[0].0.status, JobStatus::Exited(4));
}

#[tokio::test]
async fn a_scoped_handle_shuts_down_only_its_own_jobs() {
    let f = fx();
    let session = f.jobs();
    let scoped = session.scoped();
    assert!(scoped.same_jobs(&session));
    let run = BashTool::new(
        Arc::new(LocalExec::new()),
        f.dir.path(),
        ToolsConfig::default(),
        EnvScrubber::default(),
        Spiller::new(Arc::new(MemoryBlobSink::new()), &ToolsConfig::default()),
    )
    .with_jobs(scoped.clone());
    f.bg("sleep 60 & echo $! > pid1; wait").await;
    run.call(
        &f.ctx,
        json!({"command": "sleep 60 & echo $! > pid2; wait", "run_in_background": true}),
    )
    .await
    .unwrap();
    let p1 = f.wait_file("pid1").await;
    let p2 = f.wait_file("pid2").await;
    assert_eq!(session.list().len(), 2, "the session lists both");
    scoped.shutdown().await;
    assert_dead(&p2).await;
    assert_eq!(session.get("j2").unwrap().status, JobStatus::Stopped);
    assert!(session.get("j1").unwrap().status.is_running());
    session.shutdown().await;
    assert_dead(&p1).await;
}

#[tokio::test]
async fn running_jobs_are_capped() {
    let f = fx_with(ToolsConfig {
        bash_max_jobs: 1,
        ..Default::default()
    });
    f.bg("sleep 60").await;
    let err = f
        .tool()
        .call(
            &f.ctx,
            json!({"command": "true", "run_in_background": true}),
        )
        .await
        .unwrap_err();
    assert!(err.message.contains("limit 1"), "{err}");
    assert!(err.message.contains("bash_max_jobs"), "{err}");
    f.jobs().stop("j1").await.unwrap();
    let out = f.bg("true").await;
    assert!(out.contains("job j2 started"), "{out}");
    f.jobs().shutdown().await;
}

#[tokio::test]
async fn output_over_the_budget_is_counted_but_not_logged() {
    let f = fx_with(ToolsConfig {
        bash_job_output_bytes: 100,
        ..Default::default()
    });
    f.bg("head -c 1000 /dev/zero | tr '\\0' a").await;
    let (info, tail) = finished(&f.jobs()).await.remove(0);
    assert_eq!(info.bytes, 1000);
    assert_eq!(info.dropped, 900);
    assert_eq!(tail.len(), 1000, "the tail is kept in memory");
    let log = std::fs::read_to_string(info.output.unwrap()).unwrap();
    assert!(log.starts_with(&"a".repeat(100)), "{log}");
    assert!(log.contains("output limit of 100 bytes reached"), "{log}");
    assert!(!log.contains(&"a".repeat(101)), "{log}");
}

#[tokio::test]
async fn a_job_timeout_is_honoured_and_its_cd_stays_in_the_job() {
    let f = fx();
    std::fs::create_dir(f.dir.path().join("sub")).unwrap();
    f.tool()
        .call(
            &f.ctx,
            json!({"command": "cd sub; sleep 60", "run_in_background": true, "timeout": 1}),
        )
        .await
        .unwrap();
    let (info, _) = finished(&f.jobs()).await.remove(0);
    assert_eq!(info.status, JobStatus::TimedOut);
    assert_eq!(
        std::fs::canonicalize(f.tool().cwd()).unwrap(),
        std::fs::canonicalize(f.dir.path()).unwrap()
    );
}

#[tokio::test]
async fn jobs_tool_errors_say_how_to_fix_the_call() {
    let f = fx();
    assert_eq!(
        f.jobs_call(json!({"action": "list"})).await.unwrap(),
        "[no background jobs]"
    );
    let err = f.jobs_call(json!({"action": "output"})).await.unwrap_err();
    assert!(err.message.contains("needs an `id`"), "{err}");
    let err = f
        .jobs_call(json!({"action": "stop", "id": "j9"}))
        .await
        .unwrap_err();
    assert_eq!(err.kind, ToolErrorKind::NotFound);
    assert!(err.message.contains("no background jobs"), "{err}");
    let err = f
        .jobs_call(json!({"action": "pause", "id": "j1"}))
        .await
        .unwrap_err();
    assert!(err.message.contains("list, output, wait or stop"), "{err}");
}

#[test]
fn durations_and_states_read_well() {
    assert_eq!(fmt_duration(Duration::from_millis(400)), "0.4s");
    assert_eq!(fmt_duration(Duration::from_secs(45)), "45s");
    assert_eq!(fmt_duration(Duration::from_secs(192)), "3m12s");
    assert_eq!(fmt_duration(Duration::from_secs(3720)), "1h02m");
    assert_eq!(JobStatus::Exited(0).state(), "exited");
    assert_eq!(JobStatus::Signaled(9).describe(), "killed by signal 9");
    assert_eq!(JobStatus::Exited(2).exit_code(), Some(2));
    assert_eq!(one_line("a\n  b", 10), "a b");
    assert_eq!(one_line("abcdefghijk", 5), "abcd…");
}
