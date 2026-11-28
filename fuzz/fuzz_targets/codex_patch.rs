//! The Codex host's `apply_patch` parser, which reads the patch text an
//! agent sends before admission.
#![no_main]

use langspan::codex::{patch_edits, patch_paths};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(patch) = std::str::from_utf8(data) else {
        return;
    };
    let _ = patch_paths(patch);
    let _ = patch_edits(patch);
});
