//! Spotting permission errors the sandbox may have caused, and appending
//! [`super::DENIAL_NOTE`] to the command's output.

use std::path::{Component, Path, PathBuf};

use harness_tools::exec::{ExecEvent, ExecHandle, OutputChunk, StreamKind};
use tokio::sync::{mpsc, oneshot};

use super::WARNING_PREFIX;

/// Messages of `EACCES` / `EPERM`, which Landlock denials produce.
const DENIALS: &[&str] = &["Permission denied", "Operation not permitted"];

/// Longest partial line kept per stream before it is scanned anyway.
const MAX_CARRY: usize = 8 * 1024;

/// Scans output line by line for a denial the writable roots don't explain.
#[derive(Debug)]
pub(crate) struct DenialScan {
    roots: Vec<PathBuf>,
    cwd: PathBuf,
    carry: [Vec<u8>; 2],
    hit: bool,
}

impl DenialScan {
    /// `roots`: absolute writable paths (devices included); `cwd`: where
    /// the command runs, for relative paths in its messages.
    pub(crate) fn new(roots: impl IntoIterator<Item = PathBuf>, cwd: &Path) -> Self {
        let mut all = Vec::new();
        for r in roots {
            let r = normalize(&r);
            if let Ok(c) = std::fs::canonicalize(&r)
                && c != r
            {
                all.push(c);
            }
            all.push(r);
        }
        Self {
            roots: all,
            cwd: cwd.to_path_buf(),
            carry: [Vec::new(), Vec::new()],
            hit: false,
        }
    }

    pub(crate) fn feed(&mut self, chunk: &OutputChunk) {
        if self.hit {
            return;
        }
        let i = match chunk.stream {
            StreamKind::Stdout => 0,
            StreamKind::Stderr => 1,
        };
        let mut buf = std::mem::take(&mut self.carry[i]);
        buf.extend_from_slice(&chunk.data);
        let mut start = 0;
        while let Some(n) = buf[start..].iter().position(|&b| b == b'\n') {
            self.line(&buf[start..start + n]);
            start += n + 1;
        }
        buf.drain(..start);
        if buf.len() > MAX_CARRY {
            self.line(&buf);
            buf.clear();
        }
        self.carry[i] = buf;
    }

    /// Scan what is left and report whether a note is due.
    pub(crate) fn finish(&mut self) -> bool {
        for i in 0..2 {
            let rest = std::mem::take(&mut self.carry[i]);
            if !rest.is_empty() {
                self.line(&rest);
            }
        }
        self.hit
    }

    fn line(&mut self, line: &[u8]) {
        if self.hit {
            return;
        }
        let text = String::from_utf8_lossy(line);
        if text.starts_with(WARNING_PREFIX) || !DENIALS.iter().any(|d| text.contains(d)) {
            return;
        }
        let mut paths = absolute_paths(&text);
        paths.extend(adjacent_relative_path(&text, &self.cwd));
        // No path (an ioctl, a socket...), a path outside the writable roots,
        // or a device (writes there are allowed, so an ioctl was refused).
        self.hit = paths.is_empty()
            || paths
                .iter()
                .any(|p| p.starts_with("/dev/") || !self.inside(p));
    }

    fn inside(&self, p: &Path) -> bool {
        let p = normalize(p);
        self.roots.iter().any(|r| p.starts_with(r))
    }
}

/// Absolute paths mentioned in a line of error output.
fn absolute_paths(line: &str) -> Vec<PathBuf> {
    line.split(|c: char| c.is_whitespace() || "'\"`:,;()[]{}<>=".contains(c))
        .filter(|t| t.starts_with('/') && t.len() > 1)
        .map(|t| PathBuf::from(t.trim_end_matches(['.', '!'])))
        .collect()
}

/// The relative path right before or after the denial message
/// (`ro: Permission denied`, `Permission denied: 'ro'`), resolved against
/// `cwd`, if it exists there.
fn adjacent_relative_path(line: &str, cwd: &Path) -> Option<PathBuf> {
    let token = |t: &str| {
        t.trim_matches(|c: char| "'\"`:,;()[]{}<>".contains(c))
            .to_string()
    };
    let (at, msg) = DENIALS.iter().find_map(|d| line.find(d).map(|i| (i, *d)))?;
    let before = line[..at].split_whitespace().last().map(token);
    let after = line[at + msg.len()..]
        .trim_start_matches(|c: char| c == ':' || c.is_whitespace())
        .split_whitespace()
        .next()
        .map(token);
    [before, after]
        .into_iter()
        .flatten()
        .filter(|t| !t.is_empty() && !t.starts_with('/'))
        .map(|t| cwd.join(t))
        .find(|p| p.symlink_metadata().is_ok())
}

