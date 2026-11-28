//! Test-name classification of a check command's output (cargo test, go
//! test, pytest), as the coordinator reads it after a run.
#![no_main]

use langspan::test_output::read;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // The first byte chooses the run's exit status and whether one name is
    // preferred, so both branches of `results` are reached.
    let Some((&flags, output)) = data.split_first() else {
        return;
    };
    let prefer = if flags & 2 == 0 {
        Vec::new()
    } else {
        vec!["tests::a".to_owned()]
    };
    let _ = read(output, false, flags & 1 == 1, &prefer);
});
