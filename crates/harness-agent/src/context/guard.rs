//! Preventing "context anxiety".
//!
//! Models wrap up early when they believe the context is nearly full. The
//! harness therefore never tells the model how much context is left: no
//! request carries token counts, percentages or "remaining context"
//! notices (compaction handoffs and elision stubs included), and the
//! default prompts tell the model that the harness manages context.

/// Prompt text for default agents: the harness manages context, so the
/// model keeps working at a steady pace.
pub const CONTEXT_PROMPT_SECTION: &str = "\
The harness manages the conversation's length for you: older tool output \
may be shortened to a pointer you can re-read, and long histories are \
condensed into a handoff that keeps the goal, decisions and next steps. \
Work at a steady pace and keep going until the task is complete.";

const CONTEXT_WORDS: [&str; 3] = ["context", "token", "window"];
const AMOUNT_WORDS: [&str; 9] = [
    "remaining",
    "left",
    "used",
    "usage",
    "full",
    "percent",
    "%",
    "budget",
    "limit",
];

/// True when `text` tells the reader how much context is used or left
/// (a sentence that mentions context or tokens, an amount word and a
/// number), e.g. "12,000 tokens remaining" or "context 80% full".
pub fn mentions_remaining_context(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.split(['.', '!', '?', '\n']).any(|sentence| {
        sentence.chars().any(|c| c.is_ascii_digit())
            && CONTEXT_WORDS.iter().any(|w| sentence.contains(w))
            && AMOUNT_WORDS.iter().any(|w| sentence.contains(w))
    })
}
