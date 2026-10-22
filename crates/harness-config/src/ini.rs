//! `harness.ini` parsing, typed sections and unknown-key errors with line numbers.
//!
//! - [`document`]: strict lexer producing sections and entries with locations.
//! - [`value`]: typed accessors ([`SectionReader`]) and pure value parsers.
//! - [`config`]: the typed schema ([`Config`]) and built-in defaults.
//! - [`load`](mod@load): file lookup, permission check and `harness config check`.
//! - [`edit`]: line-preserving credential edits and private (`0600`) writes.
//! - [`error`]: [`IniError`] / [`IniErrors`], each carrying `file:line`.
//!
//! Typical use:
//!
//! ```no_run
//! let config = harness_config::ini::load().unwrap_or_else(|e| {
//!     eprintln!("{e}");
//!     std::process::exit(1)
//! });
//! println!("default model: {}", config.general.default_model);
//! ```

pub mod config;
pub mod document;
pub mod edit;
pub mod error;
pub mod load;
pub mod value;

pub use config::{Config, ProviderConfig, Remote};
pub use document::{Document, Entry, Section, parse};
pub use error::{IniError, IniErrors, Location};
pub use load::{load, load_file, load_location, load_str, run_check};
pub use value::{ConfigEnum, SectionReader, expand_tilde, parse_bool, parse_duration, parse_list};
