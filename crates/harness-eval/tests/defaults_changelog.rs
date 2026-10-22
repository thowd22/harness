//! CI gate (TASK-115): changing a built-in prompt, agent, workflow or a
//! harness.ini default requires a `## Defaults` entry in CHANGELOG.md that
//! names the new defaults fingerprint.

#[test]
fn changelog_records_the_current_defaults() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../CHANGELOG.md");
    let text = std::fs::read_to_string(&path).expect("CHANGELOG.md at the repository root");
    if let Err(e) = harness_eval::defaults::check_changelog(&text) {
        panic!("{e}");
    }
}
