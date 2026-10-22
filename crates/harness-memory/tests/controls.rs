//! Memory controls (TASK-97 AC#3, AC#4): turning memory off keeps the
//! notes, agents can opt out with `memory: off`, and headless runs get no
//! memory unless `[memory] headless_access = true`.

use std::sync::Arc;

use harness_config::agents::MemoryAccess;
use harness_config::ini::Config;
use harness_events::redact::Redactor;
use harness_memory::drive::Drive;
use harness_memory::sync::SessionCheckout;
use harness_memory::tool::{MemoryArgs, MemoryTool, MemoryToolConfig, effective_access, spec};

fn tool(d: &Drive, session: &str, access: MemoryAccess) -> MemoryTool {
    MemoryTool::new(
        Arc::new(SessionCheckout::open(d, session).unwrap()),
        MemoryToolConfig {
            access,
            run_id: session.into(),
            project_key: None,
            redactor: Arc::new(Redactor::default()),
            date: None,
        },
    )
}

#[test]
fn disabling_memory_keeps_the_notes() {
    let tmp = tempfile::tempdir().unwrap();
    let d = Drive::open_or_init(tmp.path().join("memory")).unwrap();
    tool(&d, "s1", MemoryAccess::ReadWrite)
        .execute(MemoryArgs::Write {
            path: "tools/cargo".into(),
            text: "Use cargo nextest for the slow suite.".into(),
            title: None,
        })
        .unwrap();
    let mut cfg = Config::defaults();
    cfg.memory.enabled = false;
    let off = effective_access(MemoryAccess::ReadWrite, &cfg.memory, false);
    assert_eq!(off, MemoryAccess::Off);
    assert!(spec(off).is_none(), "no memory tool while disabled");
    assert!(d.root().join("tools/cargo.md").is_file(), "notes are kept");
    // Turning it back on finds the same notes.
    cfg.memory.enabled = true;
    let on = effective_access(MemoryAccess::ReadWrite, &cfg.memory, false);
    let out = tool(&d, "s2", on)
        .execute(MemoryArgs::Search {
            query: "nextest".into(),
            limit: None,
        })
        .unwrap();
    assert!(out.output.contains("tools/cargo"), "{}", out.output);
}

#[test]
fn agents_can_turn_memory_off() {
    let cfg = Config::defaults();
    assert_eq!(
        effective_access(MemoryAccess::Off, &cfg.memory, false),
        MemoryAccess::Off
    );
    assert_eq!(
        effective_access(MemoryAccess::Read, &cfg.memory, false),
        MemoryAccess::Read
    );
    let catalog = harness_config::discovery::Catalog::load(&harness_config::builtins::roots());
    let reviewer = catalog.agents.get("reviewer").unwrap();
    assert_eq!(reviewer.memory, MemoryAccess::Off);
    assert!(!reviewer.request_tools().contains(&"memory"));
}

#[test]
fn headless_runs_need_headless_access() {
    let mut cfg = Config::defaults();
    assert!(!cfg.memory.headless_access, "off by default");
    assert_eq!(
        effective_access(MemoryAccess::ReadWrite, &cfg.memory, true),
        MemoryAccess::Off
    );
    cfg.memory.headless_access = true;
    assert_eq!(
        effective_access(MemoryAccess::ReadWrite, &cfg.memory, true),
        MemoryAccess::ReadWrite
    );
}
