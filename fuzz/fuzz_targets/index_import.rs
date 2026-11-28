//! An exported source index, read line by line the way `langspan index
//! import` reads a file someone else wrote: the header, then file records.
#![no_main]

use langspan::index_file::{Line, MAX_LINE_BYTES, Seen, parse_header, parse_record, read_line};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut input = data;
    let mut line = Vec::new();
    let mut seen = Seen::default();
    let mut first = true;
    // Records are parsed whatever the header says, so a schema change in
    // the main crate doesn't stop this target reaching them.
    while let Ok(read) = read_line(&mut input, &mut line, MAX_LINE_BYTES) {
        match read {
            Line::End => break,
            Line::TooLong => {}
            Line::Read if first => {
                let _ = parse_header(&line, 4);
            }
            Line::Read => {
                if let Ok(record) = parse_record(&line) {
                    seen.first(&record.path);
                }
            }
        }
        first = false;
    }
});
