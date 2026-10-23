//! Built-in default prompts, agents and workflows, compiled into the binary.
//!
//! These are the [`Scope::Builtin`](crate::discovery::Scope::Builtin) layer of
//! discovery: pass [`BUILTINS`] to
//! [`Roots::resolve`](crate::discovery::Roots::resolve). A global or project
//! file with the same `name` shadows a built-in one, so every default is
//! overridable.
//!
//! | Kind | Name | Purpose |
//! |---|---|---|
//! | agent | `coder` | default agent: read, write, edit, bash, todo; read-write memory |
//! | agent | `researcher` | read-only investigation (read, bash; read memory) |
//! | agent | `reviewer` | independent review (read, bash; no memory) |
//! | prompt | `coder`, `researcher`, `reviewer` | their system prompts; each starts with the built-in identity layer ([`crate::prompts::IDENTITY`]) |
//! | workflow | `dream` | memory consolidation settings (see `harness_memory::dream`) |
//! | workflow | `implement-verify-review`, `fix-bug`, `plan-approve-implement` | the bundled workflows (`/ivr`, `/fix`, `/plan`); documented and tested in `harness_workflow::bundled` |
//!
//! Defaults determine most users' experience, so they are short, use positive
//! scope ("you may ...") and encode the known fixes: verify before claiming
//! done, stage files by name, keep changes minimal, clean up temporary files.
//! Changing any file here needs a `CHANGELOG.md` "Defaults" entry (see
//! `docs/defaults.md`).

use sha2::{Digest, Sha256};

use crate::discovery::{BuiltinFile, Roots};

macro_rules! builtin {
    ($path:literal) => {
        BuiltinFile {
            path: $path,
            contents: include_str!(concat!("../assets/", $path)),
        }
    };
}

/// Every built-in file, with paths relative to a scope root.
pub static BUILTINS: &[BuiltinFile] = &[
    builtin!("prompts/base.md"),
    builtin!("prompts/coder.md"),
    builtin!("prompts/memory.md"),
    builtin!("prompts/context.md"),
    builtin!("prompts/tools.md"),
    builtin!("prompts/coder.yaml"),
    builtin!("prompts/researcher.yaml"),
    builtin!("prompts/reviewer.yaml"),
    builtin!("agents/coder.yaml"),
    builtin!("agents/researcher.yaml"),
    builtin!("agents/reviewer.yaml"),
    builtin!("workflows/dream.yaml"),
    builtin!("workflows/implement-verify-review.yaml"),
    builtin!("workflows/fix-bug.yaml"),
    builtin!("workflows/plan-approve-implement.yaml"),
];

/// Names of the built-in agents.
pub const AGENTS: &[&str] = &["coder", "researcher", "reviewer"];

/// Name of the default agent.
pub const DEFAULT_AGENT: &str = "coder";

/// Name of the built-in memory consolidation workflow.
pub const DREAM_WORKFLOW: &str = "dream";

/// Path of the memory guidance layer included by prompts of agents with
/// `memory: read-write`.
pub const MEMORY_LAYER: &str = "prompts/memory.md";

/// Roots containing only the built-in files (no global or project scope).
pub fn roots() -> Roots {
    Roots::new(BUILTINS)
}

/// The built-in file at `path`, if any.
pub fn file(path: &str) -> Option<&'static BuiltinFile> {
    BUILTINS.iter().find(|f| f.path == path)
}

