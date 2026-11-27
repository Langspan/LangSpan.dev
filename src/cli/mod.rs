//! The `langspan` command's drivers and what it prints, in the library so
//! a test can call them as functions (issue #300). The binary's own
//! modules in this directory (`run.rs`, `help.rs` and each command's
//! driver) keep the argument dispatch and help text.

pub mod doctor;
pub mod options;
pub mod render;
pub mod setup;

use langspan_core::{Command, Outcome};

pub fn ask(
    state_dir: &std::path::Path,
    id: String,
    command: Command,
) -> Result<Outcome, crate::Error> {
    crate::landing::ask(state_dir, id, command).map(|response| response.outcome)
}
