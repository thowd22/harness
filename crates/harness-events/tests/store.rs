//! Integration tests for the run store: round trip, crash truncation, blob
//! dedupe, schema versioning and redaction before persistence/delivery.

use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use harness_events::redact::{MASK, Redactor};
use harness_events::schema::{
    DeltaChannel, EventKind, NodeKind, NodeStatus, OutputStream, Payload, RunId, RunStatus,
    SCHEMA_VERSION, SchemaError, Usage,
};
use harness_events::store::{
    EVENTS_FILE, EventDraft, RecorderOptions, RunRecorder, StoreError, list_runs, read_run, run_dir,
};

const SECRET: &str = "ini-loaded-secret-42";

async fn recorder(data: &Path, threshold: usize) -> RunRecorder {
    RunRecorder::create(
        data,
        RunId::new(),
        Arc::new(Redactor::new([SECRET])),
        RecorderOptions {
            blob_threshold: threshold,
            ..RecorderOptions::default()
        },
    )
    .await
    .unwrap()
}

fn node_started(name: &str) -> EventKind {
    EventKind::NodeStarted {
        kind: NodeKind::Agent,
        name: name.into(),
        agent: None,
        model: Some("fake".into()),
        host: None,
        needs: vec![],
        queued: false,
    }
}

/// Every file under the run directory, concatenated.
fn all_bytes(dir: &Path) -> String {
    let mut out = String::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.push_str(&all_bytes(&path));
        } else {
            out.push_str(&String::from_utf8_lossy(&std::fs::read(&path).unwrap()));
        }
    }
    out
}

#[tokio::test]
async fn round_trip_with_sequence_and_parents() {
    let tmp = tempfile::tempdir().unwrap();
    let rec = recorder(tmp.path(), 1024).await;
    let mut emitted = Vec::new();
    let drafts = vec![
        EventDraft::run(EventKind::RunStarted {
            name: "demo".into(),
            workflow: None,
            host: None,
            title: None,
            agent_names: Default::default(),
        }),
        EventDraft::node("root", node_started("main")),
        EventDraft::node("child", node_started("researcher")).with_parent("root"),
        EventDraft::node(
            "child",
            EventKind::ToolCall {
                call_id: "t1".into(),
                tool: "read".into(),
                args: Payload::Json(serde_json::json!({"path": "a.rs"})),
            },
        ),
        EventDraft::node(
            "child",
            EventKind::NodeFinished {
                status: NodeStatus::Succeeded,
                error: None,
            },
        ),
        EventDraft::run(EventKind::RunFinished {
            status: RunStatus::Succeeded,
            error: None,
        }),
    ];
    for d in drafts {
        emitted.extend(rec.emit(d).unwrap());
    }
    rec.finish().await.unwrap();

    let replay = read_run(rec.dir()).unwrap();
    assert!(!replay.truncated);
    assert_eq!(replay.header.schema_version, SCHEMA_VERSION);
    assert_eq!(replay.header.run_id, rec.run_id());
    assert_eq!(replay.events, emitted);
    let seqs: Vec<u64> = replay.events.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, (0..6).collect::<Vec<_>>());
    // Parent is filled in from node_started for later events of the node.
    assert_eq!(
        replay.events[3].parent_node_id.as_ref().unwrap().as_str(),
        "root"
    );
    assert!(replay.events[1].parent_node_id.is_none());
    assert!(replay.events.windows(2).all(|w| w[0].ts <= w[1].ts));
    assert_eq!(list_runs(tmp.path()).unwrap(), vec![rec.run_id()]);
    assert_eq!(rec.dir(), run_dir(tmp.path(), rec.run_id()));
}

#[tokio::test]
async fn emit_after_finish_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let rec = recorder(tmp.path(), 1024).await;
    rec.finish().await.unwrap();
    let err = rec
        .emit(EventDraft::run(EventKind::Cost { usd: 1.0 }))
        .unwrap_err();
    assert!(matches!(err, StoreError::Closed));
}

