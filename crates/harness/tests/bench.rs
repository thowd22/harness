//! `harness bench --fake` end to end (TASK-161): the real binary runs the
//! bundled two-turn task against the fake provider, offline, and reports
//! every model call, gap, tool and hook. Timings are not asserted (they
//! depend on the machine); structure, counts and the cache-prefix
//! stability are.

mod common;

use std::process::Command;

use common::*;
use serde_json::Value;

fn bench(args: &[&str]) -> (std::process::Output, tempfile::TempDir) {
    let root = hermetic_root();
    let tmp = root.path().join("tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    let mut c = Command::new(env!("CARGO_BIN_EXE_harness"));
    hermetic(&mut c, root.path());
    c.env("TMPDIR", &tmp)
        .current_dir(root.path())
        .arg("bench")
        .args(args);
    (c.output().unwrap(), root)
}

#[test]
fn fake_bench_reports_calls_gaps_tools_and_a_stable_prefix() {
    let (o, root) = bench(&["--fake", "--runs", "2", "--json", "-"]);
    assert_eq!(o.status.code(), Some(0), "stderr: {}", stderr(&o));
    let r: Value = serde_json::from_str(&stdout(&o)).unwrap();
    assert_eq!(r["mode"], "fake");
    assert_eq!(r["runs"].as_array().unwrap().len(), 2);
    for run in r["runs"].as_array().unwrap() {
        let calls = run["calls"].as_array().unwrap();
        // Two user turns, five scripted calls each.
        assert_eq!(calls.len(), 10, "{run}");
        assert!(calls.iter().all(|c| c["kind"] == "agent"));
        assert!(calls.iter().all(|c| c["ttfb_ms"].is_number()));
        assert!(calls.iter().all(|c| c["ttft_ms"].is_number()));
        assert!(calls.iter().all(|c| c["body"]["total"].as_u64() > Some(0)));
        assert_eq!(run["turns"].as_array().unwrap().len(), 2);
        let gaps = run["gaps"].as_array().unwrap();
        assert_eq!(gaps.len(), 10);
        assert_eq!(gaps.iter().filter(|g| g["turn_start"] == true).count(), 2);
        // The first reply asks for glob and grep together.
        assert_eq!(gaps[1]["tool_calls"], 2);
        let tools: Vec<&str> = run["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["tool"].as_str().unwrap())
            .collect();
        for t in ["bash", "edit", "glob", "grep", "read"] {
            assert!(tools.contains(&t), "{tools:?}");
        }
        let hooks: Vec<&str> = run["hooks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["hook"].as_str().unwrap())
            .collect();
        assert!(
            hooks.contains(&"context") && hooks.contains(&"budget"),
            "{hooks:?}"
        );
    }
    // Cache stability: every request starts with the whole previous
    // request, byte for byte (tools, system, messages).
    assert_eq!(r["summary"]["prefix_breaks"], 0, "{}", r["summary"]);
    assert_eq!(r["summary"]["prefix_shared_pct"]["min"], 100.0);
    assert!(r["summary"]["overhead_per_call_ms"]["p50"].is_number());
    assert!(!r["tool_schemas"].as_array().unwrap().is_empty());
    assert!(!r["emit"].as_array().unwrap().is_empty());
    assert!(r["render"]["frames"].as_u64() > Some(0));
    // Temporary repositories and data dirs are removed.
    let left: Vec<_> = std::fs::read_dir(root.path().join("tmp"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn bench_prints_a_table_and_rejects_bad_arguments() {
    let (o, _root) = bench(&["--fake", "--runs", "1", "--no-render", "--turns", "1"]);
    assert_eq!(o.status.code(), Some(0), "stderr: {}", stderr(&o));
    let text = stdout(&o);
    for needle in ["ttfb ms", "overhead", "summary over 1 run(s)", "turn 1:"] {
        assert!(text.contains(needle), "{needle}: {text}");
    }
    assert!(!text.contains("turn 2:"), "{text}");
    let (o, _root) = bench(&[]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("exactly one of --fake and --model"));
    let (o, _root) = bench(&["--help"]);
    assert_eq!(o.status.code(), Some(0));
    assert!(stdout(&o).contains("usage: harness bench"));
}
