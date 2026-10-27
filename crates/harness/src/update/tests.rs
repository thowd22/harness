//! The update check against a local mock of the GitHub API: semver and
//! pre-release rules, the cache interval, dismissed versions, and failures
//! that stay silent.

use super::mock::{MockApi, listing};
use super::*;

const DAY: Duration = Duration::from_secs(86_400);
const NOW: u64 = 1_800_000_000;

fn v(s: &str) -> Version {
    Version::parse(s).unwrap_or_else(|| panic!("{s}"))
}

fn check_at(api: &str, dir: &Path, current: &str) -> Check {
    Check {
        api: api.to_owned(),
        file: dir.join("update-check.json"),
        interval: DAY,
        current: current.to_owned(),
    }
}

#[test]
fn versions_compare_by_semver_precedence() {
    assert!(v("0.10.0") > v("0.9.9"));
    assert!(v("v1.0.0") > v("0.99.99"));
    assert_eq!(v("v0.4.0"), v("0.4.0"));
    assert_eq!(v("0.4.0+build.7"), v("0.4.0"));
    // The semver spec's example order.
    let order = [
        "1.0.0-alpha",
        "1.0.0-alpha.1",
        "1.0.0-alpha.beta",
        "1.0.0-beta",
        "1.0.0-beta.2",
        "1.0.0-beta.11",
        "1.0.0-rc.1",
        "1.0.0",
    ];
    for w in order.windows(2) {
        assert!(v(w[0]) < v(w[1]), "{} < {}", w[0], w[1]);
    }
    assert!(v("0.5.0-rc.1").is_prerelease() && !v("0.5.0").is_prerelease());
    for bad in [
        "",
        "1",
        "1.2",
        "1.2.3.4",
        "x.1.2",
        "1.2.3-",
        "1.2.3-rc..1",
        "nightly",
    ] {
        assert_eq!(Version::parse(bad), None, "{bad:?}");
    }
}

#[test]
fn pre_releases_count_only_for_a_pre_release_build() {
    let l: Value = serde_json::from_str(&listing(&[
        ("v0.6.0-rc.1", true),
        ("v0.5.0", false),
        ("v0.4.0", false),
        ("not-a-version", false),
    ]))
    .unwrap();
    let (stable, any) = newest(&l);
    let cache = Cache {
        stable,
        any,
        ..Cache::default()
    };
    assert_eq!(cache.offer(&v("0.4.0")).unwrap().version, "0.5.0");
    assert_eq!(cache.offer(&v("0.5.0-rc.2")).unwrap().version, "0.6.0-rc.1");
    assert_eq!(
        cache.offer(&v("0.5.0-rc.2")).unwrap().url,
        "https://github.com/thowd22/harness/releases/tag/v0.6.0-rc.1"
    );
    // Up to date, or ahead (a dev build).
    assert_eq!(cache.offer(&v("0.5.0")), None);
    assert_eq!(cache.offer(&v("0.7.0")), None);
    // A tag with a pre-release suffix is a pre-release even if GitHub's
    // flag is not set, and drafts never count.
    let l: Value = serde_json::from_str(
        r#"[{"tag_name": "v0.9.0-rc.1", "prerelease": false},
            {"tag_name": "v0.8.0", "draft": true, "prerelease": false}]"#,
    )
    .unwrap();
    assert_eq!(newest(&l).0, None);
}

#[test]
fn a_dismissed_version_is_offered_again_only_when_a_newer_one_appears() {
    let rel = |v: &str| Release {
        version: v.into(),
        url: format!("https://example.com/{v}"),
    };
    let mut cache = Cache {
        stable: Some(rel("0.5.0")),
        dismissed: Some("0.5.0".into()),
        ..Cache::default()
    };
    assert_eq!(cache.offer(&v("0.4.0")), None);
    cache.stable = Some(rel("0.5.1"));
    assert_eq!(cache.offer(&v("0.4.0")), Some(rel("0.5.1")));
}

#[test]
fn the_cache_round_trips_and_damage_is_an_empty_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("sub").join("update-check.json");
    let cache = Cache {
        checked_at: Some(NOW),
        stable: Some(Release {
            version: "0.5.0".into(),
            url: "https://example.com".into(),
        }),
        any: None,
        dismissed: Some("0.4.9".into()),
    };
    cache.save(&file).unwrap();
    assert_eq!(Cache::load(&file), cache);
    std::fs::write(&file, "{not json").unwrap();
    assert_eq!(Cache::load(&file), Cache::default());
    assert_eq!(Cache::load(&tmp.path().join("missing")), Cache::default());
    // Fresh for the interval only, and not when the clock went back.
    assert!(cache.is_fresh(NOW + DAY.as_secs() - 1, DAY));
    assert!(!cache.is_fresh(NOW + DAY.as_secs(), DAY));
    assert!(!cache.is_fresh(NOW - 1, DAY));
}

