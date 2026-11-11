#![allow(clippy::missing_errors_doc, clippy::must_use_candidate)]

pub mod answer;
pub mod cards;
pub mod cli;
pub mod db;
pub mod error;
pub mod instruct;
pub mod overlay;

pub use error::{Error, Result};

/// Print `value` as AYML to stdout.
pub(crate) fn print_ayml<T: serde::Serialize>(value: &T) -> Result<()> {
    let text = ayml::to_string(value).map_err(|e| Error::AymlSerialize(e.to_string()))?;
    print_stdout(&text)
}

/// Write `text` to stdout, and fail with [`Error::Stdout`] instead of a panic if stdout is closed.
pub(crate) fn print_stdout(text: &str) -> Result<()> {
    use std::io::Write as _;
    let mut out = std::io::stdout().lock();
    out.write_all(text.as_bytes())
        .and_then(|()| out.flush())
        .map_err(Error::Stdout)
}
pub mod discover;
pub mod event_log;
pub mod graph;
pub mod import;
#[cfg(feature = "mcp")]
pub mod mcp;
pub mod path;
pub mod progress;
pub mod scheduler;
pub mod schema;
pub mod state;
pub mod store;
pub mod subpath;
pub mod surface;
pub mod syllabus;
pub mod tree;
pub mod types;
pub mod view;