#[tokio::test]
async fn flush_makes_events_readable_while_running() {
    let tmp = tempfile::tempdir().unwrap();
    let rec = recorder(tmp.path(), 1024).await;
    rec.emit(EventDraft::run(EventKind::Cost { usd: 0.5 }))
        .unwrap();
    rec.flush().await.unwrap();
    assert_eq!(read_run(rec.dir()).unwrap().events.len(), 1);
    rec.finish().await.unwrap();
}

#[tokio::test]
async fn tolerates_crash_truncated_final_line() {
    let tmp = tempfile::tempdir().unwrap();
    let rec = recorder(tmp.path(), 1024).await;
    for i in 0..3 {
        rec.emit(EventDraft::run(EventKind::Cost { usd: f64::from(i) }))
            .unwrap();
    }
    rec.finish().await.unwrap();
    let path = rec.dir().join(EVENTS_FILE);
    let clean = std::fs::read(&path).unwrap();
    // Simulate a crash mid-write: half a JSON object, no newline.
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    f.write_all(br#"{"seq":3,"ts":"2026-10-22T12:00:"#).unwrap();
    drop(f);

    let replay = read_run(rec.dir()).unwrap();
    assert!(replay.truncated);
    assert_eq!(replay.events.len(), 3);

    // Torn multi-byte character on the last line is also tolerated.
    std::fs::write(&path, &clean).unwrap();
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    f.write_all(b"{\"x\":\"\xE2\x82").unwrap();
    drop(f);
    let replay = read_run(rec.dir()).unwrap();
    assert!(replay.truncated);
    assert_eq!(replay.events.len(), 3);
}

#[tokio::test]
async fn corrupt_line_in_the_middle_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let rec = recorder(tmp.path(), 1024).await;
    rec.finish().await.unwrap();
    let path = rec.dir().join(EVENTS_FILE);
    let header = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        format!(
            "{header}garbage\n{}\n",
            r#"{"seq":0,"ts":"2026-10-22T12:00:00Z","run_id":"00000000-0000-0000-0000-000000000000","type":"node_running"}"#
        ),
    )
    .unwrap();
    assert!(matches!(
        read_run(rec.dir()),
        Err(StoreError::Json { line: 2, .. })
    ));
}

#[tokio::test]
async fn out_of_order_sequence_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let rec = recorder(tmp.path(), 1024).await;
    rec.finish().await.unwrap();
    let path = rec.dir().join(EVENTS_FILE);
    let header = std::fs::read_to_string(&path).unwrap();
    let ev = |seq: u64| {
        format!(
            r#"{{"seq":{seq},"ts":"2026-10-22T12:00:00Z","run_id":"00000000-0000-0000-0000-000000000000","type":"node_running"}}"#
        )
    };
    std::fs::write(&path, format!("{header}{}\n{}\n{}\n", ev(1), ev(1), ev(2))).unwrap();
    assert!(matches!(
        read_run(rec.dir()),
        Err(StoreError::OutOfOrder { line: 3, .. })
    ));
}

#[tokio::test]
async fn unsupported_schema_versions_are_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let rec = recorder(tmp.path(), 1024).await;
    rec.finish().await.unwrap();
    let path = rec.dir().join(EVENTS_FILE);
    let header = std::fs::read_to_string(&path).unwrap();
    for bad in [0, SCHEMA_VERSION + 1, 999] {
        let changed = header.replace(
            &format!("\"schema_version\":{SCHEMA_VERSION}"),
            &format!("\"schema_version\":{bad}"),
        );
        assert_ne!(changed, header);
        std::fs::write(&path, changed).unwrap();
        match read_run(rec.dir()) {
            Err(StoreError::Schema(SchemaError::UnsupportedVersion { found })) => {
                assert_eq!(found, bad)
            }
            other => panic!("version {bad}: {other:?}"),
        }
    }
}

