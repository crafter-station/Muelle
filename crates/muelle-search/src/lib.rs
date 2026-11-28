//! The search window: a launcher for apps, settings, files, calculations and
//! definitions, opened by a global shortcut or the menu bar's search button.
//!
//! The dock runs it as a worker process of its own, so that neither can take
//! the other down, and it exits when the dock does.

// A test that cannot set itself up should stop there.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod app;
mod calc;
mod contents;
mod dictionary;
mod history;
mod hotkey;
mod icons;
mod index;
mod input;
mod render;
mod search;
mod shell;
mod strings;
mod textfield;

pub use app::run;

/// The argument of `muelle.exe` that starts the search process, followed by
/// the dock's process id.
pub const FLAG: &str = "--search-worker";
