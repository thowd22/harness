//! Approval requests, scopes, session-pattern approvals and autonomy modes.
//!
//! - [`ModeState`] picks the autonomy [`Mode`] from harness.ini, the command
//!   palette and the running agent (an agent can only tighten it).
//! - [`ApprovalBroker`] turns `ask` verdicts into one prompt per batch of
//!   parallel tool calls, records `approval_requested` / `approval_resolved`
//!   events and counts prompts.
//! - [`ApprovalScope`] is what the user chose: once, this session for a
//!   command pattern ([`SessionGrants`]), or always, which yields an
//!   [`IniRuleProposal`] that is written to harness.ini only after the user
//!   confirms it ([`IniRuleProposal::confirm`]).

mod broker;
mod grants;
mod ini_edit;
mod mode;

pub use broker::{
    ApprovalAnswer, ApprovalBroker, ApprovalEvents, ApprovalMetrics, ApprovalPrompt,
    ApprovalRequest, ApprovalResolution, ApprovalScope, ApprovalUi, ChannelUi, NoApprovalEvents,
    PendingApproval, PromptEnvelope, RecorderApprovalEvents, count_prompts, suggest_pattern,
};
pub use grants::SessionGrants;
pub use ini_edit::{ALWAYS_KEY, IniEditError, IniRuleProposal, append_rule};
pub use mode::{ModeState, PALETTE_MODE_PREFIX, PaletteModeEntry, YOLO_WARNING, palette_entries};

pub use crate::rules::Mode;