#[tokio::test]
async fn large_payloads_go_to_deduplicated_blobs() {
    let tmp = tempfile::tempdir().unwrap();
    let rec = recorder(tmp.path(), 64).await;
    let big = "line of tool output\n".repeat(100);
    let mut refs = Vec::new();
    for call in ["t1", "t2"] {
        let ev = rec
            .emit(EventDraft::node(
                "n",
                EventKind::ToolResult {
                    call_id: call.into(),
                    ok: true,
                    output: Payload::text(big.clone()),
                    duration_ms: 3,
                },
            ))
            .unwrap()
            .remove(0);
        let EventKind::ToolResult {
            output: Payload::Blob(b),
            ..
        } = ev.kind
        else {
            panic!("expected blob, got {:?}", ev.kind);
        };
        refs.push(b);
    }
    // Small payloads stay inline.
    let small = rec
        .emit(EventDraft::node(
            "n",
            EventKind::ToolCall {
                call_id: "t3".into(),
                tool: "x".into(),
                args: Payload::text("tiny"),
            },
        ))
        .unwrap()
        .remove(0);
    assert!(matches!(
        small.kind,
        EventKind::ToolCall {
            args: Payload::Text(_),
            ..
        }
    ));
    rec.finish().await.unwrap();

    assert_eq!(refs[0], refs[1]);
    let blob_files = std::fs::read_dir(rec.blobs().dir()).unwrap().count();
    assert_eq!(blob_files, 1, "identical payloads must be stored once");
    let replay = read_run(rec.dir()).unwrap();
    let EventKind::ToolResult { output, .. } = &replay.events[0].kind else {
        panic!()
    };
    assert_eq!(rec.blobs().resolve(output).unwrap(), Payload::text(big));
}

#[tokio::test]
async fn events_are_redacted_before_persistence_and_delivery() {
    let tmp = tempfile::tempdir().unwrap();
    let rec = recorder(tmp.path(), 64).await;
    let mut live = rec.subscribe();

    let returned = rec
        .emit(EventDraft::node(
            "n",
            EventKind::ToolResult {
                call_id: "t1".into(),
                ok: true,
                output: Payload::text(format!("ANTHROPIC_API_KEY={SECRET}\n")),
                duration_ms: 1,
            },
        ))
        .unwrap();
    // The returned tool result (what the agent sends back to the model) is redacted.
    let EventKind::ToolResult {
        output: Payload::Text(t),
        ..
    } = &returned[0].kind
    else {
        panic!()
    };
    assert_eq!(t, &format!("ANTHROPIC_API_KEY={MASK}\n"));

    // Big payload with the secret: redacted before it becomes a blob.
    rec.emit(EventDraft::node(
        "n",
        EventKind::VerifyFinished {
            command: "env".into(),
            passed: true,
            exit_code: Some(0),
            duration_ms: 1,
            output: Payload::text(format!("{}{SECRET}", "x".repeat(200))),
        },
    ))
    .unwrap();
    rec.emit(EventDraft::node(
        "n",
        EventKind::Error {
            message: format!("failed with Bearer {SECRET}"),
            recoverable: true,
            action: None,
        },
    ))
    .unwrap();

    // A secret split across streaming chunks.
    let call = "m1".to_string();
    rec.emit(EventDraft::node(
        "n",
        EventKind::ModelCallStarted {
            call_id: call.clone(),
            provider: "fake".into(),
            model: "fake".into(),
            request: Payload::Json(serde_json::json!({"system": format!("key {SECRET}")})),
        },
    ))
    .unwrap();
    let chunks = ["the key is ini-loa", "ded-sec", "ret-42 ok"];
    let mut streamed = String::new();
    for c in chunks {
        for ev in rec
            .emit(EventDraft::node(
                "n",
                EventKind::Delta {
                    call_id: call.clone(),
                    channel: DeltaChannel::Text,
                    text: c.into(),
                },
            ))
            .unwrap()
        {
            let EventKind::Delta { text, .. } = ev.kind else {
                panic!()
            };
            streamed.push_str(&text);
        }
    }
    let fin = rec
        .emit(EventDraft::node(
            "n",
            EventKind::ModelCallFinished {
                call_id: call,
                usage: Usage::default(),
                stop_reason: None,
                response: None,
                duration_ms: 1,
            },
        ))
        .unwrap();
    assert_eq!(
        fin.len(),
        2,
        "held text is released before the finish event"
    );
    let EventKind::Delta { text, .. } = &fin[0].kind else {
        panic!()
    };
    streamed.push_str(text);
    assert_eq!(streamed, format!("the key is {MASK} ok"));

    // Unfinished stream is released on finish().
    rec.emit(EventDraft::node(
        "n",
        EventKind::Delta {
            call_id: "m2".into(),
            channel: DeltaChannel::Reasoning,
            text: format!("thinking about {SECRET}"),
        },
    ))
    .unwrap();
    rec.finish().await.unwrap();

    let on_disk = all_bytes(rec.dir());
    assert!(
        !on_disk.contains(SECRET),
        "secret leaked to disk:\n{on_disk}"
    );
    assert!(
        !on_disk.contains("ini-loa"),
        "partial secret leaked:\n{on_disk}"
    );
    let mut delivered = 0;
    while let Ok(ev) = live.try_recv() {
        let json = serde_json::to_string(&ev).unwrap();
        assert!(!json.contains("ini-loa"), "secret leaked to UI: {json}");
        delivered += 1;
    }
    let replay = read_run(rec.dir()).unwrap();
    assert_eq!(delivered, replay.events.len());
    let last = replay.events.last().unwrap();
    assert!(
        matches!(&last.kind, EventKind::Delta { text, .. } if text == &format!("thinking about {MASK}"))
    );
}