#[tokio::test]
async fn a_newer_release_is_found_with_one_plain_get() {
    let api = MockApi::start(200, &listing(&[("v0.5.0", false), ("v0.4.0", false)])).await;
    let tmp = tempfile::tempdir().unwrap();
    let c = check_at(&api.url, tmp.path(), "0.4.0");
    let u = check(&c, NOW).await.expect("an update");
    assert_eq!(u.current, "0.4.0");
    assert_eq!(u.latest.version, "0.5.0");
    assert_eq!(
        u.latest.url,
        "https://github.com/thowd22/harness/releases/tag/v0.5.0"
    );
    let heads = api.heads();
    assert_eq!(heads.len(), 1);
    let head = heads[0].to_ascii_lowercase();
    assert!(
        head.starts_with("get /repos/thowd22/harness/releases?per_page=30 http/1.1\r\n"),
        "{head}"
    );
    // Nothing but a plain GET: no credentials or cookies.
    for h in ["authorization", "cookie", "x-api-key"] {
        assert!(!head.contains(h), "{h}: {head}");
    }
    assert!(head.contains("user-agent: harness/"), "{head}");
}

#[tokio::test]
async fn the_check_runs_at_most_once_per_interval() {
    let api = MockApi::start(200, &listing(&[("v0.5.0", false)])).await;
    let tmp = tempfile::tempdir().unwrap();
    let mut c = check_at(&api.url, tmp.path(), "0.4.0");
    assert!(check(&c, NOW).await.is_some());
    // Within the interval the cached result answers.
    assert!(check(&c, NOW + 3600).await.is_some());
    assert!(check(&c, NOW + DAY.as_secs() - 1).await.is_some());
    assert_eq!(api.heads().len(), 1);
    // After it, GitHub is asked again.
    assert!(check(&c, NOW + DAY.as_secs()).await.is_some());
    assert_eq!(api.heads().len(), 2);
    // The interval is configurable.
    c.interval = Duration::from_secs(60);
    assert!(check(&c, NOW + DAY.as_secs() + 60).await.is_some());
    assert_eq!(api.heads().len(), 3);
}

#[tokio::test]
async fn a_dismissed_version_is_not_offered_again_until_a_newer_one() {
    let tmp = tempfile::tempdir().unwrap();
    let api = MockApi::start(200, &listing(&[("v0.5.0", false)])).await;
    let c = check_at(&api.url, tmp.path(), "0.4.0");
    assert!(check(&c, NOW).await.is_some());
    dismiss(&c.file, "0.5.0");
    assert_eq!(check(&c, NOW + 60).await, None);
    // Still dismissed after the next fetch finds the same release.
    assert_eq!(check(&c, NOW + 2 * DAY.as_secs()).await, None);
    // A newer release shows again.
    let api = MockApi::start(200, &listing(&[("v0.5.1", false)])).await;
    let c = Check {
        api: api.url.clone(),
        ..c
    };
    let u = check(&c, NOW + 4 * DAY.as_secs()).await.unwrap();
    assert_eq!(u.latest.version, "0.5.1");
}

#[tokio::test]
async fn failures_are_silent_and_still_count_as_a_check() {
    for (status, body) in [
        (403, r#"{"message": "API rate limit exceeded"}"#),
        (429, "{}"),
        (500, "oops"),
        (200, "<html>not json</html>"),
    ] {
        let api = MockApi::start(status, body).await;
        let tmp = tempfile::tempdir().unwrap();
        let c = check_at(&api.url, tmp.path(), "0.4.0");
        assert_eq!(check(&c, NOW).await, None, "{status}");
        assert_eq!(check(&c, NOW + 60).await, None, "{status}");
        assert_eq!(
            api.heads().len(),
            1,
            "{status}: no retry within the interval"
        );
    }
    // Offline: nothing listens.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let tmp = tempfile::tempdir().unwrap();
    let c = check_at(&format!("http://127.0.0.1:{port}"), tmp.path(), "0.4.0");
    assert_eq!(check(&c, NOW).await, None);
    assert_eq!(Cache::load(&c.file).checked_at, Some(NOW));
    // A failed refresh keeps what an earlier check found.
    let api = MockApi::start(200, &listing(&[("v0.5.0", false)])).await;
    let tmp = tempfile::tempdir().unwrap();
    let mut c = check_at(&api.url, tmp.path(), "0.4.0");
    assert!(check(&c, NOW).await.is_some());
    c.api = format!("http://127.0.0.1:{port}");
    assert!(check(&c, NOW + 2 * DAY.as_secs()).await.is_some());
}
