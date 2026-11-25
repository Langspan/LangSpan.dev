//! LangSpan adapters. The domain crate has no dependency on these modules.

pub mod auto_check;
pub mod breaks;
pub mod claude_code;
pub mod client;
pub mod clock;
pub mod commit;
pub mod daemon;
pub mod dashboard;
pub mod formatter;
pub mod install;
mod lines;
mod local;
pub mod local_day;
pub mod mcp;
pub mod protocol;
pub mod recent;
mod reformat;
pub mod regions;
mod sha1;
mod sha256;
pub mod skill;
pub mod status_line;
pub mod store;
pub mod test_output;
pub mod today;
pub mod user_install;

pub type Error = Box<dyn std::error::Error + Send + Sync>;

pub mod hook;

pub mod hosts;

pub mod git;

pub mod codex;

pub mod cursor;

pub mod gemini;

pub mod process;

pub mod observer;

pub mod remote;

pub mod shell;

pub mod journal;

pub mod spool;

pub mod graph;
pub mod integrate;
pub mod landing;
