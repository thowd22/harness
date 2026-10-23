//! Human-readable names for agents, runs and sessions.
//!
//! Everything a person picks from (the palette, the run history, the map
//! header, `harness runs show`, the `/resume` picker) is named with these
//! helpers so the same thing reads the same everywhere:
//!
//! - [`agent_display_name`]: an agent's `display_name` from its YAML, as
//!   recorded in `run_started.agent_names`, else [`default_display_name`].
//! - [`title_from_text`]: a short title from a first prompt (first line,
//!   markdown markers stripped, ids replaced by tags, cut on a word).
//! - [`title_from_inputs`]: the same from workflow inputs.
//! - [`when_label`]: `14:32 Oct 23` (with the year when it is not this
//!   year).
//! - [`session_name`]: a short session name (a few words) from a first
//!   prompt.
//! - [`activity_label`]: when a session was last active, relative for
//!   recent days (`14:32 today`, `yesterday 09:10`, `Oct 21 16:05`).
//! - [`short_tag`]: `#1a2b3c`, the last random hex digits of an id, shown
//!   only to tell apart two entries whose labels collide
//!   ([`disambiguate`]).
//! - [`find_raw_id`]: finds UUIDs and long hashes, for audits that keep
//!   raw ids out of rendered screens.
//!
//! Ids stay secondary: views show titles, and fall back to a tag only when
//! two labels would otherwise be identical.

use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;
use std::sync::LazyLock;

use regex::Regex;
use time::{OffsetDateTime, UtcOffset};

use crate::schema::RunStatus;

/// Longest title, in characters, before it is cut with `…`.
pub const TITLE_MAX: usize = 48;

/// Hex digits in a [`short_tag`].
pub const TAG_LEN: usize = 6;

static UUID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b")
        .expect("uuid regex")
});

static HASH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b[0-9a-f]{16,}\b").expect("hash regex"));

/// The display name used when an agent's YAML sets none: the name with
/// `-` and `_` as spaces and the first letter capitalised (`coder` →
/// `Coder`, `bugfix-proof` → `Bugfix proof`).
pub fn default_display_name(agent: &str) -> String {
    let spaced: String = agent
        .trim()
        .chars()
        .map(|c| if c == '-' || c == '_' { ' ' } else { c })
        .collect();
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The display name of `agent`: its entry in `names` (from the run's
/// `run_started.agent_names`, i.e. the agent YAML's `display_name`) or
/// [`default_display_name`].
pub fn agent_display_name(agent: &str, names: &BTreeMap<String, String>) -> String {
    names
        .get(agent)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| default_display_name(agent))
}

/// Replaces every UUID in `text` with its [`short_tag`].
pub fn scrub_ids(text: &str) -> String {
    UUID.replace_all(text, |c: &regex::Captures<'_>| short_tag(&c[0]))
        .into_owned()
}

/// A short title from free text such as a first prompt: the first
/// non-empty line, without leading markdown markers (`#`, `>`, `-`, `*`),
/// whitespace collapsed, UUIDs replaced by tags, trailing `.`/`:`/`,`
/// dropped and cut on a word boundary at [`TITLE_MAX`] characters.
/// `None` when nothing readable is left.
pub fn title_from_text(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|l| {
        !l.trim_start_matches(['#', '>', '-', '*', '`', ' '])
            .is_empty()
    })?;
    let line = line.trim_start_matches(['#', '>', '-', '*', ' ']);
    let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
    let scrubbed = scrub_ids(&collapsed);
    let trimmed = scrubbed.trim_end_matches(['.', ':', ',', ';', ' ']);
    if trimmed.is_empty() {
        return None;
    }
    Some(cut(trimmed, TITLE_MAX))
}

/// A title from workflow inputs, in the order given: the first non-empty
/// string value as [`title_from_text`], else `key=value` pairs of the
/// scalar inputs. `None` when there are no usable inputs.
pub fn title_from_inputs<'a>(
    inputs: impl IntoIterator<Item = (&'a str, &'a serde_json::Value)>,
) -> Option<String> {
    let inputs: Vec<(&str, &serde_json::Value)> = inputs.into_iter().collect();
    if let Some(t) = inputs
        .iter()
        .filter_map(|(_, v)| v.as_str())
        .find_map(title_from_text)
    {
        return Some(t);
    }
    let pairs: Vec<String> = inputs
        .iter()
        .filter(|(_, v)| v.is_number() || v.is_boolean())
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    (!pairs.is_empty()).then(|| cut(&pairs.join(" "), TITLE_MAX))
}

