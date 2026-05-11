//! Library entry. The `mt` binary is a thin shell over these modules;
//! integration tests in `tests/` call into them directly.
//!
//! This isn't a public API — `pub` here means "visible to the test
//! binary," not "stable for external consumers." The two pedantic
//! doc lints below would otherwise demand library-grade boilerplate
//! on every internal helper.

#![allow(clippy::missing_errors_doc, clippy::must_use_candidate)]

pub mod answer;
pub mod cards;
pub mod cli;
pub mod error;
pub mod overlay;

pub use error::{Error, Result};
pub mod discover;
pub mod event_log;
pub mod graph;
pub mod path;
pub mod scheduler;
pub mod state;
pub mod store;
pub mod tree;
pub mod types;
