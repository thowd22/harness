//! `harness ui-demo --print` plays the scripted session through the real
//! TUI, core and agent loop (fake provider) and prints the screens.

mod common;

use common::*;

#[test]
fn ui_demo_plays_the_scripted_session() {
    let env = Env::raw("");
    let o = env.cmd(&["ui-demo", "--print"]).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let out = stdout(&o);
    let section = |name: &str| -> String {
        let start = out
            .find(&format!("=== {name} ==="))
            .unwrap_or_else(|| panic!("no {name} screen:\n{out}"));
        let rest = &out[start + name.len() + 8..];
        rest[..rest.find("\n=== ").unwrap_or(rest.len())].to_owned()
    };
    let chat = section("chat");
    // Tool cards with a diff, live bash output, todo summaries.
    assert!(chat.contains("✔ edit  src/parse.rs  +1 −1"), "{chat}");
    assert!(chat.contains("✔ bash  $ sh check.sh"), "{chat}");
    assert!(chat.contains("test result: ok. 3 passed"), "{chat}");
    assert!(chat.contains("todo  update t2 → done"), "{chat}");
    // Indicators: the retry and the honest end-of-turn status.
    assert!(chat.contains("retrying in 1s (429 rate limited"), "{chat}");
    assert!(chat.contains("unverified: 1 file change"), "{chat}");
    // Timestamps (hh:mm:ss) on entries.
    let clock = |w: &[u8]| {
        w.len() == 8
            && w.iter().enumerate().all(|(i, c)| {
                if i == 2 || i == 5 {
                    *c == b':'
                } else {
                    c.is_ascii_digit()
                }
            })
    };
    assert!(chat.as_bytes().windows(8).any(clock), "{chat}");
    // The goal bar from the todo tool, on chat and map.
    assert!(chat.contains("Tasks") && chat.contains("2/2"), "{chat}");
    let map = section("map");
    assert!(map.contains("✔ Researcher"), "subagent on the map: {map}");
    assert!(map.contains("Find the header parser"), "{map}");
    assert!(section("palette").contains("Command palette"));
    assert!(section("approval").contains("answered: once"));
    let o = env.cmd(&["ui-demo", "--nope"]).output().unwrap();
    assert_eq!(o.status.code(), Some(1));
}
