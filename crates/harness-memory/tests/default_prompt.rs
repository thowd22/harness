//! The bundled default prompt for `memory: read-write` agents teaches the
//! memory triggers (TASK-93 AC#1): its memory layer is exactly
//! `extract::PROMPT_SECTION`.

use harness_config::agents::MemoryAccess;
use harness_config::builtins;
use harness_config::discovery::Catalog;
use harness_config::prompts::{PromptVars, SessionPrompt};
use harness_memory::extract::PROMPT_SECTION;

#[test]
fn memory_layer_matches_prompt_section() {
    let layer = builtins::file(builtins::MEMORY_LAYER).expect("memory layer");
    assert_eq!(layer.contents.trim(), PROMPT_SECTION);
}

#[test]
fn read_write_default_agents_render_the_memory_section() {
    let catalog = Catalog::load(&builtins::roots());
    let mut checked = 0;
    for agent in catalog.agents.active() {
        if agent.item.memory != MemoryAccess::ReadWrite {
            continue;
        }
        let harness_config::agents::PromptRef::Named(name) = &agent.item.prompt else {
            panic!("built-in agents use named prompts");
        };
        let prompt = catalog.prompts.get(name).unwrap();
        let text = SessionPrompt::new(prompt, PromptVars::detect(std::path::Path::new("/tmp")))
            .unwrap()
            .text()
            .to_owned();
        assert!(text.contains(PROMPT_SECTION), "agent {}", agent.item.name);
        checked += 1;
    }
    assert!(
        checked >= 1,
        "the default coder agent has read-write memory"
    );
}