/// `s` cut on a word boundary to at most `max` characters, ending in `…`
/// when anything was cut.
pub fn shorten(s: &str, max: usize) -> String {
    cut(s, max)
}

fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let head: String = s.chars().take(max.saturating_sub(1)).collect();
    let head = match head.rfind(' ') {
        Some(i) if i > max / 2 => &head[..i],
        _ => head.as_str(),
    };
    format!("{}…", head.trim_end_matches([' ', '.', ',', ':', ';']))
}

/// `#` and the last [`TAG_LEN`] hex digits of `id` (the random part of a
/// UUID v7, so ids created in the same millisecond still differ). Ids
/// without enough hex digits are returned whole after the `#`.
pub fn short_tag(id: &str) -> String {
    let hex: Vec<char> = id.chars().filter(char::is_ascii_hexdigit).collect();
    if hex.len() < TAG_LEN || hex.len() != id.chars().filter(|c| *c != '-').count() {
        return format!("#{id}");
    }
    let tail: String = hex[hex.len() - TAG_LEN..]
        .iter()
        .collect::<String>()
        .to_lowercase();
    format!("#{tail}")
}

/// Whether `query` (with or without the leading `#`) is the tag of `id`.
pub fn matches_tag(id: &str, query: &str) -> bool {
    let q = query.trim_start_matches('#').to_lowercase();
    !q.is_empty() && short_tag(id).trim_start_matches('#') == q
}

/// When something happened, for lists: `14:32 Oct 23`, with the year
/// (`14:32 Oct 23 2025`) when `ts` is not in `now`'s year. Shown in
/// `offset`.
pub fn when_label(ts: OffsetDateTime, offset: UtcOffset, now: OffsetDateTime) -> String {
    let t = ts.to_offset(offset);
    let month = &format!("{:?}", t.month())[..3];
    let mut s = format!("{:02}:{:02} {month} {}", t.hour(), t.minute(), t.day());
    if t.year() != now.to_offset(offset).year() {
        s.push_str(&format!(" {}", t.year()));
    }
    s
}

/// Most words in a [`session_name`].
pub const SESSION_NAME_WORDS: usize = 5;

/// Longest [`session_name`], in characters.
pub const SESSION_NAME_MAX: usize = 32;

/// Name of a session with no readable prompt yet.
pub const UNNAMED_SESSION: &str = "New session";

/// Openers dropped from the start of a prompt before naming it.
const FILLER_OPENERS: &[&str] = &[
    "hi there",
    "hey there",
    "hello there",
    "hi all",
    "hello all",
    "hi everyone",
    "hello everyone",
    "please",
    "pls",
    "hey",
    "hi",
    "hello",
    "ok",
    "okay",
    "so",
    "now",
    "can you",
    "could you",
    "would you",
    "will you",
    "can we",
    "could we",
    "i want you to",
    "i want to",
    "i need you to",
    "i need to",
    "i'd like you to",
    "i would like you to",
    "i'd like to",
    "help me",
    "let's",
    "lets",
];

/// Words left out of a session name.
const NAME_STOPWORDS: &[&str] = &["a", "an", "the", "some", "this", "that", "my", "our"];

/// Words a session name never ends on: prepositions, articles,
/// conjunctions and pronouns that would leave it mid-phrase.
const DANGLING: &[&str] = &[
    "a", "an", "the", "some", "this", "that", "these", "those", "my", "our", "your", "its",
    "their", "in", "on", "at", "to", "for", "of", "with", "from", "by", "into", "onto", "about",
    "over", "under", "after", "before", "between", "through", "without", "within", "via", "as",
    "per", "and", "or", "but", "so", "if", "when", "while", "because", "then", "than", "nor",
    "where", "which", "who", "whose", "whether", "why", "how", "what", "it", "they", "we", "i",
    "you", "he", "she", "is", "are", "was", "were", "be", "been", "do", "does", "not",
];

