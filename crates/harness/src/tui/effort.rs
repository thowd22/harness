//! `/effort`: the reasoning effort of the session's agent.
//!
//! `/effort minimal|low|medium|high|xhigh|max|ultra` sets it for the rest
//! of the session (the next request carries it and the session file keeps
//! it for resume), `/effort default` restores the agent YAML's `effort` or
//! the model default, and a bare `/effort` opens a panel with the active
//! effort, where it comes from and how this model's provider receives it,
//! with one button per level the model accepts. A level the model does not
//! accept is sent as the nearest one it does, and the command says so. On
//! a model that does not reason the effort is kept but nothing is sent.

use harness_config::agents::Effort;
use harness_providers::anthropic::request::effort_budget;
use harness_providers::model::ReasoningEffort;
use harness_providers::registry::ThinkingStyle;
use harness_tui::app::CoreEvent;
use harness_tui::notify::Level;
use harness_tui::panel::PanelButton;

use super::Driver;
use crate::core::{Core, EffortState, effort_name};

/// Title of the `/effort` panel.
pub const TITLE: &str = "Reasoning effort";
/// Panel button id prefix: `effort.set:<choice>`.
pub const BUTTON_PREFIX: &str = "effort.set:";
/// The `/effort` choices, in picker order.
pub const CHOICES: [&str; 8] = [
    "default", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
];

/// Parses a choice: `Some(None)` is `default`.
pub fn parse(choice: &str) -> Option<Option<Effort>> {
    if choice.trim().eq_ignore_ascii_case("default") {
        return Some(None);
    }
    Effort::parse(choice).map(Some)
}

/// The panel's choices: `default` and the levels the model accepts (every
/// level when they are unknown or the model does not reason).
fn panel_choices(state: &EffortState) -> Vec<&'static str> {
    let levels: Vec<Effort> = if state.reasons && !state.efforts.is_empty() {
        state.efforts.clone()
    } else {
        Effort::ALL.to_vec()
    };
    std::iter::once("default")
        .chain(levels.into_iter().map(Effort::as_str))
        .collect()
}

/// Whether `id` is an `/effort` panel button.
pub fn is_action(id: &str) -> bool {
    id.starts_with(BUTTON_PREFIX)
}

/// The status-line event for `core`.
pub fn status_event(core: &Core) -> CoreEvent {
    CoreEvent::Effort(core.effort().status())
}

/// `/effort [choice]`.
pub(super) async fn command(d: &mut Driver, choice: Option<&str>) {
    let choice = choice.map(str::trim).filter(|c| !c.is_empty());
    if d.busy() {
        let line = match choice {
            Some(c) => format!("/effort {c}"),
            None => "/effort".to_owned(),
        };
        d.queue.push_back(line);
        return d
            .notify(
                Level::Info,
                "effort change queued",
                Some("after this turn".into()),
            )
            .await;
    }
    match choice {
        None => open(d).await,
        Some(c) => match parse(c) {
            Some(effort) => set(d, effort, false).await,
            None => {
                d.notify(
                    Level::Error,
                    "unknown effort",
                    Some(format!("`{c}`: use one of {}", CHOICES.join(", "))),
                )
                .await
            }
        },
    }
}

/// A panel button.
pub(super) async fn action(d: &mut Driver, id: &str) {
    let Some(effort) = id.strip_prefix(BUTTON_PREFIX).and_then(parse) else {
        return;
    };
    if d.busy() {
        return command(d, id.strip_prefix(BUTTON_PREFIX)).await;
    }
    set(d, effort, true).await;
}

async fn open(d: &mut Driver) {
    let Some(core) = d.core.as_ref() else {
        return;
    };
    let lines = lines(core);
    let buttons = panel_choices(&core.effort())
        .into_iter()
        .map(|c| PanelButton::new(format!("{BUTTON_PREFIX}{c}"), c))
        .collect();
    d.send(CoreEvent::Panel {
        title: TITLE.into(),
        lines,
        buttons,
    })
    .await;
}

async fn set(d: &mut Driver, effort: Option<Effort>, from_panel: bool) {
    let Some(core) = d.core.as_mut() else {
        return;
    };
    let state = match core.set_effort(effort) {
        Ok(s) => s,
        Err(e) => return d.notify(Level::Error, "effort", Some(e)).await,
    };
    let model = core.model.reference.clone();
    let lines = lines(core);
    d.send(CoreEvent::Effort(state.status())).await;
    if from_panel {
        d.send(CoreEvent::PanelText {
            title: TITLE.into(),
            lines,
        })
        .await;
    }
    match (state.effective(), state.reasons) {
        (Some(_), false) => {
            d.notify(
                Level::Warning,
                &format!("effort {}: not sent", state.label()),
                Some(format!(
                    "{model} does not reason, so no reasoning fields are sent; \
                     the effort applies after /model switches to a model that reasons"
                )),
            )
            .await
        }
        (Some(e), true) if state.sent() != Some(e) => {
            let sent = effort_name(state.sent());
            d.notify(
                Level::Info,
                &format!("effort {}: sent as {sent}", state.label()),
                Some(format!(
                    "{model} does not take {}; the nearest level it takes is {sent}",
                    state.label()
                )),
            )
            .await
        }
        _ => {
            d.notify(
                Level::Info,
                &format!("effort {}", state.label()),
                Some(state.source().to_owned()),
            )
            .await
        }
    }
}

