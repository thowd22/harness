//! Cheap liveness of a running process group: CPU time and process counts.
//!
//! A long, quiet command (a `cargo test` that prints nothing for minutes)
//! looks the same as a hung one if all you watch is its output. A
//! [`Liveness`] sample tells them apart: the group's accumulated CPU time
//! advances while anything in it computes, and [`Liveness::active`] counts
//! members that are runnable or in uninterruptible (disk) wait.
//!
//! [`sample_group`] takes one sample. It is meant to be called at a bounded
//! interval ([`LIVENESS_INTERVAL`], by [`crate::local::LocalExec`]), never
//! per frame:
//!
//! - **Linux**: one pass over `/proc/<pid>/stat`, keeping the processes whose
//!   process group or session is the tool's (bash runs in its own session,
//!   so descendants that start their own group are still counted).
//! - **macOS and other unix**: one `ps -A -o pgid=,stat=,time=` run, keeping
//!   the group's members.
//! - Anything else, or a failure: `None`, and callers fall back to elapsed
//!   time against the timeout.
//!
//! CPU time is the sum over live members of their own user + system time
//! plus (on Linux) the time of children they already reaped, so a compiler
//! that runs and exits between two samples still shows as progress.

use std::path::Path;
use std::time::Duration;

/// How often [`crate::local::LocalExec`] samples a running process group.
pub const LIVENESS_INTERVAL: Duration = Duration::from_secs(3);

/// One liveness sample of a process group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Liveness {
    /// CPU time used so far by the group's live members (user + system,
    /// including reaped children where the platform reports it).
    pub cpu: Duration,
    /// Live (non-zombie) processes in the group, the shell included.
    pub procs: u32,
    /// Members that are running, runnable or in uninterruptible wait.
    pub active: u32,
}

/// Samples the process group `pgid`. `None` when liveness is unavailable on
/// this platform, or for ids that are not a real group (0 and 1).
pub fn sample_group(pgid: i32) -> Option<Liveness> {
    if pgid <= 1 {
        return None;
    }
    let proc_root = Path::new("/proc");
    if proc_root.join("self/stat").is_file() {
        return sample_procfs(proc_root, pgid, clock_ticks()?).ok();
    }
    sample_ps(pgid)
}

fn clock_ticks() -> Option<u64> {
    nix::unistd::sysconf(nix::unistd::SysconfVar::CLK_TCK)
        .ok()
        .flatten()
        .and_then(|t| u64::try_from(t).ok())
        .filter(|t| *t > 0)
}

/// The fields of `/proc/<pid>/stat` liveness needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProcStat {
    state: char,
    pgrp: i32,
    session: i32,
    /// utime + stime + cutime + cstime, in clock ticks.
    ticks: u64,
}

/// Parses one `/proc/<pid>/stat` line. The command name (field 2) may hold
/// spaces and parentheses, so fields are counted after its last `)`.
fn parse_proc_stat(line: &str) -> Option<ProcStat> {
    let rest = &line[line.rfind(')')? + 1..];
    // Fields from 3 (state) on.
    let f: Vec<&str> = rest.split_whitespace().collect();
    let num = |i: usize| f.get(i)?.parse::<i64>().ok();
    let state = f.first()?.chars().next()?;
    let pgrp = i32::try_from(num(2)?).ok()?;
    let session = i32::try_from(num(3)?).ok()?;
    // utime(14) stime(15) cutime(16) cstime(17) => indices 11..=14.
    let ticks = (11..=14)
        .map(|i| num(i).map(|v| v.max(0) as u64))
        .sum::<Option<u64>>()?;
    Some(ProcStat {
        state,
        pgrp,
        session,
        ticks,
    })
}

fn is_active(state: char) -> bool {
    // Linux: R running, D disk wait; macOS: R runnable, U uninterruptible.
    matches!(state, 'R' | 'D' | 'U')
}

fn sample_procfs(root: &Path, pgid: i32, ticks_per_sec: u64) -> std::io::Result<Liveness> {
    let mut ticks = 0u64;
    let mut out = Liveness::default();
    for entry in std::fs::read_dir(root)? {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        if !name
            .to_str()
            .is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()))
        {
            continue;
        }
        // Processes vanish between listing and reading: skip them.
        let Ok(line) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        let Some(st) = parse_proc_stat(&line) else {
            continue;
        };
        if (st.pgrp != pgid && st.session != pgid) || matches!(st.state, 'Z' | 'X') {
            continue;
        }
        out.procs += 1;
        if is_active(st.state) {
            out.active += 1;
        }
        ticks += st.ticks;
    }
    out.cpu = Duration::from_millis(ticks.saturating_mul(1000) / ticks_per_sec);
    Ok(out)
}