/// A short session name from free text such as a first prompt: the first
/// readable line ([`title_from_text`]), first letter capitalised,
/// [`UNNAMED_SESSION`] when nothing readable is left.
///
/// - A prompt that is already short ([`SESSION_NAME_WORDS`] words,
///   [`SESSION_NAME_MAX`] characters) and is not a request behind an
///   opener stays whole: `hi there` → `Hi there`.
/// - Otherwise openers like `please` or `can you` and articles are
///   dropped, as long as a real request is left.
/// - A name that is still too long is cut on a word, and never ends on a
///   preposition, article or conjunction.
///
/// `"Please fix the header parser so it keeps colons"` → `Fix header
/// parser`; `"Fix flaky login test in the auth module"` → `Fix flaky
/// login test`.
pub fn session_name(text: &str) -> String {
    let Some(title) = title_from_text(text) else {
        return UNNAMED_SESSION.to_owned();
    };
    let title = title.trim_end_matches('…');
    let words: Vec<&str> = title
        .split_whitespace()
        .map(|w| w.trim_matches([',', '.', ';', ':', '!', '?', '"', '\'', '`', '(', ')']))
        .filter(|w| !w.is_empty())
        .collect();
    let is_stop = |w: &&str| NAME_STOPWORDS.contains(&w.to_lowercase().as_str());
    if words.is_empty() || words.iter().all(is_stop) {
        return UNNAMED_SESSION.to_owned();
    }
    let request: Vec<&str> = drop_openers(&words)
        .iter()
        .copied()
        .filter(|w| !is_stop(w))
        .collect();
    let fits = |ws: &[&str]| {
        ws.len() <= SESSION_NAME_WORDS && ws.join(" ").chars().count() <= SESSION_NAME_MAX
    };
    // A greeting or a one-word remainder ("hi there", "ok thanks") is
    // better kept whole than cut down to its last word.
    let chosen = if request.is_empty() || (request.len() < 2 && fits(&words)) {
        words
    } else {
        request
    };
    capitalise(&fit_words(&chosen))
}

/// `words` with leading openers (`ok so can you ...`) removed, unless
/// nothing would be left.
fn drop_openers<'a>(words: &[&'a str]) -> Vec<&'a str> {
    let mut words = words.to_vec();
    loop {
        let lower: Vec<String> = words.iter().map(|w| w.to_lowercase()).collect();
        let hit = FILLER_OPENERS.iter().find_map(|f| {
            let fw: Vec<&str> = f.split(' ').collect();
            (lower.len() > fw.len() && lower.iter().zip(&fw).all(|(a, b)| a == b))
                .then_some(fw.len())
        });
        match hit {
            Some(n) => {
                words.drain(..n);
            }
            None => return words,
        }
    }
}

/// At most [`SESSION_NAME_WORDS`] of `words` within [`SESSION_NAME_MAX`]
/// characters, without trailing [`DANGLING`] words. A single word that is
/// too long is cut with `…`.
fn fit_words(words: &[&str]) -> String {
    let mut kept: Vec<&str> = Vec::new();
    let mut len = 0;
    for w in words.iter().take(SESSION_NAME_WORDS) {
        let add = w.chars().count() + usize::from(!kept.is_empty());
        if !kept.is_empty() && len + add > SESSION_NAME_MAX {
            break;
        }
        kept.push(w);
        len += add;
    }
    let cut_short = kept.len() < words.len();
    while cut_short && kept.len() > 1 {
        let last = kept[kept.len() - 1].to_lowercase();
        if !DANGLING.contains(&last.as_str()) {
            break;
        }
        kept.pop();
    }
    cut(&kept.join(" "), SESSION_NAME_MAX)
}

