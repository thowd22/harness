//! TASK-164: the startup update check in the driver, against a local mock
//! of the GitHub API. It runs in the background (starting it never waits
//! for the answer), a newer release reaches the UI as the update dialog,
//! failures send nothing, Esc's dismissal is recorded, the setting turns it
//! off, and starting a session never checks by itself.

use std::time::Instant;

use crate::update::mock::{MockApi, listing};

use super::*;

fn update_events(h: &Harness) -> Vec<harness_tui::update::UpdateNotice> {
    h.seen
        .iter()
        .filter_map(|e| match e {
            AppEvent::Core(CoreEvent::UpdateAvailable(n)) => Some(n.clone()),
            _ => None,
        })
        .collect()
}

/// Waits for the update check task (the last background task).
async fn check_done(h: &mut Harness) {
    let task = h.d.background.pop().expect("a check is running");
    task.await.unwrap();
    // Collect what it sent.
    while let Ok(ev) = h.rx.try_recv() {
        h.seen.push(ev);
    }
}

#[tokio::test]
async fn a_newer_release_opens_the_dialog_without_waiting_for_github() {
    let env = Env::new();
    let script = env.script(TEXT);
    let mut h = Harness::start(&env, &script).await;
    let api = MockApi::held(200, &listing(&[("v99.0.0", false)])).await;
    h.d.update_api = api.url.clone();

    let started = Instant::now();
    h.d.spawn_update_check();
    assert!(
        started.elapsed() < Duration::from_millis(200),
        "starting the check does not wait for it"
    );
    // The request is out, unanswered; the driver keeps serving the UI.
    let deadline = Instant::now() + Duration::from_secs(10);
    while api.heads().is_empty() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(api.heads().len(), 1);
    h.d.request(UiRequest::Submit("hello".into())).await;
    h.finish_turn().await;
    assert!(update_events(&h).is_empty());

    api.release();
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::UpdateAvailable(_))))
        .await;
    assert_eq!(
        update_events(&h),
        vec![harness_tui::update::UpdateNotice {
            current: crate::update::CURRENT.into(),
            latest: "99.0.0".into(),
            url: "https://github.com/thowd22/harness/releases/tag/v99.0.0".into(),
            command: crate::update::INSTALL_COMMAND.into(),
        }]
    );
    let file = env.root.join("data").join("update-check.json");
    assert!(file.exists(), "the result is cached in the data dir");

    // Esc in the dialog: the version is not offered again.
    h.d.request(UiRequest::DismissUpdate("99.0.0".into())).await;
    assert_eq!(
        crate::update::Cache::load(&file).dismissed.as_deref(),
        Some("99.0.0")
    );
    h.seen.clear();
    h.d.spawn_update_check();
    check_done(&mut h).await;
    assert!(update_events(&h).is_empty());
    assert_eq!(api.heads().len(), 1, "the cached check is still fresh");
}

#[tokio::test]
async fn failed_checks_send_nothing_to_the_ui() {
    for (status, body) in [(403, r#"{"message":"API rate limit exceeded"}"#), (502, "")] {
        let env = Env::new();
        let script = env.script(TEXT);
        let mut h = Harness::start(&env, &script).await;
        let api = MockApi::start(status, body).await;
        h.d.update_api = api.url.clone();
        h.seen.clear();
        h.d.spawn_update_check();
        check_done(&mut h).await;
        assert_eq!(api.heads().len(), 1);
        let shown: Vec<_> = h
            .seen
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    AppEvent::Core(
                        CoreEvent::UpdateAvailable(_)
                            | CoreEvent::Notify { .. }
                            | CoreEvent::Panel { .. }
                    )
                )
            })
            .collect();
        assert!(shown.is_empty(), "{status}: nothing shown: {shown:?}");
    }
}

#[tokio::test]
async fn the_setting_turns_the_check_off_and_sessions_never_check_by_themselves() {
    let env = Env::new();
    let script = env.script(TEXT);
    let api = MockApi::start(200, &listing(&[("v99.0.0", false)])).await;
    let mut h = Harness::with_ini(&env, &script, false, "[general]\nupdate_check = off\n").await;
    h.d.update_api = api.url.clone();
    let before = h.d.background.len();
    h.d.spawn_update_check();
    assert_eq!(h.d.background.len(), before, "no task with the check off");

    // On, but only `run` starts it: a session start (`/new`) does not.
    let mut h = Harness::start(&env, &script).await;
    h.d.update_api = api.url.clone();
    h.d.request(UiRequest::Submit("/new".into())).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(api.heads().is_empty());
    assert!(!env.root.join("data").join("update-check.json").exists());
}
