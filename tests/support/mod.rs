//! Shared by the integration tests: a remote which asks for credentials
//! (loopback only), process checks, and a home of the test's own.

use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
};

/// A loopback HTTP server that asks for Basic authentication on every
/// request, so Git consults its credential helpers. It records whether a
/// request carried credentials, never their value.
#[allow(dead_code)]
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

/// Points `command` at `home` as its user home, with every host location
/// LangSpan reads inside it (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`,
/// `GEMINI_CLI_HOME`) and no state-directory default from the environment.
/// Cursor's location follows `HOME`. A test that runs `doctor` or a
/// user-level command through this sees only what it put in `home`, never
/// the machine's own install (issue #108). `home` need not exist.
#[allow(dead_code)] // Not every test that includes this module runs `doctor`.
pub fn in_home<'a>(
    command: &'a mut std::process::Command,
    home: &std::path::Path,
) -> &'a mut std::process::Command {
    command
        .env("HOME", home)
        .env("CLAUDE_CONFIG_DIR", home.join(".claude"))
        .env("CODEX_HOME", home.join(".codex"))
        .env("GEMINI_CLI_HOME", home)
        .env_remove("LANGSPAN_STATE_DIR")
        .env_remove("XDG_STATE_HOME")
        .env_remove("CLAUDE_PROJECT_DIR")
}

#[allow(dead_code)] // Not every test that includes this module checks a process.
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
