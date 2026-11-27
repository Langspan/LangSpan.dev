//! LangSpan adapters. The domain crate has no dependency on these modules.

pub mod adopt;
pub mod auto_check;
pub mod autostart;
pub mod between_steps;
pub mod binary_version;
pub mod breaks;
pub mod claude_code;
pub mod client;
pub mod clock;
pub mod commit;
pub mod count;
pub mod daemon;
pub mod dashboard;
pub mod definitions;
pub mod demo;
pub mod formatter;
pub mod guidance;
pub mod install;
mod lines;
mod local;
pub mod local_day;
pub mod mcp;
pub mod notify;
pub mod protocol;
pub mod recent;
mod reformat;
pub mod regions;
mod registration;
pub mod report;
pub mod service;
mod sha1;
mod sha256;
pub mod since;
pub mod skill;
pub mod status_line;
pub mod store;
pub mod test_output;
pub mod today;
pub mod trail;
pub mod user_install;
mod wait;
mod zone;

pub type Error = Box<dyn std::error::Error + Send + Sync>;

pub mod hook;

pub mod hosts;

pub mod git;

pub mod codex;

pub mod cursor;

pub mod gemini_removed;

pub mod antigravity;

pub mod process;

pub mod observer;

pub mod remote;

pub mod shell;

pub mod journal;

pub mod spool;

pub mod graph;
pub mod index_file;
pub mod integrate;
pub mod landing;
pub mod rewrite;