/// The panel text: agent, model, active effort with its source, and how
/// the provider receives it.
pub fn lines(core: &Core) -> Vec<String> {
    let state = core.effort();
    let mut v = vec![
        format!("agent   {}", core.agent.name),
        format!("model   {}", core.model.reference),
        format!("effort  {} ({})", state.sent_label(), state.source()),
    ];
    if state.session.is_some() {
        v.push(format!(
            "        default would be: {}",
            effort_name(state.agent)
        ));
    }
    v.push(String::new());
    v.push(wire(&state, &core.model.reference));
    if state.reasons && !state.efforts.is_empty() {
        let names: Vec<&str> = state.efforts.iter().map(|e| e.as_str()).collect();
        v.push(format!(
            "This model takes {}; other levels are sent as the nearest of these.",
            names.join(", ")
        ));
    }
    v.push(String::new());
    v.push("default  the agent YAML's effort, else the model's own".into());
    v.push("minimal  barely any thinking (OpenAI, OpenRouter)".into());
    v.push("low      little thinking: fast and cheap".into());
    v.push("medium   balanced".into());
    v.push("high     thorough".into());
    v.push("xhigh    more than high (Claude 4.7+, newer OpenAI models)".into());
    v.push("max      the most a Claude model offers".into());
    v.push("ultra    above max, where a model offers it".into());
    v
}

/// How this model's provider receives the effort.
fn wire(state: &EffortState, reference: &str) -> String {
    if !state.reasons {
        return "This model does not reason: no reasoning fields are sent at any effort.".into();
    }
    match state.style {
        ThinkingStyle::Adaptive => "Sent as adaptive thinking with output_config.effort.".into(),
        ThinkingStyle::Budget => format!(
            "Sent as a thinking token budget: low {}, medium {}, high {}, xhigh {}, max {}.",
            effort_budget(ReasoningEffort::Low),
            effort_budget(ReasoningEffort::Medium),
            effort_budget(ReasoningEffort::High),
            effort_budget(ReasoningEffort::XHigh),
            effort_budget(ReasoningEffort::Max),
        ),
        ThinkingStyle::Effort | ThinkingStyle::None => {
            if reference.starts_with("openrouter/") || reference.starts_with("openai/") {
                "Sent as reasoning.effort.".into()
            } else {
                "Sent as the provider's reasoning effort.".into()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choices_parse() {
        assert_eq!(parse("default"), Some(None));
        assert_eq!(parse(" LOW "), Some(Some(Effort::Low)));
        assert_eq!(parse("medium"), Some(Some(Effort::Medium)));
        assert_eq!(parse("high"), Some(Some(Effort::High)));
        assert_eq!(parse("XHigh"), Some(Some(Effort::XHigh)));
        assert_eq!(parse("ultra"), Some(Some(Effort::Ultra)));
        assert_eq!(parse("extreme"), None);
        for c in CHOICES {
            assert!(parse(c).is_some(), "{c}");
        }
        assert_eq!(CHOICES.len(), Effort::ALL.len() + 1);
    }

    #[test]
    fn panel_offers_the_levels_the_model_takes() {
        let mut s = EffortState {
            session: Some(Effort::Ultra),
            agent: None,
            reasons: true,
            style: ThinkingStyle::Adaptive,
            efforts: vec![Effort::Low, Effort::Medium, Effort::High, Effort::Max],
        };
        assert_eq!(
            panel_choices(&s),
            ["default", "low", "medium", "high", "max"]
        );
        assert_eq!(s.sent(), Some(Effort::Max));
        assert_eq!(s.sent_label(), "ultra (sent as max)");
        assert_eq!(s.status().as_deref(), Some("effort ultra→max"));
        s.session = Some(Effort::XHigh);
        assert_eq!(s.sent(), Some(Effort::High));
        s.session = Some(Effort::Minimal);
        assert_eq!(s.sent(), Some(Effort::Low));
        s.session = Some(Effort::Medium);
        assert_eq!(s.status().as_deref(), Some("effort medium"));
        s.efforts.clear();
        assert_eq!(panel_choices(&s).len(), CHOICES.len());
    }

    #[test]
    fn wire_lines_name_the_provider_mapping() {
        let s = |style, reasons| EffortState {
            session: Some(Effort::Low),
            agent: None,
            reasons,
            style,
            efforts: Vec::new(),
        };
        assert!(wire(&s(ThinkingStyle::Adaptive, true), "anthropic/x").contains("output_config"));
        assert!(wire(&s(ThinkingStyle::Budget, true), "anthropic/x").contains("2048"));
        assert!(wire(&s(ThinkingStyle::None, true), "openrouter/x").contains("reasoning.effort"));
        assert!(wire(&s(ThinkingStyle::Effort, true), "openai/x").contains("reasoning.effort"));
        assert!(wire(&s(ThinkingStyle::None, false), "openai/x").contains("does not reason"));
    }
}
