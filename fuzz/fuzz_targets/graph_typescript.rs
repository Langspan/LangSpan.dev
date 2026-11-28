//! `langspan_graph::extract` over TypeScript source: the tree-sitter
//! grammar's C code, run on repository text inside the coordinator.
#![no_main]

use langspan_graph::{Language, extract};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = extract(Language::TypeScript, data);
});
