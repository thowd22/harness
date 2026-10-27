//! The startup update check: is there a newer harness release on GitHub?
//!
//! Only the interactive TUI runs it ([`crate::tui`]), in the background
//! after the session has started, and only with `[general] update_check`
//! on. Headless commands (`harness run`, `wf run`, `bench`, `eval`) never
//! call it.
//!
//! The check is one plain `GET {api}/repos/thowd22/harness/releases` with
//! no credentials or cookies (only harness's user agent). The newest
//! non-draft release is compared with the running version by semver;
//! pre-releases (`X.Y.Z-rc.N`) count only when the running build is a
//! pre-release itself.
//!
//! What it found is cached in `<data_dir>/update-check.json`
//! ([`harness_config::paths::DataDir::update_check`]): the request runs at
//! most once per `[general] update_check_interval`, and a failed request
//! (offline, rate-limited) counts as a check too, so an offline machine is
//! not asked again at every start. The cache also keeps the version the
//! user dismissed ([`dismiss`]); it is not offered again until a newer one
//! appears. Failures are logged and never shown.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use harness_providers::http::{HttpClient, HttpConfig};
use serde_json::{Value, json};

/// The GitHub repository releases are published to.
pub const REPO: &str = "thowd22/harness";

/// The GitHub API base URL.
pub const GITHUB_API: &str = "https://api.github.com";

/// Environment variable that replaces [`GITHUB_API`] (tests, mirrors).
pub const API_ENV: &str = "HARNESS_UPDATE_API";

/// The command that installs the latest release (as in the README).
pub const INSTALL_COMMAND: &str =
    "curl -fsSL https://raw.githubusercontent.com/thowd22/harness/main/install.sh | bash";

/// The running version.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// Connect timeout of the check.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Whole-request timeout of the check.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The API base: `$HARNESS_UPDATE_API` when set and non-empty, else
/// [`GITHUB_API`].
pub fn api_base() -> String {
    std::env::var(API_ENV)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| GITHUB_API.to_owned())
}

/// A semantic version (`1.2.3`, `1.2.3-rc.1`; build metadata ignored).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    pre: Vec<Pre>,
}

/// A pre-release identifier: numeric ones sort before alphanumeric ones.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Pre {
    Num(u64),
    Text(String),
}

impl Version {
    /// Parses `1.2.3`, `v1.2.3` or `1.2.3-rc.1+build`; `None` for anything
    /// else.
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim();
        let s = s.strip_prefix('v').unwrap_or(s);
        let s = s.split_once('+').map_or(s, |(v, _)| v);
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (s, None),
        };
        let mut nums = core.split('.').map(|n| {
            (!n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                .then(|| n.parse::<u64>().ok())
                .flatten()
        });
        let (major, minor, patch) = (nums.next()??, nums.next()??, nums.next()??);
        if nums.next().is_some() {
            return None;
        }
        let pre = match pre {
            None => Vec::new(),
            Some(p) => p
                .split('.')
                .map(|id| {
                    if id.is_empty() {
                        None
                    } else if id.bytes().all(|b| b.is_ascii_digit()) {
                        id.parse().ok().map(Pre::Num)
                    } else {
                        Some(Pre::Text(id.to_owned()))
                    }
                })
                .collect::<Option<Vec<_>>>()?,
        };
        Some(Version {
            major,
            minor,
            patch,
            pre,
        })
    }

    /// Whether this is a pre-release (`1.2.3-rc.1`).
    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }
}

impl Ord for Version {
    /// Semver precedence: a pre-release sorts before its release.
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => self.pre.cmp(&other.pre),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A published release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// Its version, without the `v` (`0.5.0`).
    pub version: String,
    /// Its release-notes page.
    pub url: String,
}

/// The newest stable release and the newest release of any kind in a
/// GitHub `releases` listing (drafts and unparseable tags skipped).
fn newest(listing: &Value) -> (Option<Release>, Option<Release>) {
    let mut stable: Option<(Version, Release)> = None;
    let mut any: Option<(Version, Release)> = None;
    for r in listing.as_array().into_iter().flatten() {
        if r["draft"].as_bool() == Some(true) {
            continue;
        }
        let Some(tag) = r["tag_name"].as_str() else {
            continue;
        };
        let Some(v) = Version::parse(tag) else {
            continue;
        };
        let url = r["html_url"].as_str().map_or_else(
            || format!("https://github.com/{REPO}/releases/tag/{tag}"),
            str::to_owned,
        );
        let release = Release {
            version: tag.trim().trim_start_matches('v').to_owned(),
            url,
        };
        let pre = v.is_prerelease() || r["prerelease"].as_bool() == Some(true);
        if !pre && stable.as_ref().is_none_or(|(s, _)| v > *s) {
            stable = Some((v.clone(), release.clone()));
        }
        if any.as_ref().is_none_or(|(a, _)| v > *a) {
            any = Some((v, release));
        }
    }
    (stable.map(|s| s.1), any.map(|a| a.1))
}

