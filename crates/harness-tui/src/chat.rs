//! The chat view: a virtualized transcript of the main agent's work.
//!
//! - [`copy`]: copy palette entries and the selection-mode bar.
//! - [`text`]: display-width aware layout and number formatting.
//! - [`timefmt`]: `[ui] timestamps` formatting from event times.
//! - [`highlight`]: lazily loaded lexical syntax highlighting.
//! - [`markdown`]: streaming Markdown rendering with a block cache.
//! - [`transcript`]: the virtualized transcript folded from run events.
//! - [`view`]: [`view::ChatView`], every chat component composed with input routing.

pub mod copy;
pub mod highlight;
pub mod markdown;
pub mod text;
pub mod timefmt;
pub mod transcript;
pub mod view;