/// A stable fingerprint of every built-in file (sha256 hex over sorted
/// `path NUL contents NUL` records, followed by the compiled-in identity
/// layer as `builtin:identity NUL text NUL`). It changes whenever a default
/// changes, which is what the defaults changelog check keys on.
pub fn fingerprint() -> String {
    let mut files: Vec<&BuiltinFile> = BUILTINS.iter().collect();
    files.sort_by_key(|f| f.path);
    let mut h = Sha256::new();
    for f in files {
        h.update(f.path.as_bytes());
        h.update([0]);
        h.update(f.contents.as_bytes());
        h.update([0]);
    }
    h.update(b"builtin:identity");
    h.update([0]);
    h.update(crate::prompts::IDENTITY.as_bytes());
    h.update([0]);
    hex::encode(h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{MemoryAccess, ValidationContext};
    use crate::discovery::{Catalog, Scope};
    use crate::ini::Config;
    use crate::lint::{self, LintOptions};
    use crate::prompts::{PromptVars, SessionPrompt};

    fn vars() -> PromptVars {
        PromptVars {
            cwd: "/work/app".into(),
            os: "linux".into(),
            date: "2026-10-22".into(),
            git_branch: "main".into(),
        }
    }

    #[test]
    fn builtins_load_without_errors() {
        let c = Catalog::load(&roots());
        assert!(c.errors().is_empty(), "{:?}", c.errors());
        for a in AGENTS {
            let e = c.agents.get_entry(a).expect(a);
            assert_eq!(e.origin.scope, Scope::Builtin);
        }
        for p in ["coder", "researcher", "reviewer"] {
            assert!(c.prompts.get(p).is_some(), "{p}");
        }
        assert!(c.workflows.get(DREAM_WORKFLOW).is_some());
        assert!(
            c.validate_agents(&ValidationContext::from_config(&Config::defaults()))
                .is_empty()
        );
    }

    #[test]
    fn defaults_pass_prompt_lint() {
        let c = Catalog::load(&roots());
        let ctx = ValidationContext::from_config(&Config::defaults());
        let report = lint::lint_catalog(&c, Some(&ctx), &[], &LintOptions::default());
        assert!(report.findings.is_empty(), "{}", report.summary());
    }

    #[test]
    fn defaults_fit_the_initial_context_budget() {
        let c = Catalog::load(&roots());
        let budget = LintOptions::default().prompt_token_budget;
        for p in c.prompts.active() {
            let text = SessionPrompt::new(&p.item, vars())
                .unwrap()
                .text()
                .to_owned();
            let tokens = lint::estimate_tokens(&text);
            // Keep well under the lint budget: the system prompt shares the
            // initial context with tool schemas and the memory index.
            assert!(
                tokens * 4 <= budget,
                "prompt {} is {tokens} tokens",
                p.item.name
            );
        }
    }

    #[test]
    fn coder_has_todo_and_its_prompt_explains_it() {
        let c = Catalog::load(&roots());
        let coder = c.agents.get(DEFAULT_AGENT).unwrap();
        assert!(coder.tools.iter().any(|t| t == "todo"));
        let text = SessionPrompt::new(c.prompts.get("coder").unwrap(), vars())
            .unwrap()
            .text()
            .to_owned();
        assert!(text.contains("`todo` tool"));
        assert!(text.contains("several steps"));
        assert!(text.contains("one-step tasks"));
    }

    #[test]
    fn defaults_encode_the_known_fixes() {
        let c = Catalog::load(&roots());
        let text = SessionPrompt::new(c.prompts.get("coder").unwrap(), vars())
            .unwrap()
            .text()
            .to_lowercase();
        for needle in [
            "done only when the checks you ran pass", // verify before claiming done
            "stage files by name",                    // no broad git add
            "as small as the task allows",            // minimal changes
            "remove any you created",                 // temp files
        ] {
            assert!(text.contains(needle), "missing: {needle}");
        }
    }

    #[test]
    fn read_write_memory_agents_include_the_memory_layer() {
        let c = Catalog::load(&roots());
        let layer = file(MEMORY_LAYER).unwrap().contents.trim();
        for a in c.agents.active() {
            let crate::agents::PromptRef::Named(p) = &a.item.prompt else {
                panic!("built-in agents use named prompts");
            };
            let text = c.prompts.get(p).unwrap().raw_text();
            assert_eq!(
                a.item.memory == MemoryAccess::ReadWrite,
                text.contains(layer),
                "agent {}",
                a.item.name
            );
        }
    }

    #[test]
    fn identity_is_the_first_layer_of_every_builtin_agent_prompt() {
        use crate::prompts::{IDENTITY, LayerSource};
        let c = Catalog::load(&roots());
        for a in c.agents.active() {
            let crate::agents::PromptRef::Named(p) = &a.item.prompt else {
                panic!("built-in agents use named prompts");
            };
            let prompt = c.prompts.get(p).unwrap();
            assert_eq!(
                prompt.layers[0].source,
                LayerSource::Builtin("identity".into()),
                "agent {}",
                a.item.name
            );
            let text = SessionPrompt::new(prompt, vars()).unwrap();
            assert!(text.text().starts_with(IDENTITY), "agent {}", a.item.name);
        }
    }

    #[test]
    fn identity_prompt_off_removes_the_layer_and_survives_reload() {
        use crate::prompts::IDENTITY;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("prompts");
        std::fs::create_dir_all(&dir).unwrap();
        let custom = dir.join("mine.yaml");
        std::fs::write(
            &custom,
            "name: mine\nlayers:\n  - builtin: identity\n  - text: Mine\n",
        )
        .unwrap();
        let r = roots().with_global(tmp.path());
        let mut c = Catalog::load(&r);
        assert!(c.identity_prompt());
        let render = |c: &Catalog, p: &str| {
            SessionPrompt::new(c.prompts.get(p).unwrap(), vars())
                .unwrap()
                .text()
                .to_owned()
        };
        assert_eq!(render(&c, "mine"), format!("{IDENTITY}\n\nMine"));
        c.set_identity_prompt(false);
        assert!(!c.identity_prompt());
        for p in ["coder", "researcher", "reviewer", "mine"] {
            assert!(!render(&c, p).contains(IDENTITY), "{p}");
        }
        // A changed file is re-parsed on reload and still follows the setting.
        std::fs::write(
            &custom,
            "name: mine\nlayers:\n  - builtin: identity\n  - text: Mine 2\n",
        )
        .unwrap();
        c.reload(&r);
        assert_eq!(render(&c, "mine"), "Mine 2");
        assert!(!render(&c, "coder").contains(IDENTITY));
        c.set_identity_prompt(true);
        assert!(render(&c, "coder").starts_with(IDENTITY));
    }

    #[test]
    fn builtins_are_overridable_by_global_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("agents");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("coder.yaml"),
            "name: coder\nprompt: coder\ntools: [read]\n",
        )
        .unwrap();
        let c = Catalog::load(&roots().with_global(tmp.path()));
        let e = c.agents.get_entry("coder").unwrap();
        assert_eq!(e.origin.scope, Scope::Global);
        assert_eq!(e.item.tools, ["read"]);
        assert_eq!(c.agents.shadowed().len(), 1);
    }

    #[test]
    fn fingerprint_is_stable_and_content_sensitive() {
        assert_eq!(fingerprint(), fingerprint());
        assert_eq!(fingerprint().len(), 64);
        assert!(file("agents/coder.yaml").is_some());
        assert!(file("agents/missing.yaml").is_none());
    }
}
