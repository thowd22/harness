use std::sync::atomic::AtomicUsize;

use harness_providers::fake::{FakeError, FakeErrorKind, FakeProvider, FakeTurn};
use harness_providers::provider::collect;

use super::*;

fn sched(slots: usize) -> Scheduler {
    Scheduler::new(SchedulerConfig {
        max_concurrent_agents: slots,
        default_rate_limit_pause: Duration::from_millis(30),
        ..SchedulerConfig::default()
    })
}

#[tokio::test]
async fn global_agent_limit_is_enforced_and_queued_work_is_reported() {
    let s = sched(2);
    let cancel = CancellationToken::new();
    let a = s
        .acquire_agent(&cancel, || panic!("first slot is free"))
        .await
        .unwrap();
    let _b = s
        .acquire_agent(&cancel, || panic!("second slot is free"))
        .await
        .unwrap();
    assert_eq!(s.stats().agents_running, 2);

    let queued = Arc::new(AtomicUsize::new(0));
    let q = queued.clone();
    let s2 = s.clone();
    let c2 = cancel.clone();
    let waiter = tokio::spawn(async move {
        s2.acquire_agent(&c2, move || {
            q.fetch_add(1, Ordering::SeqCst);
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(queued.load(Ordering::SeqCst), 1, "on_queued ran once");
    assert_eq!(s.stats().agents_queued, 1);
    assert!(!waiter.is_finished(), "third agent must wait");
    drop(a);
    let c = waiter.await.unwrap().unwrap();
    assert!(c.is_held());
    assert_eq!(s.stats().agents_queued, 0);
    assert_eq!(s.stats().agents_running, 2);
}

#[tokio::test]
async fn cancelled_wait_returns_error() {
    let s = sched(1);
    let cancel = CancellationToken::new();
    let _a = s.acquire_agent(&cancel, || {}).await.unwrap();
    let c2 = cancel.clone();
    let s2 = s.clone();
    let w = tokio::spawn(async move { s2.acquire_agent(&c2, || {}).await.map(|_| ()) });
    tokio::time::sleep(Duration::from_millis(10)).await;
    cancel.cancel();
    assert_eq!(w.await.unwrap(), Err(Cancelled));
    assert_eq!(s.stats().agents_queued, 0);
}

#[tokio::test]
async fn suspended_slot_lends_capacity_to_children() {
    let s = sched(1);
    let cancel = CancellationToken::new();
    let parent = s.acquire_agent(&cancel, || {}).await.unwrap();
    parent.suspend();
    assert!(!parent.is_held());
    let child = s
        .acquire_agent(&cancel, || panic!("slot was lent"))
        .await
        .unwrap();
    drop(child);
    parent.resume().await;
    assert!(parent.is_held());
    assert_eq!(s.stats().agents_running, 1);
}

#[tokio::test]
async fn per_provider_call_limit_is_enforced() {
    let s = Scheduler::new(SchedulerConfig::default().with_provider_limit("fake", 1));
    let cancel = CancellationToken::new();
    let p1 = s.acquire_call("fake", &cancel).await.unwrap();
    // Another provider is not affected.
    let _other = s.acquire_call("other", &cancel).await.unwrap();
    let s2 = s.clone();
    let c2 = cancel.clone();
    let w = tokio::spawn(async move { s2.acquire_call("fake", &c2).await.map(|_| ()) });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!w.is_finished(), "second call to the same provider waits");
    assert_eq!(s.stats().calls_running, 2);
    drop(p1);
    w.await.unwrap().unwrap();
}

#[tokio::test]
async fn rate_limit_pauses_new_calls() {
    let s = sched(1);
    let cancel = CancellationToken::new();
    let pause = s.note_rate_limited("fake", Some(Duration::from_millis(60)));
    assert_eq!(pause, Duration::from_millis(60));
    assert!(s.paused_for("fake").is_some());
    assert!(s.paused_for("other").is_none());
    let start = Instant::now();
    let _p = s.acquire_call("fake", &cancel).await.unwrap();
    assert!(start.elapsed() >= Duration::from_millis(50));
    assert_eq!(s.stats().rate_limits, 1);
}

fn limited(ms: u64) -> FakeTurn {
    let mut e = FakeError::new(FakeErrorKind::RateLimited, "slow down");
    e.retry_after_ms = Some(ms);
    FakeTurn::error(e)
}

#[tokio::test]
async fn rate_limited_call_is_retried_after_the_pause_instead_of_failing() {
    let fake = FakeProvider::with_turns([limited(40), limited(10), FakeTurn::text("done")]);
    let s = sched(1);
    let p = ScheduledProvider::new(Arc::new(fake.clone()), s.clone());
    let start = Instant::now();
    let stream = p
        .stream(ChatRequest::new("m"), CancellationToken::new())
        .await
        .unwrap();
    let resp = collect(stream).await.unwrap();
    assert_eq!(resp.message.text(), "done");
    assert_eq!(fake.requests().len(), 3);
    assert!(start.elapsed() >= Duration::from_millis(45));
    assert_eq!(resp.retries.len(), 2, "each wait is visible as a retry");
    assert_eq!(resp.retries[0].reason, RetryReason::RateLimited);
    assert_eq!(s.stats().rate_limits, 2);
    assert_eq!(s.stats().calls_running, 0);
}

#[tokio::test]
async fn rate_limit_retries_are_bounded_and_other_errors_pass_through() {
    let s = Scheduler::new(SchedulerConfig {
        max_rate_limit_waits: 1,
        ..SchedulerConfig::default()
    });
    let fake = FakeProvider::with_turns([limited(1), limited(1)]);
    let p = ScheduledProvider::new(Arc::new(fake.clone()), s.clone());
    let stream = p
        .stream(ChatRequest::new("m"), CancellationToken::new())
        .await
        .unwrap();
    let err = collect(stream).await.unwrap_err();
    assert!(is_rate_limit(&err));
    assert_eq!(fake.requests().len(), 2);

    let fake = FakeProvider::with_turns([FakeTurn::error(FakeError::new(
        FakeErrorKind::Auth,
        "bad key",
    ))]);
    let p = ScheduledProvider::new(Arc::new(fake.clone()), s);
    let stream = p
        .stream(ChatRequest::new("m"), CancellationToken::new())
        .await
        .unwrap();
    assert!(matches!(
        collect(stream).await.unwrap_err(),
        ProviderError::Auth(_)
    ));
    assert_eq!(fake.requests().len(), 1);
}

#[tokio::test]
async fn retry_events_from_the_transport_pause_the_provider() {
    let s = sched(1);
    let mut turn = FakeTurn::text("ok");
    turn.chunk_delay_ms = 0;
    let fake = FakeProvider::with_turns([turn]);
    let p = ScheduledProvider::new(Arc::new(fake), s.clone());
    // Simulate a transport retry signal directly.
    s.note_rate_limited("fake", Some(Duration::from_millis(20)));
    let stream = p
        .stream(ChatRequest::new("m"), CancellationToken::new())
        .await
        .unwrap();
    let resp = collect(stream).await.unwrap();
    assert_eq!(resp.message.text(), "ok");
}