/// `p` with `.` and `..` resolved lexically.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// A handle that forwards `inner` unchanged and, when the command fails
/// with a denial `scan` attributes to the sandbox, adds
/// [`super::DENIAL_NOTE`] on stderr before the exit outcome. Kill and drop
/// propagate to `inner`, so the process group still dies with the handle.
pub(crate) fn relay(mut inner: ExecHandle, mut scan: DenialScan) -> ExecHandle {
    let (otx, orx) = mpsc::unbounded_channel();
    let (ltx, lrx) = mpsc::unbounded_channel();
    let (ktx, mut krx) = oneshot::channel::<()>();
    let (stx, srx) = oneshot::channel();
    tokio::spawn(async move {
        let mut kill_pending = true;
        loop {
            tokio::select! {
                event = inner.next_event() => match event {
                    Some(ExecEvent::Output(chunk)) => {
                        scan.feed(&chunk);
                        let _ = otx.send(chunk);
                    }
                    Some(ExecEvent::Liveness(sample)) => {
                        let _ = ltx.send(sample);
                    }
                    None => break,
                },
                // A kill request, or the outer handle was dropped.
                _ = &mut krx, if kill_pending => {
                    kill_pending = false;
                    inner.kill();
                }
            }
        }
        drop(ltx);
        // Output can close long before the process exits (`exec >log`), so
        // keep honouring kill and drop of the outer handle until it does.
        let outcome = inner
            .wait_or_kill(async move {
                if kill_pending {
                    let _ = krx.await;
                } else {
                    std::future::pending::<()>().await;
                }
            })
            .await;
        if let Ok(o) = &outcome
            && !o.success()
            && !o.killed
            && !o.timed_out
            && scan.finish()
        {
            let _ = otx.send(OutputChunk {
                stream: StreamKind::Stderr,
                data: super::DENIAL_NOTE.as_bytes().to_vec(),
            });
        }
        drop(otx);
        // On a lost outcome, dropping `stx` reports `ExecError::Lost`.
        if let Ok(o) = outcome {
            let _ = stx.send(o);
        }
    });
    ExecHandle::new(orx, ktx, srx).with_liveness(lrx)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan() -> DenialScan {
        DenialScan::new(
            ["/ws".into(), "/tmp".into(), "/dev/null".into()],
            Path::new("/ws"),
        )
    }

    fn feed(s: &mut DenialScan, stream: StreamKind, text: &str) {
        s.feed(&OutputChunk {
            stream,
            data: text.as_bytes().to_vec(),
        });
    }

    #[test]
    fn flags_denials_outside_roots_and_on_devices() {
        for line in [
            "touch: cannot touch '/home/u/.cache/x': Permission denied\n",
            "PermissionError: [Errno 13] Permission denied: '/etc/x'\n",
            "stty: /dev/null: Permission denied\n",
            "ioctl failed: Permission denied\n",
            "mkdir: cannot create directory '/opt/y': Operation not permitted\n",
            "touch: cannot touch '/ws/../outside/f': Permission denied\n",
        ] {
            let mut s = scan();
            feed(&mut s, StreamKind::Stderr, line);
            assert!(s.finish(), "{line}");
        }
    }

    #[test]
    fn ignores_explained_or_unrelated_lines() {
        for line in [
            "cat: /ws/secret: Permission denied\n",
            "rm: cannot remove '/tmp/x/y': Permission denied\n",
            "all good\n",
            "harness sandbox: Landlock is only partially enforced: Permission denied\n",
        ] {
            let mut s = scan();
            feed(&mut s, StreamKind::Stderr, line);
            assert!(!s.finish(), "{line}");
        }
    }

    #[test]
    fn relative_paths_that_exist_in_cwd_are_explained() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ro"), "").unwrap();
        let mut s = DenialScan::new([dir.path().to_path_buf()], dir.path());
        feed(
            &mut s,
            StreamKind::Stderr,
            "bash: line 1: ro: Permission denied\n",
        );
        assert!(!s.hit, "{s:?}");
        feed(
            &mut s,
            StreamKind::Stderr,
            "PermissionError: Permission denied: 'ro'\n",
        );
        assert!(!s.finish(), "{s:?}");
        // A word that is not a file there does not explain it.
        let mut s = DenialScan::new([dir.path().to_path_buf()], dir.path());
        feed(
            &mut s,
            StreamKind::Stderr,
            "ioctl failed: Permission denied\n",
        );
        assert!(s.finish());
    }

    #[test]
    fn joins_lines_split_across_chunks_per_stream() {
        let mut s = scan();
        feed(&mut s, StreamKind::Stderr, "touch: cannot touch '/opt/");
        feed(&mut s, StreamKind::Stdout, "unrelated\n");
        assert!(!s.hit);
        feed(&mut s, StreamKind::Stderr, "x': Permission denied\nmore");
        assert!(s.hit);
        // An unterminated last line is scanned on finish.
        let mut s = scan();
        feed(&mut s, StreamKind::Stdout, "x: Permission denied");
        assert!(s.finish());
    }

    #[test]
    fn note_is_short_and_names_the_setting() {
        let note = super::super::DENIAL_NOTE;
        assert!(note.starts_with(WARNING_PREFIX));
        assert!(note.contains("[sandbox]") && note.contains("harness.ini"));
        assert!(note.len() < 260, "{}", note.len());
    }

    type Parts = (
        ExecHandle,
        oneshot::Receiver<()>,
        oneshot::Sender<harness_tools::exec::ExitOutcome>,
    );

    /// A handle that has already produced `chunks` on stderr.
    fn handle(chunks: Vec<&str>) -> Parts {
        let (otx, orx) = mpsc::unbounded_channel();
        let (ktx, krx) = oneshot::channel();
        let (stx, srx) = oneshot::channel();
        for c in chunks {
            otx.send(OutputChunk {
                stream: StreamKind::Stderr,
                data: c.as_bytes().to_vec(),
            })
            .unwrap();
        }
        drop(otx);
        (ExecHandle::new(orx, ktx, srx), krx, stx)
    }

    async fn collect(h: ExecHandle) -> (String, harness_tools::exec::ExitOutcome) {
        let mut out = String::new();
        let status = h
            .drive(|c| out.push_str(&String::from_utf8_lossy(&c.data)))
            .await
            .unwrap();
        (out, status)
    }

    fn exited(code: i32) -> harness_tools::exec::ExitOutcome {
        harness_tools::exec::ExitOutcome {
            code: Some(code),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn relay_appends_note_only_on_failed_denials() {
        let denial = "touch: cannot touch '/opt/x': Permission denied\n";
        let (h, _k, stx) = handle(vec![denial]);
        stx.send(exited(1)).unwrap();
        let (out, status) = collect(relay(h, scan())).await;
        assert_eq!(status.code, Some(1));
        assert!(
            out.starts_with(denial) && out.ends_with(super::super::DENIAL_NOTE),
            "{out}"
        );

        let (h, _k, stx) = handle(vec![denial]);
        stx.send(exited(0)).unwrap();
        let (out, _) = collect(relay(h, scan())).await;
        assert_eq!(out, denial, "a command that succeeded gets no note");

        let (h, _k, stx) = handle(vec!["cat: /ws/a: Permission denied\n"]);
        stx.send(exited(1)).unwrap();
        let (out, _) = collect(relay(h, scan())).await;
        assert!(!out.contains(WARNING_PREFIX), "{out}");
    }

    #[tokio::test]
    async fn relay_propagates_kill_and_drop() {
        let (otx, orx) = mpsc::unbounded_channel::<OutputChunk>();
        let (ktx, krx) = oneshot::channel();
        let (_stx, srx) = oneshot::channel::<harness_tools::exec::ExitOutcome>();
        let mut outer = relay(ExecHandle::new(orx, ktx, srx), scan());
        outer.kill();
        // The inner implementation sees the kill request.
        assert!(krx.await.is_ok());
        drop(otx);

        let (_otx, orx) = mpsc::unbounded_channel::<OutputChunk>();
        let (ktx, krx) = oneshot::channel();
        let (_stx, srx) = oneshot::channel::<harness_tools::exec::ExitOutcome>();
        drop(relay(ExecHandle::new(orx, ktx, srx), scan()));
        // Dropping the outer handle kills (or drops) the inner one.
        let _ = krx.await;
    }

    #[tokio::test]
    async fn relay_kills_after_output_closed() {
        // Output closed but no outcome yet (`exec >/dev/null 2>&1; sleep`).
        for drop_outer in [false, true] {
            let (otx, orx) = mpsc::unbounded_channel::<OutputChunk>();
            drop(otx);
            let (ktx, krx) = oneshot::channel();
            let (stx, srx) = oneshot::channel::<harness_tools::exec::ExitOutcome>();
            let mut outer = relay(ExecHandle::new(orx, ktx, srx), scan());
            // Let the relay see EOF and start waiting for the outcome.
            for _ in 0..10 {
                tokio::task::yield_now().await;
            }
            if drop_outer {
                drop(outer);
                let got = tokio::time::timeout(std::time::Duration::from_secs(10), krx).await;
                assert!(
                    matches!(got, Ok(Ok(()))),
                    "drop after EOF must kill: {got:?}"
                );
                drop(stx);
            } else {
                outer.kill();
                let got = tokio::time::timeout(std::time::Duration::from_secs(10), krx).await;
                assert!(
                    matches!(got, Ok(Ok(()))),
                    "kill after EOF must reach inner: {got:?}"
                );
                stx.send(harness_tools::exec::ExitOutcome {
                    killed: true,
                    ..Default::default()
                })
                .unwrap();
                assert!(outer.wait().await.unwrap().killed);
            }
        }
    }
}
