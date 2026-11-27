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

/// A child process a test started, such as a coordinator, killed and reaped
/// when this is dropped: on success, a failed assertion or any other panic
/// (issue #239). `std::process::Child` alone does neither, so a test that
/// keeps a bare `Child` past a statement that can panic leaves the process
/// running.
#[allow(dead_code)] // Not every test that includes this module starts a process.
pub struct Owned(Option<std::process::Child>);
#[allow(dead_code)]
impl Owned {
    /// Starts `command` and owns the process from the moment it exists.
    pub fn spawn(command: &mut std::process::Command) -> Self {
        Self(Some(command.spawn().expect("start the child process")))
    }
    pub fn child(&mut self) -> &mut std::process::Child {
        self.0
            .as_mut()
            .expect("the child process was already stopped")
    }
    pub fn id(&self) -> u32 {
        self.0.as_ref().map_or(0, std::process::Child::id)
    }
    /// Kills and reaps the process now. Stopping one that is already gone
    /// does nothing.
    pub fn stop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    /// Gives up ownership, for a test that waits for the process itself.
    pub fn wait_with_output(mut self) -> std::process::Output {
        self.0
            .take()
            .expect("the child process was already stopped")
            .wait_with_output()
            .expect("wait for the child process")
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        self.stop();
    }
}

#[allow(dead_code)] // Not every test that includes this module checks a process.
pub fn alive(pid: i32) -> bool {
    rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap()).is_ok()
}

/// Whether `pid` is still running: gone, or a zombie no parent has reaped
/// yet, is not. A process orphaned to an init that does not reap (some
/// Linux containers) stays a zombie, and [`alive`] still finds it. Use this
/// to wait for a process to end.
#[allow(dead_code)] // Not every test that includes this module waits for a process.
pub fn running(pid: i32) -> bool {
    if !alive(pid) {
        return false;
    }
    let state = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .expect("run ps");
    let state = String::from_utf8_lossy(&state.stdout);
    let state = state.trim();
    !state.is_empty() && !state.starts_with('Z')
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

/// Waits for `ready` to return `Ok`, looking every 20 ms, and returns its
/// value. After `limit` it fails, naming `what` it waited for and the last
/// `Err` it saw. Use it, not a fixed sleep, before an assertion (issue
/// #310): it ends once the condition holds, and on a loaded machine it waits
/// up to the whole limit instead of a guess.
#[allow(dead_code)] // Not every test that includes this module waits.
#[track_caller]
pub fn wait_for<T>(
    what: &str,
    limit: std::time::Duration,
    mut ready: impl FnMut() -> Result<T, String>,
) -> T {
    let deadline = std::time::Instant::now() + limit;
    loop {
        match ready() {
            Ok(value) => return value,
            Err(seen) => assert!(
                std::time::Instant::now() < deadline,
                "waited {limit:?} for {what}; last saw: {seen}"
            ),
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// As [`wait_for`], for a condition with nothing to report.
#[allow(dead_code)]
#[track_caller]
pub fn wait_until(what: &str, limit: std::time::Duration, mut done: impl FnMut() -> bool) {
    wait_for(what, limit, || {
        if done() {
            Ok(())
        } else {
            Err("not yet".into())
        }
    });
}

/// Checks that `holds` stays true for the whole `period`, looking every
/// 20 ms, and fails the first time it doesn't, naming `what`. This is for a
/// test whose subject is that something does not happen within a stated
/// time, such as no second run or no fetch during a back-off: the period is
/// part of the assertion. Wait for a positive sign first where there is one.
#[allow(dead_code)]
#[track_caller]
pub fn holds_for(what: &str, period: std::time::Duration, mut holds: impl FnMut() -> bool) {
    let started = std::time::Instant::now();
    loop {
        assert!(
            holds(),
            "{what}: no longer true {:?} into the {period:?} it must hold for",
            started.elapsed()
        );
        if started.elapsed() >= period {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Waits until `sample` has returned the same value for `quiet`, and returns
/// that value: for a count a burst of activity raises, read once the burst
/// is over. Fails after `limit` if it never stays still that long.
#[allow(dead_code)]
#[track_caller]
pub fn settles<T: PartialEq + std::fmt::Debug>(
    what: &str,
    quiet: std::time::Duration,
    limit: std::time::Duration,
    mut sample: impl FnMut() -> T,
) -> T {
    let deadline = std::time::Instant::now() + limit;
    let mut value = sample();
    let mut since = std::time::Instant::now();
    loop {
        std::thread::sleep(std::time::Duration::from_millis(20));
        let now = sample();
        if now != value {
            assert!(
                std::time::Instant::now() < deadline,
                "{what} was still changing after {limit:?}: {now:?}"
            );
            value = now;
            since = std::time::Instant::now();
        } else if since.elapsed() >= quiet {
            return value;
        }
    }
}

/// Waits up to 10 s for the coordinator at `state_dir` to answer a status
/// request. A socket file alone isn't enough: a killed coordinator leaves
/// one behind.
#[allow(dead_code)]
#[track_caller]
pub fn daemon_ready(state_dir: &std::path::Path) {
    let status = langspan::protocol::Request::new("read", langspan_core::Command::Status);
    wait_for(
        &format!("the coordinator at {} to answer", state_dir.display()),
        std::time::Duration::from_secs(10),
        || {
            langspan::client::call(state_dir, &status)
                .map(drop)
                .map_err(|error| error.to_string())
        },
    );
}

/// Waits up to 30 s for the coordinator at `state_dir` to record an event
/// that `matches`, and returns it.
#[allow(dead_code)]
#[track_caller]
pub fn event_recorded(
    state_dir: &std::path::Path,
    what: &str,
    mut matches: impl FnMut(&langspan_core::Event) -> bool,
) -> langspan_core::RecordedEvent {
    let mut after = 0;
    wait_for(
        &format!("the event {what}"),
        std::time::Duration::from_secs(30),
        || {
            let page = langspan::protocol::Request::new(
                "events",
                langspan_core::Command::Events { after, limit: 1000 },
            );
            let events = match langspan::client::call(state_dir, &page)
                .map_err(|error| error.to_string())?
                .outcome
            {
                langspan_core::Outcome::Events { events } => events,
                other => return Err(format!("{other:?}")),
            };
            after = events.last().map_or(after, |event| event.seq);
            events
                .into_iter()
                .find(|recorded| matches(&recorded.event))
                .ok_or_else(|| format!("no such event up to sequence {after}"))
        },
    )
}

/// Waits up to 10 s for process `pid` to end (see [`running`]).
#[allow(dead_code)]
#[track_caller]
pub fn process_gone(pid: i32) {
    wait_until(
        &format!("process {pid} to end"),
        std::time::Duration::from_secs(10),
        || !running(pid),
    );
}

/// Waits until the wall clock reaches the next whole second: for a test of
/// a window given in RFC 3339 to the second, so what happened before the
/// call falls in an earlier second than what happens after it.
#[allow(dead_code)]
pub fn next_wall_second() {
    let second = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    };
    let start = second();
    wait_until(
        "the wall clock's next second",
        std::time::Duration::from_secs(5),
        || second() > start,
    );
}
