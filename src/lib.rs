//! Library entrypoint.
//!
//! The `mt` binary is a thin shell over these modules and
//! integration tests in `tests/` call into them directly.
//! `pub` here means "visible for tests," not "stable for external consumers."

#![allow(clippy::missing_errors_doc, clippy::must_use_candidate)]

pub mod answer;
pub mod cards;
pub mod cli;
pub mod db;
pub mod error;
pub mod instruct;
pub mod overlay;

pub use error::{Error, Result};
pub mod discover;
pub mod event_log;
pub mod graph;
#[cfg(feature = "mcp")]
pub mod mcp;
pub mod migrate;
pub mod path;
pub mod progress;
pub mod scheduler;
pub mod state;
pub mod store;
pub mod subpath;
pub mod syllabus;
pub mod tree;
pub mod types;
