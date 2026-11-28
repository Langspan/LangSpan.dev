//! The Win32 calls LangSpan needs on Windows, behind a safe API.
//!
//! LangSpan's own crates forbid `unsafe` code. On Unix, `rustix` provides the
//! system calls they need as safe functions. Nothing comparable covers
//! LangSpan's needs on Windows, so this crate does that job for Windows:
//!
//! - [`interrupt_time_ms`]: the sleep-inclusive monotonic clock behind
//!   `langspan::clock`.
//! - [`create_private_directory`], [`open_private_file`],
//!   [`open_no_follow`] and [`is_owner_only`]: owner-only files and
//!   directories, the Windows counterpart of mode 0700 and 0600.
//!
//! Each `unsafe` block is one documented Win32 call with a `SAFETY` comment,
//! and no raw pointer or handle leaves the crate. On other platforms the crate
//! is empty, and `langspan` depends on it only on Windows.

#[cfg(windows)]
mod win32;

#[cfg(windows)]
pub use win32::{
    create_private_directory, interrupt_time_ms, is_owner_only, open_no_follow, open_private_file,
};
