//! `harness-providers`: Normalized message model, `Provider` trait, concrete providers, model registry, pricing and HTTP/SSE/retry plumbing.
//!
//! See `backlog/docs/doc-1 - Architecture-and-Problem-Map.md` for crate boundaries.

pub mod anthropic;
pub mod cache_hint;
pub mod catalog;
pub mod chatgpt;
pub mod fake;
pub mod http;
pub mod login;
pub mod model;
pub mod openai;
pub mod openrouter;
pub mod pricing;
pub mod probe;
pub mod provider;
pub mod registry;
pub mod retry;
pub mod signin;
pub mod sse;
pub mod timing;
