//! TASK-87: every built-in agent prompt tells the model that the harness
//! manages the conversation length, so it keeps going, without exposing how
//! much context remains.

use harness_agent::context::mentions_remaining_context;
use harness_config::agents::PromptRef;
use harness_config::builtins;
use harness_config::discovery::Catalog;
use harness_config::prompts::{PromptVars, SessionPrompt};

#[test]
fn default_prompts_say_the_harness_handles_context() {
    let catalog = Catalog::load(&builtins::roots());
    let vars = PromptVars {
        cwd: "/tmp/project".into(),
        os: "linux".into(),
        date: "2026-10-22".into(),
        git_branch: "main".into(),
    };
    for name in builtins::AGENTS {
        let agent = catalog.agents.get(name).expect("built-in agent");
        let PromptRef::Named(prompt) = &agent.prompt else {
            panic!("{name} uses a named prompt");
        };
        let prompt = catalog.prompts.get(prompt).expect("built-in prompt");
        let text = SessionPrompt::new(prompt, vars.clone())
            .unwrap()
            .text()
            .to_owned();
        assert!(
            text.contains("The harness manages the conversation's length"),
            "{name}: prompt does not explain context management"
        );
        assert!(
            text.contains("keep going until the task is complete"),
            "{name}: prompt does not ask the model to keep going"
        );
        assert!(
            !mentions_remaining_context(&text),
            "{name}: prompt exposes remaining context"
        );
    }
}