#[tokio::test]
async fn tool_output_is_stream_redacted_and_flushed_by_the_result() {
    let data = tempfile::tempdir().unwrap();
    let rec = recorder(data.path(), 1 << 20).await;
    let mut live = rec.subscribe();
    let node = harness_events::schema::NodeId::from("n");
    rec.emit(EventDraft::node(node.clone(), node_started("agent")))
        .unwrap();
    // The secret is split across two chunks of live output.
    let (a, b) = SECRET.split_at(7);
    for chunk in [format!("token={a}"), format!("{b} done\n")] {
        rec.emit(EventDraft::node(
            node.clone(),
            EventKind::ToolOutput {
                call_id: "c1".into(),
                stream: OutputStream::Stdout,
                text: chunk,
            },
        ))
        .unwrap();
    }
    let out = rec
        .emit(EventDraft::node(
            node.clone(),
            EventKind::ToolResult {
                call_id: "c1".into(),
                ok: true,
                output: Payload::text("done"),
                duration_ms: 1,
            },
        ))
        .unwrap();
    // Held output is released before the result.
    assert!(matches!(
        out.first().unwrap().kind,
        EventKind::ToolOutput { .. }
    ));
    assert!(matches!(
        out.last().unwrap().kind,
        EventKind::ToolResult { .. }
    ));
    rec.finish().await.unwrap();

    let mut text = String::new();
    while let Ok(ev) = live.try_recv() {
        if let EventKind::ToolOutput { text: t, .. } = &ev.kind {
            text.push_str(t);
        }
    }
    assert_eq!(text, format!("token={MASK} done\n"));
    let disk = all_bytes(&run_dir(data.path(), rec.run_id()));
    assert!(!disk.contains(SECRET));
    assert!(
        !disk.contains(a) || !disk.contains(b),
        "a split secret reached the log"
    );
}

#[tokio::test]
async fn header_carries_the_config_hash_and_unfinished_output_is_released() {
    let data = tempfile::tempdir().unwrap();
    let rec = RunRecorder::create(
        data.path(),
        RunId::new(),
        Arc::new(Redactor::new([SECRET])),
        RecorderOptions::default().with_config_hash("abc123"),
    )
    .await
    .unwrap();
    rec.emit(EventDraft::run(EventKind::UserMessage {
        text: format!("use {SECRET}"),
    }))
    .unwrap();
    rec.emit(EventDraft::run(EventKind::ToolOutput {
        call_id: "c".into(),
        stream: OutputStream::Stderr,
        text: "partial".into(),
    }))
    .unwrap();
    rec.finish().await.unwrap();
    let replay = read_run(&run_dir(data.path(), rec.run_id())).unwrap();
    assert_eq!(replay.header.config_hash.as_deref(), Some("abc123"));
    let kinds: Vec<&EventKind> = replay.events.iter().map(|e| &e.kind).collect();
    assert!(matches!(kinds[0], EventKind::UserMessage { text } if text == &format!("use {MASK}")));
    assert!(matches!(kinds[1], EventKind::ToolOutput { text, .. } if text == "partial"));
}
