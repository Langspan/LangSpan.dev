//! Version 3 of the local IPC contract. One bounded JSON line per connection.

use langspan_core::{Command, Outcome};
use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, BufReader, Read};

pub const PROTOCOL_VERSION: u16 = 3;
pub const MAX_FRAME_BYTES: usize = 65_536;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u16,
    pub request_id: String,
    pub command: Command,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub version: u16,
    pub request_id: String,
    pub replayed: bool,
    pub generation: u64,
    pub lease_valid: Option<bool>,
    pub outcome: Outcome,
}

impl Response {
    /// The same response carrying another outcome: an answer cut to fit.
    pub fn with_outcome(&self, outcome: Outcome) -> Self {
        Self {
            version: self.version,
            request_id: self.request_id.clone(),
            replayed: self.replayed,
            generation: self.generation,
            lease_valid: self.lease_valid,
            outcome,
        }
    }
}

pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, crate::Error> {
    let mut bytes = serde_json::to_vec(value)?;
    if bytes.len() >= MAX_FRAME_BYTES {
        return Err("JSON message exceeds the 64 KiB frame limit; narrow the request".into());
    }
    bytes.push(b'\n');
    Ok(bytes)
}

pub fn read_frame(reader: impl Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    BufReader::new(reader.take((MAX_FRAME_BYTES + 1) as u64)).read_until(b'\n', &mut bytes)?;
    if bytes.len() > MAX_FRAME_BYTES || bytes.last() != Some(&b'\n') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "expected one newline-terminated JSON frame of at most 64 KiB",
        ));
    }
    Ok(bytes)
}
