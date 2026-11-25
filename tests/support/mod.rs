//! Shared by the integration tests that need a remote which asks for
//! credentials. Loopback only.

use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
};

/// A loopback HTTP server that asks for Basic authentication on every
/// request, so Git consults its credential helpers. It records whether a
/// request carried credentials, never their value.
pub fn asks_for_credentials() -> (String, std::sync::mpsc::Receiver<bool>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (sent, seen) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut authorised = false;
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 0) && line != "\r\n" {
                authorised |= line.to_ascii_lowercase().starts_with("authorization:");
                line.clear();
            }
            let _ = sent.send(authorised);
            let mut stream = stream;
            let _ = stream.write_all(
                b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"t\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    });
    (format!("http://{address}/repo.git"), seen)
}

pub fn alive(pid: i32) -> bool {
    rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap()).is_ok()
}

/// Runs a command built fresh by `make`, retrying while Linux says the
/// program is busy (ETXTBSY). A test that copies a binary and runs it can hit
/// this: a process another test thread forks while the copy is still open for
/// writing holds that descriptor until it execs, which takes moments.
#[allow(dead_code)] // Not every test that includes this module runs a copied binary.
pub fn output_once_not_busy(
    mut make: impl FnMut() -> std::process::Command,
) -> std::process::Output {
    for _ in 0..50 {
        match make().output() {
            Err(error) if error.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            result => return result.unwrap(),
        }
    }
    make().output().unwrap()
}
