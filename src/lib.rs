//! LangSpan adapters. The domain crate has no dependency on these modules.
//!
//! One block per target namespace, bottom layer first (docs/architecture.md#module-map).
//! A block lists today's top-level modules that move into that namespace.

pub type Error = Box<dyn std::error::Error + Send + Sync>;

// Utility layer.

// text
mod text;

// ids
mod ids;

// time
pub mod clock;
pub mod local_day;
mod zone;

// protocol
pub mod protocol;

// regions
pub mod breaks;
pub mod regions;

// hosts
mod tool_names;
mod vendor;

// Infrastructure layer.

// fs
mod file_state;
mod input;
mod local;

// git
pub mod git;
pub mod observer;
pub mod remote;
mod sha1;

// process
pub mod process;

// ipc
pub mod autostart;
pub mod client;

// store
pub mod store;

// graph
pub mod graph;
pub mod index_file;

// shell
pub mod shell;

// install
pub mod binary_version;
pub mod guidance;
pub mod hosts;
pub mod install;
pub mod service;
pub mod user_install;

// Application layer.

// ownership
pub mod adopt;
pub mod commit;
pub mod formatter;
pub mod journal;
mod lines;
mod reformat;

// checks
pub mod auto_check;
pub mod definitions;
pub mod integrate;
pub mod landing;
pub mod rewrite;
pub mod test_output;

// briefing
pub mod recent;
pub mod since;
pub mod trail;

// person
pub mod count;
pub mod dashboard;
pub mod notify;
pub mod report;
pub mod status_line;
pub mod today;

// hook
pub mod antigravity;
pub mod between_steps;
pub mod claude_code;
pub mod codex;
pub mod cursor;
pub mod gemini_removed;
pub mod hook;
pub mod spool;
mod wait;

// Interface layer.

// daemon
pub mod daemon;

// mcp
pub mod mcp;

// cli
pub mod cli;
pub mod demo;
pub mod skill;

// Compatibility aliases: old paths kept until importers are repointed.
use ids::registration;
use ids::sha256;
use text::redact;