/// The cached state of the check.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cache {
    /// When GitHub was last asked (Unix seconds), whatever the outcome.
    pub checked_at: Option<u64>,
    /// The newest stable release found.
    pub stable: Option<Release>,
    /// The newest release found, pre-releases included.
    pub any: Option<Release>,
    /// The version the user dismissed (Esc in the dialog).
    pub dismissed: Option<String>,
}

impl Cache {
    /// Reads `path`; a missing or damaged file is an empty cache.
    pub fn load(path: &Path) -> Cache {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Cache::default();
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            tracing::info!(target: "harness::update", path = %path.display(), "ignoring a damaged update-check cache");
            return Cache::default();
        };
        let release = |v: &Value| {
            Some(Release {
                version: v["version"].as_str()?.to_owned(),
                url: v["url"].as_str()?.to_owned(),
            })
        };
        Cache {
            checked_at: v["checked_at"].as_u64(),
            stable: release(&v["stable"]),
            any: release(&v["any"]),
            dismissed: v["dismissed"].as_str().map(str::to_owned),
        }
    }

    /// Writes the cache to `path` (through a temporary file, so a crash
    /// never leaves half a file).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let release = |r: &Option<Release>| {
            r.as_ref()
                .map_or(Value::Null, |r| json!({"version": r.version, "url": r.url}))
        };
        let v = json!({
            "checked_at": self.checked_at,
            "stable": release(&self.stable),
            "any": release(&self.any),
            "dismissed": self.dismissed,
        });
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, format!("{v:#}\n"))?;
        std::fs::rename(&tmp, path)
    }

    /// Whether the last check is younger than `interval` at `now`.
    pub fn is_fresh(&self, now: u64, interval: Duration) -> bool {
        self.checked_at
            .is_some_and(|t| t <= now && now - t < interval.as_secs())
    }

    /// The release to offer a build at `current`: newer than it and than
    /// the dismissed version, if any.
    pub fn offer(&self, current: &Version) -> Option<Release> {
        let latest = if current.is_prerelease() {
            self.any.as_ref()
        } else {
            self.stable.as_ref()
        }?;
        let v = Version::parse(&latest.version)?;
        let dismissed = self.dismissed.as_deref().and_then(Version::parse);
        (v > *current && dismissed.is_none_or(|d| v > d)).then(|| latest.clone())
    }
}

/// A newer release for the running build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    /// The running version.
    pub current: String,
    /// The newer release.
    pub latest: Release,
}

/// What a check needs.
#[derive(Debug, Clone)]
pub struct Check {
    /// The GitHub API base ([`api_base`]).
    pub api: String,
    /// The cache file ([`harness_config::paths::DataDir::update_check`]).
    pub file: PathBuf,
    /// How long a check stays fresh (`[general] update_check_interval`).
    pub interval: Duration,
    /// The running version ([`CURRENT`]).
    pub current: String,
}

/// Seconds since the Unix epoch.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Runs the check at `now` (Unix seconds): asks GitHub unless the cache is
/// fresh, records the attempt, and returns the release to offer, if any.
/// Never fails: errors are logged.
pub async fn check(c: &Check, now: u64) -> Option<Update> {
    let Some(current) = Version::parse(&c.current) else {
        tracing::info!(target: "harness::update", version = %c.current, "not a semver version; no update check");
        return None;
    };
    let mut cache = Cache::load(&c.file);
    if !cache.is_fresh(now, c.interval) {
        match fetch(&c.api).await {
            Ok(listing) => {
                (cache.stable, cache.any) = newest(&listing);
                tracing::debug!(target: "harness::update", stable = ?cache.stable, any = ?cache.any, "update check done");
            }
            Err(e) => tracing::info!(target: "harness::update", error = %e, "update check failed"),
        }
        cache.checked_at = Some(now);
        if let Err(e) = cache.save(&c.file) {
            tracing::info!(target: "harness::update", error = %e, "update-check cache not written");
        }
    }
    cache.offer(&current).map(|latest| Update {
        current: c.current.clone(),
        latest,
    })
}

/// Remembers that the user dismissed `version`, so it is not offered again.
pub fn dismiss(file: &Path, version: &str) {
    let mut cache = Cache::load(file);
    cache.dismissed = Some(version.to_owned());
    if let Err(e) = cache.save(file) {
        tracing::info!(target: "harness::update", error = %e, "update-check cache not written");
    }
}

/// The releases listing: one plain GET, no retries.
async fn fetch(api: &str) -> Result<Value, String> {
    let mut cfg = HttpConfig::new(api);
    cfg.connect_timeout = CONNECT_TIMEOUT;
    cfg.request_timeout = REQUEST_TIMEOUT;
    let client = HttpClient::new(cfg).map_err(|e| e.to_string())?;
    let (status, body) = client
        .get_text(&format!("repos/{REPO}/releases?per_page=30"))
        .await
        .map_err(|e| e.to_string())?;
    if status != 200 {
        return Err(format!("HTTP {status}"));
    }
    serde_json::from_str(&body).map_err(|e| format!("bad releases listing: {e}"))
}

#[cfg(test)]
pub(crate) mod mock;
#[cfg(test)]
mod tests;
