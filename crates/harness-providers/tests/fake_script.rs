//! Drives the fake provider from a YAML fixture, as eval and integration
//! tests do.

use harness_providers::fake::FakeProvider;
use harness_providers::fake::FakeScript;
use harness_providers::model::{ChatRequest, ContentBlock, Message, Role, StopKind, Usage};
use harness_providers::provider::{Provider, ProviderError};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn yaml_fixture_runs_a_tool_loop_and_records_requests() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/fake_tool_loop.yaml"
    );
    let provider = FakeProvider::new(FakeScript::from_yaml_file(path).unwrap());
    let mut request = ChatRequest::new("fake");
    request
        .messages
        .push(Message::user("What is in the README?"));

    let first = provider
        .complete(request.clone(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(first.stop.kind, StopKind::ToolUse);
    let (id, name, _) = first.message.tool_uses().next().unwrap();
    assert_eq!((id, name), ("call_1", "read"));

    request.messages.push(first.message.clone());
    request.messages.push(Message {
        role: Role::User,
        content: vec![ContentBlock::tool_result("call_1", "# harness", false)],
    });
    let err = provider
        .complete(request.clone(), CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, ProviderError::Overloaded { .. }));
    let last = provider
        .complete(request.clone(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(last.message.text(), "The README describes the project.");
    assert_eq!(
        last.usage,
        Usage {
            input_tokens: 80,
            output_tokens: 8,
            ..Usage::default()
        }
    );

    let seen = provider.requests();
    assert_eq!(seen.len(), 3);
    assert_eq!(seen[0].messages.len(), 1);
    assert_eq!(seen[2].messages.len(), 3);
    assert_eq!(
        seen[2].messages[2].content[0],
        ContentBlock::tool_result("call_1", "# harness", false)
    );
}