/// `s` with its first letter upper-cased.
fn capitalise(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// When a session was last active, for session lists, in `offset`:
///
/// - `14:32 today` on `now`'s day,
/// - `yesterday 09:10` on the day before,
/// - `Oct 21 16:05` earlier (or later) in `now`'s year,
/// - `Oct 21 2025 16:05` in another year.
///
/// Days are calendar days in `offset`, so `23:59` and `00:01` are on
/// different days.
pub fn activity_label(ts: OffsetDateTime, offset: UtcOffset, now: OffsetDateTime) -> String {
    let t = ts.to_offset(offset);
    let today = now.to_offset(offset).date();
    let hm = format!("{:02}:{:02}", t.hour(), t.minute());
    if t.date() == today {
        return format!("{hm} today");
    }
    if today.previous_day() == Some(t.date()) {
        return format!("yesterday {hm}");
    }
    let month = &format!("{:?}", t.month())[..3];
    if t.year() == today.year() {
        format!("{month} {} {hm}", t.day())
    } else {
        format!("{month} {} {} {hm}", t.day(), t.year())
    }
}

/// The status glyph of a run: `◐` running, `✔` succeeded, `✗` failed,
/// `⊘` cancelled.
pub fn status_glyph(status: RunStatus) -> char {
    match status {
        RunStatus::Running => '◐',
        RunStatus::Succeeded => '✔',
        RunStatus::Failed => '✗',
        RunStatus::Cancelled => '⊘',
    }
}

/// Makes labels distinct: every `(label, id)` whose label appears more
/// than once gets ` #tag` ([`short_tag`] of its id) appended. Labels that
/// are already unique are left alone.
pub fn disambiguate(items: &mut [(String, String)]) {
    let labels: Vec<String> = items.iter().map(|(l, _)| l.clone()).collect();
    for ((label, id), dup) in items.iter_mut().zip(collisions(&labels)) {
        if dup {
            label.push(' ');
            label.push_str(&short_tag(id));
        }
    }
}

/// For each key, whether another key in `keys` equals it.
pub fn collisions<T: Eq + Hash>(keys: &[T]) -> Vec<bool> {
    let mut count: HashMap<&T, usize> = HashMap::new();
    for k in keys {
        *count.entry(k).or_default() += 1;
    }
    keys.iter().map(|k| count[k] > 1).collect()
}

/// The first UUID or long hex hash (16 or more hex digits) in `text`.
/// Audits use it to keep raw ids out of rendered screens.
pub fn find_raw_id(text: &str) -> Option<&str> {
    UUID.find(text)
        .or_else(|| HASH.find(text))
        .map(|m| m.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use time::macros::datetime;

    #[test]
    fn default_display_names_read_as_words() {
        assert_eq!(default_display_name("coder"), "Coder");
        assert_eq!(default_display_name("bugfix-proof"), "Bugfix proof");
        assert_eq!(default_display_name("deep_research"), "Deep research");
        assert_eq!(default_display_name(""), "");
    }

    #[test]
    fn yaml_display_names_win_over_defaults() {
        let names = BTreeMap::from([("coder".to_owned(), "Code Smith".to_owned())]);
        assert_eq!(agent_display_name("coder", &names), "Code Smith");
        assert_eq!(agent_display_name("reviewer", &names), "Reviewer");
        let blank = BTreeMap::from([("coder".to_owned(), "  ".to_owned())]);
        assert_eq!(agent_display_name("coder", &blank), "Coder");
    }

    #[test]
    fn titles_come_from_the_first_readable_line() {
        assert_eq!(
            title_from_text("\n\n## Fix the header parser.\nIt breaks on colons").as_deref(),
            Some("Fix the header parser")
        );
        assert_eq!(
            title_from_text("> fix   the\theader").as_deref(),
            Some("fix the header")
        );
        assert_eq!(title_from_text("  \n---\n").as_deref(), None);
        let long =
            "please refactor the configuration loader so that every layer reports precise errors";
        let t = title_from_text(long).unwrap();
        assert!(t.ends_with('…') && t.chars().count() <= TITLE_MAX, "{t}");
        assert_eq!(t, "please refactor the configuration loader so…");
    }

    #[test]
    fn titles_never_carry_raw_ids() {
        let t = title_from_text("rerun 0199f1a2-3b4c-7d5e-8f60-123456abcdef please").unwrap();
        assert_eq!(t, "rerun #abcdef please");
        assert!(find_raw_id(&t).is_none());
    }

    #[test]
    fn titles_from_workflow_inputs() {
        let issue = json!("Header parser drops values after a colon");
        let n = json!(3);
        assert_eq!(
            title_from_inputs([("n", &n), ("issue", &issue)]).as_deref(),
            Some("Header parser drops values after a colon")
        );
        assert_eq!(title_from_inputs([("n", &n)]).as_deref(), Some("n=3"));
        assert_eq!(title_from_inputs([]), None);
    }

    #[test]
    fn tags_are_short_and_match_queries() {
        let id = "0199f1a2-3b4c-7d5e-8f60-123456ABCDEF";
        assert_eq!(short_tag(id), "#abcdef");
        assert!(matches_tag(id, "#abcdef") && matches_tag(id, "ABCDEF"));
        assert!(!matches_tag(id, "abcde") && !matches_tag(id, ""));
        assert_eq!(short_tag("main"), "#main");
    }

    #[test]
    fn when_labels_show_time_and_day() {
        let now = datetime!(2026-10-23 18:00 UTC);
        let ts = datetime!(2026-10-23 14:32:10 UTC);
        assert_eq!(when_label(ts, UtcOffset::UTC, now), "14:32 Oct 23");
        let east = UtcOffset::from_hms(2, 0, 0).unwrap();
        assert_eq!(when_label(ts, east, now), "16:32 Oct 23");
        let old = datetime!(2025-01-02 09:05 UTC);
        assert_eq!(when_label(old, UtcOffset::UTC, now), "09:05 Jan 2 2025");
    }

    #[test]
    fn session_names_are_a_few_words() {
        assert_eq!(
            session_name("Please fix the header parser so it keeps colons and quotes"),
            "Fix header parser"
        );
        assert_eq!(
            session_name("ok so can you add a /rename command"),
            "Add /rename command"
        );
        assert_eq!(
            session_name("## refactor config loader."),
            "Refactor config loader"
        );
        assert_eq!(session_name("  \n---\n"), UNNAMED_SESSION);
        assert_eq!(session_name("please"), "Please");
        assert_eq!(session_name("the a an"), UNNAMED_SESSION);
        let long = session_name("supercalifragilisticexpialidocious extraordinarily verbose");
        assert!(long.chars().count() <= SESSION_NAME_MAX, "{long}");
        let id = session_name("rerun 0199f1a2-3b4c-7d5e-8f60-123456abcdef now");
        assert_eq!(id, "Rerun #abcdef now");
    }

    #[test]
    fn session_names_read_well() {
        // Short prompts stay whole; a greeting is not cut down to "There".
        assert_eq!(session_name("hi there"), "Hi there");
        assert_eq!(session_name("hello everyone!"), "Hello everyone");
        assert_eq!(session_name("ok thanks"), "Ok thanks");
        // Openers go when a real request follows.
        assert_eq!(session_name("hi there, can you fix the tests"), "Fix tests");
        assert_eq!(session_name("please fix bugs"), "Fix bugs");
        // Long prompts are cut on a word, never on a dangling
        // preposition, article or conjunction.
        assert_eq!(
            session_name("Fix flaky login test in the auth module when CI is slow"),
            "Fix flaky login test"
        );
        assert_eq!(
            session_name("refactor the parser and the lexer so errors carry spans"),
            "Refactor parser and lexer"
        );
        assert_eq!(
            session_name("add retries to the HTTP client for transient failures"),
            "Add retries to HTTP client"
        );
        for prompt in [
            "Fix flaky login test in the auth module",
            "move the cache into a separate crate with its own tests",
            "investigate why the build is slow on macOS runners",
        ] {
            let name = session_name(prompt);
            let last = name.rsplit(' ').next().unwrap().to_lowercase();
            assert!(!DANGLING.contains(&last.as_str()), "{prompt:?} -> {name:?}");
            assert!(!name.ends_with('…'), "{prompt:?} -> {name:?}");
            assert!(name.chars().count() <= SESSION_NAME_MAX, "{name}");
            assert!(name.starts_with(|c: char| c.is_uppercase()), "{name}");
        }
    }

    #[test]
    fn activity_labels_are_relative_for_recent_days() {
        let now = datetime!(2026-10-23 18:00 UTC);
        let utc = UtcOffset::UTC;
        assert_eq!(
            activity_label(datetime!(2026-10-23 14:32:59 UTC), utc, now),
            "14:32 today"
        );
        assert_eq!(
            activity_label(datetime!(2026-10-22 09:10 UTC), utc, now),
            "yesterday 09:10"
        );
        assert_eq!(
            activity_label(datetime!(2026-10-21 16:05 UTC), utc, now),
            "Oct 21 16:05"
        );
        assert_eq!(
            activity_label(datetime!(2025-10-21 16:05 UTC), utc, now),
            "Oct 21 2025 16:05"
        );
        // Day boundaries: one minute either side of midnight.
        assert_eq!(
            activity_label(datetime!(2026-10-23 00:00 UTC), utc, now),
            "00:00 today"
        );
        assert_eq!(
            activity_label(datetime!(2026-10-22 23:59 UTC), utc, now),
            "yesterday 23:59"
        );
        assert_eq!(
            activity_label(datetime!(2026-10-21 23:59 UTC), utc, now),
            "Oct 21 23:59"
        );
        // New year: Dec 31 is yesterday, Dec 30 shows its year.
        let jan1 = datetime!(2027-01-01 08:00 UTC);
        assert_eq!(
            activity_label(datetime!(2026-12-31 22:00 UTC), utc, jan1),
            "yesterday 22:00"
        );
        assert_eq!(
            activity_label(datetime!(2026-12-30 22:00 UTC), utc, jan1),
            "Dec 30 2026 22:00"
        );
    }

    #[test]
    fn activity_labels_use_the_local_day_not_utc() {
        // 23:30 UTC on Oct 22 is 01:30 on Oct 23 in UTC+2: today there.
        let now = datetime!(2026-10-23 10:00 UTC);
        let ts = datetime!(2026-10-22 23:30 UTC);
        let east = UtcOffset::from_hms(2, 0, 0).unwrap();
        assert_eq!(activity_label(ts, east, now), "01:30 today");
        assert_eq!(activity_label(ts, UtcOffset::UTC, now), "yesterday 23:30");
        // West of UTC the same instant is yesterday afternoon, and `now`
        // early on Oct 23 UTC is still Oct 22 there.
        let west = UtcOffset::from_hms(-7, 0, 0).unwrap();
        assert_eq!(activity_label(ts, west, now), "yesterday 16:30");
        let early = datetime!(2026-10-23 03:00 UTC);
        assert_eq!(
            activity_label(datetime!(2026-10-22 06:00 UTC), west, early),
            "yesterday 23:00"
        );
        // Half-hour offsets.
        let india = UtcOffset::from_hms(5, 30, 0).unwrap();
        assert_eq!(
            activity_label(datetime!(2026-10-23 14:32 UTC), india, now),
            "20:02 today"
        );
    }

    #[test]
    fn only_colliding_labels_get_tags() {
        let mut v = vec![
            ("Coder · fix".to_owned(), "0199-aaaaaa111111".to_owned()),
            ("Coder · fix".to_owned(), "0199-bbbbbb222222".to_owned()),
            ("Coder · other".to_owned(), "0199-cccccc333333".to_owned()),
        ];
        disambiguate(&mut v);
        assert_eq!(v[0].0, "Coder · fix #111111");
        assert_eq!(v[1].0, "Coder · fix #222222");
        assert_eq!(v[2].0, "Coder · other");
    }

    #[test]
    fn raw_ids_are_found() {
        assert!(find_raw_id("run 0199f1a2-3b4c-7d5e-8f60-123456abcdef").is_some());
        assert!(find_raw_id("commit 3f2a9c1d4e5b6a7f8091").is_some());
        assert!(find_raw_id("Coder · fix #abcdef · 14:32 Oct 23").is_none());
        assert!(find_raw_id("cost $0.0123 tokens 1234567").is_none());
    }
}
