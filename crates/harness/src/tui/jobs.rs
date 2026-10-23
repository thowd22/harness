//! `/jobs`: the session's background bash jobs (`bash` with
//! `run_in_background`).
//!
//! The panel lists every job with its state, elapsed time, command and log
//! file. Its buttons (mouse or Tab/arrows + Enter, like every panel) open a
//! job's latest output, stop a running job (its whole process group) or
//! refresh the list. The status line's `⚙ N jobs` indicator opens it.

use harness_tools::jobs::{JobInfo, JobManager, fmt_duration};
use harness_tui::app::CoreEvent;
use harness_tui::notify::Level;
use harness_tui::panel::PanelButton;

use super::Driver;

/// Title of the `/jobs` panel.
pub(super) const TITLE: &str = "Background jobs";
/// Button prefix: show a job's output (`jobs.output:j1`).
const OUTPUT: &str = "jobs.output:";
/// Button prefix: stop a job (`jobs.stop:j1`).
const STOP: &str = "jobs.stop:";
/// Button: list the jobs again.
const REFRESH: &str = "jobs.refresh";
/// Bytes of a log shown in the output view.
const VIEW_BYTES: u64 = 64 * 1024;
/// Lines of a log shown in the output view.
const VIEW_LINES: usize = 200;

/// True for the `/jobs` panel's buttons.
pub(super) fn is_action(id: &str) -> bool {
    id.starts_with("jobs.")
}

fn job_line(j: &JobInfo) -> String {
    format!(
        "{:<4} {:<22} {:>7}  {}",
        j.id,
        j.status.describe(),
        fmt_duration(j.elapsed),
        j.command.split_whitespace().collect::<Vec<_>>().join(" ")
    )
}

fn size(bytes: u64) -> String {
    match bytes {
        b if b >= 1024 * 1024 => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
        b if b >= 1024 => format!("{:.1} KB", b as f64 / 1024.0),
        b => format!("{b} B"),
    }
}

/// The list panel: lines and buttons.
pub(super) fn list_panel(jobs: Option<&JobManager>) -> (Vec<String>, Vec<PanelButton>) {
    let list = jobs.map(JobManager::list).unwrap_or_default();
    if list.is_empty() {
        return (
            vec![
                "No background jobs in this session.".into(),
                "The agent starts one with bash run_in_background; it keeps running".into(),
                "after the call, and its exit is reported to the agent.".into(),
            ],
            vec![PanelButton::new(REFRESH, "refresh")],
        );
    }
    let running = list.iter().filter(|j| j.status.is_running()).count();
    let mut lines = vec![
        format!("{running} running · {} ended", list.len() - running),
        String::new(),
    ];
    let mut buttons = Vec::new();
    for j in &list {
        lines.push(job_line(j));
        if let Some(p) = &j.output {
            lines.push(format!("     log {} ({})", p.display(), size(j.bytes)));
        }
        buttons.push(PanelButton::new(
            format!("{OUTPUT}{}", j.id),
            format!("output {}", j.id),
        ));
        if j.status.is_running() {
            buttons.push(PanelButton::new(
                format!("{STOP}{}", j.id),
                format!("stop {}", j.id),
            ));
        }
    }
    buttons.push(PanelButton::new(REFRESH, "refresh"));
    (lines, buttons)
}

/// The output view of one job: its line, then the end of its log.
pub(super) async fn output_panel(
    jobs: &JobManager,
    id: &str,
) -> Option<(Vec<String>, Vec<PanelButton>)> {
    let info = jobs.get(id)?;
    let text = jobs.log_tail(id, VIEW_BYTES).await.unwrap_or_default();
    let mut lines = vec![job_line(&info)];
    if let Some(p) = &info.output {
        lines.push(format!("log {} ({})", p.display(), size(info.bytes)));
    }
    lines.push(String::new());
    let all: Vec<&str> = text.lines().collect();
    if all.is_empty() {
        lines.push("(no output yet)".into());
    }
    let skip = all.len().saturating_sub(VIEW_LINES);
    if skip > 0 || info.bytes > VIEW_BYTES {
        lines.push(format!(
            "[... earlier output in the log; last {} lines]",
            all.len() - skip
        ));
    }
    lines.extend(all[skip..].iter().map(|l| (*l).to_owned()));
    let mut buttons = vec![PanelButton::new(format!("{OUTPUT}{id}"), "refresh")];
    if info.status.is_running() {
        buttons.push(PanelButton::new(
            format!("{STOP}{id}"),
            format!("stop {id}"),
        ));
    }
    buttons.push(PanelButton::new(REFRESH, "all jobs"));
    Some((lines, buttons))
}

fn jobs(d: &Driver) -> Option<JobManager> {
    d.h.as_ref().and_then(|h| h.jobs.clone())
}

/// Opens the `/jobs` panel.
pub(super) async fn open(d: &mut Driver) {
    let (lines, buttons) = list_panel(jobs(d).as_ref());
    d.send(CoreEvent::Panel {
        title: TITLE.into(),
        lines,
        buttons,
    })
    .await;
}

/// A `/jobs` panel button.
pub(super) async fn action(d: &mut Driver, id: &str) {
    let Some(manager) = jobs(d) else {
        return open(d).await;
    };
    if let Some(job) = id.strip_prefix(OUTPUT) {
        let Some((lines, buttons)) = output_panel(&manager, job).await else {
            return open(d).await;
        };
        let lines = lines
            .into_iter()
            .map(|l| d.redactor.redact(&l).into_owned())
            .collect();
        return d
            .send(CoreEvent::Panel {
                title: format!("Job {job}"),
                lines,
                buttons,
            })
            .await;
    }
    if let Some(job) = id.strip_prefix(STOP) {
        match manager.stop(job).await {
            Ok(info) => {
                d.notify(
                    Level::Info,
                    &format!("stopped {}", info.id),
                    Some(format!(
                        "{} after {}",
                        info.status.describe(),
                        fmt_duration(info.elapsed)
                    )),
                )
                .await
            }
            Err(e) => {
                d.notify(Level::Warning, "cannot stop", Some(e.message))
                    .await
            }
        }
    }
    open(d).await
}
