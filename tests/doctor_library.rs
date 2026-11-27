//! `doctor` called as a library function, without spawning the binary
//! (issue #355).

use langspan::{cli::doctor::doctor, hosts::UserHome};

#[test]
fn doctor_runs_as_a_function_and_needs_attention_without_a_coordinator() {
    let base = std::env::temp_dir().join(format!("ls-doctor-library-{}", std::process::id()));
    let home = base.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let home = UserHome::new(Some(home)).unwrap();
    // Nothing runs for this state directory, and the home has no host: the
    // unreachable coordinator is the one finding that needs attention.
    let code = doctor(&base.join("state"), None, &home, true).unwrap();
    std::fs::remove_dir_all(&base).unwrap();
    assert_eq!(code, 1);
}
