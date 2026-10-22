use super::*;

#[test]
fn stop_reasons_have_distinct_codes_and_round_trip_through_node_finished() {
    let all = [
        StopReason::EndTurn,
        StopReason::MaxTurns { limit: 7 },
        StopReason::Budget {
            reason: "token budget of 10 exhausted".into(),
        },
        StopReason::Cancelled,
        StopReason::Error {
            message: "model call failed: boom".into(),
            action: None,
        },
    ];
    let codes: std::collections::HashSet<&str> = all.iter().map(StopReason::code).collect();
    assert_eq!(codes.len(), all.len());
    for reason in all {
        let back =
            StopReason::from_node_finished(reason.node_status(), reason.error_text().as_deref());
        assert_eq!(back, reason);
    }
}

#[test]
fn unknown_failures_map_to_error() {
    assert_eq!(
        StopReason::from_node_finished(NodeStatus::Failed, Some("weird")),
        StopReason::Error {
            message: "weird".into(),
            action: None,
        }
    );
}

#[test]
fn conversation_helpers() {
    let mut c = Conversation::new(Some("s".into()));
    assert_eq!(c.last_assistant_text(), "");
    c.push_user("u");
    c.messages.push(Message::assistant("a"));
    assert_eq!(c.last_assistant_text(), "a");
}
