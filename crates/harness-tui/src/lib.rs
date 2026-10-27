//! `harness-tui`: ratatui application: chat, map, detail, palette, approvals, diff and terminal handling.
//!
//! See `backlog/docs/doc-1 - Architecture-and-Problem-Map.md` for crate boundaries.

pub mod app;
pub mod approval;
pub mod cards;
pub mod chat;
pub mod clipboard;
pub mod demo;
pub mod detail;
pub mod diff;
pub mod fuzzy;
pub mod goalbar;
pub mod history;
pub mod indicators;
pub mod input;
pub mod inspector;
pub mod interaction;
pub mod item_detail;
pub mod keys;
pub mod layout;
pub mod login;
pub mod map;
pub mod memory;
pub mod model_picker;
pub mod notify;
pub mod palette;
pub mod panel;
pub mod selection;
pub mod session_picker;
pub mod signin;
pub mod status;
pub mod terminal;
pub mod theme;
pub mod update;

#[cfg(test)]
mod layout_matrix;
