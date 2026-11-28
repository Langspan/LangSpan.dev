//! The coordinator socket's frame reader and request decoder, as
//! `daemon::serve_connection` and the client call them on a peer's bytes.
#![no_main]

use langspan::protocol::{Request, Response, read_frame};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(frame) = read_frame(data) else {
        return;
    };
    if let Ok(mut request) = serde_json::from_slice::<Request>(&frame) {
        langspan::text::redact::redact_command(&mut request.command);
    }
    let _ = serde_json::from_slice::<Response>(&frame);
});