fn sample_ps(pgid: i32) -> Option<Liveness> {
    let out = std::process::Command::new("ps")
        .args(["-A", "-o", "pgid=", "-o", "stat=", "-o", "time="])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| parse_ps(&String::from_utf8_lossy(&out.stdout), pgid))
}

/// Parses `ps -o pgid=,stat=,time=` output, keeping the members of `pgid`.
fn parse_ps(text: &str, pgid: i32) -> Liveness {
    let mut out = Liveness::default();
    for line in text.lines() {
        let mut f = line.split_whitespace();
        let (Some(g), Some(stat), Some(time)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        if g.parse::<i32>().ok() != Some(pgid) {
            continue;
        }
        let state = stat.chars().next().unwrap_or('?');
        if state == 'Z' {
            continue;
        }
        out.procs += 1;
        if is_active(state) {
            out.active += 1;
        }
        out.cpu += parse_cpu_time(time).unwrap_or_default();
    }
    out
}

/// Parses a `ps` CPU time: `[DD-][HH:]MM:SS[.ff]` (macOS prints `M:SS.ff`,
/// procps `HH:MM:SS`).
fn parse_cpu_time(s: &str) -> Option<Duration> {
    let (days, clock) = match s.split_once('-') {
        Some((d, rest)) => (d.parse::<u64>().ok()?, rest),
        None => (0, s),
    };
    let mut parts: Vec<&str> = clock.split(':').collect();
    let secs: f64 = parts.pop()?.parse().ok()?;
    if !(secs.is_finite() && secs >= 0.0) || parts.len() > 2 {
        return None;
    }
    let mut whole = days * 86_400;
    let mut mult = 60;
    for p in parts.iter().rev() {
        whole += p.parse::<u64>().ok()? * mult;
        mult *= 60;
    }
    Some(Duration::from_secs(whole) + Duration::from_secs_f64(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_stat_parses_fields_after_the_command_name() {
        let line = "4242 (cargo (test) x) R 4200 4242 4242 0 -1 4194560 100 0 0 0 250 50 30 20 20 0 3 0 1 2 3";
        assert_eq!(
            parse_proc_stat(line),
            Some(ProcStat {
                state: 'R',
                pgrp: 4242,
                session: 4242,
                ticks: 350,
            })
        );
        assert_eq!(parse_proc_stat("garbage"), None);
        assert_eq!(parse_proc_stat("1 (x) S 0"), None);
    }

    #[test]
    fn procfs_sample_counts_group_and_session_members_only() {
        let dir = tempfile::tempdir().unwrap();
        let put = |pid: &str, line: &str| {
            std::fs::create_dir_all(dir.path().join(pid)).unwrap();
            std::fs::write(dir.path().join(pid).join("stat"), line).unwrap();
        };
        // The leader (sleeping), a busy child, a member that set its own
        // group but stays in the session, a zombie, and a stranger.
        put(
            "100",
            "100 (bash) S 1 100 100 0 -1 0 0 0 0 0 10 5 0 0 20 0 1 0 1 1 1",
        );
        put(
            "101",
            "101 (rustc) R 100 100 100 0 -1 0 0 0 0 0 200 85 0 0 20 0 1 0 1 1 1",
        );
        put(
            "102",
            "102 (sh) D 100 102 100 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 1 1 1",
        );
        put(
            "103",
            "103 (old) Z 100 100 100 0 -1 0 0 0 0 0 99 0 0 0 20 0 1 0 1 1 1",
        );
        put(
            "200",
            "200 (other) R 1 200 200 0 -1 0 0 0 0 0 500 0 0 0 20 0 1 0 1 1 1",
        );
        std::fs::create_dir_all(dir.path().join("self")).unwrap();
        let s = sample_procfs(dir.path(), 100, 100).unwrap();
        assert_eq!(
            s,
            Liveness {
                cpu: Duration::from_millis(3000),
                procs: 3,
                active: 2,
            }
        );
    }

    #[test]
    fn ps_output_parses_macos_and_procps_formats() {
        let text = "  500 Ss    0:00.05\n  500 R+    1:02.50\n  500 Z     0:09.00\n  501 R     9:00.00\n  500 S  01:00:00\n";
        let s = parse_ps(text, 500);
        assert_eq!(s.procs, 3);
        assert_eq!(s.active, 1);
        assert_eq!(s.cpu, Duration::from_millis(50 + 62_500 + 3_600_000));
        assert_eq!(
            parse_cpu_time("2-01:00:00"),
            Some(Duration::from_secs(2 * 86_400 + 3600))
        );
        assert_eq!(parse_cpu_time("x"), None);
        assert_eq!(parse_cpu_time("1:2:3:4"), None);
    }

    #[test]
    fn bogus_group_ids_are_never_sampled() {
        assert_eq!(sample_group(0), None);
        assert_eq!(sample_group(1), None);
        assert_eq!(sample_group(-5), None);
    }
}
