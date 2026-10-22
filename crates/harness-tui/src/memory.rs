//! Memory UI: the inline "Updated memory" card ([`card`]) and the
//! `/memory` browser ([`browser`]). Both are pure views; the core performs
//! the drive operations they request (see `docs/memory-ui.md`).

pub mod browser;
pub mod card;
