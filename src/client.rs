//! Synchronous local IPC used by the CLI and MCP adapter.

#[cfg(unix)]
mod unix {
    use crate::protocol::{Capabilities, PROTOCOL_VERSION, Request, Response, encode, read_frame};
    use langspan_core::Outcome;
    use rustix::{
        event::{PollFd, PollFlags, Timespec, poll},
        io::Errno,
        net::{self, AddressFamily, SocketAddrUnix, SocketType},
    };
    use std::{
        io::{self, Read, Write},
        os::unix::net::UnixStream,
        path::Path,
        time::{Duration, Instant},
    };

    const CALL_TIMEOUT: Duration = Duration::from_secs(5);

    pub fn call(state_dir: &Path, request: &Request) -> Result<Response, crate::Error> {
        call_until(state_dir, request, Instant::now() + CALL_TIMEOUT)
    }

    /// Like `call`, bounded by the caller's total deadline instead of the default.
    pub fn call_until(
        state_dir: &Path,
        request: &Request,
        deadline: Instant,
    ) -> Result<Response, crate::Error> {
        remaining(deadline)?;
        // Owner and agent text is redacted here, before it leaves the client.
        let mut request = request.clone();
        crate::hook::redact_command(&mut request.command);
        let (mut response, granted) = exchange(state_dir, &request, deadline)?;
        // A daemon refuses a request it cannot read before it reads the
        // command (ADR 0037), so the same request asked again cannot take
        // effect twice. A daemon of an earlier build refuses this version,
        // or the `capabilities` it does not know, in its own version: it is
        // asked again in that version, naming none. One of this version
        // that cannot read the request names the capabilities it granted:
        // it is asked again with those. Either way the command is reshaped
        // to what that daemon reads (`protocol::for_capabilities`), and one
        // that would lose its meaning is not sent at all (`protocol::fits_with`).
        let refused = matches!(&response.outcome, Outcome::Error { code, .. }
            if code == "unsupported_version" || code == "invalid_request");
        let again = if !refused || request.version != PROTOCOL_VERSION {
            None
        } else if response.version < PROTOCOL_VERSION && crate::protocol::answers(response.version)
        {
            Some((response.version, None))
        } else if response.version == PROTOCOL_VERSION && granted.is_some() {
            Some((response.version, granted))
        } else {
            None
        };
        if let Some((version, named)) = again {
            let capabilities = Capabilities::negotiate(version, named.as_deref());
            if !crate::protocol::fits_with(capabilities, &request.command) {
                return Err(crate::protocol::older_daemon(version).into());
            }
            request.version = version;
            request.capabilities = named;
            crate::protocol::for_capabilities(capabilities, &mut request.command);
            response = exchange(state_dir, &request, deadline)?.0;
        }
        if response.version != request.version {
            return Err(
                "daemon response does not match the request identity or protocol version".into(),
            );
        }
        Ok(response)
    }

    /// One request and its answer, in the request's version, with the
    /// capabilities the answer names when the daemon granted fewer than the
    /// request named.
    fn exchange(
        state_dir: &Path,
        request: &Request,
        deadline: Instant,
    ) -> Result<(Response, Option<Vec<String>>), crate::Error> {
        let bytes = encode(request)?;
        crate::local::check_endpoint(state_dir).map_err(|error| {
            format!("coordination unavailable: {error}; start `langspan daemon --state-dir <dir>`")
        })?;
        let socket = state_dir.join("daemon.sock");
        let stream = connect(&socket, deadline).map_err(|error| {
            format!(
                "coordination unavailable at {}: {error}; start `langspan daemon --state-dir <dir>`",
                socket.display()
            )
        })?;
        // A coordinator that stops during the call resets or closes the
        // connection. That is the same unavailability as a refused connect,
        // except the request may already be recorded, so the retry must reuse
        // its request ID.
        let frame = DeadlineStream::new(stream, deadline)
            .and_then(|mut stream| {
                stream.write_all(&bytes)?;
                read_frame(stream)
            })
            .map_err(|error| {
                format!(
                    "coordination unavailable at {}: {error}; once `langspan daemon --state-dir <dir>` is running, retry with the same request_id",
                    socket.display()
                )
            })?;
        let mut value: serde_json::Value = serde_json::from_slice(&frame)?;
        // How many events a page left out (`withheld_events`): only a later
        // daemon, with event types this build has no variant for, sends it.
        // The page holds what this build reads.
        value.as_object_mut().map(|f| f.shift_remove("withheld"));
        let granted = match value
            .as_object_mut()
            .and_then(|f| f.shift_remove("capabilities"))
        {
            Some(names) => Some(serde_json::from_value::<Vec<String>>(names)?),
            None => None,
        };
        let response: Response = serde_json::from_value(value)?;
        // A daemon answers a request it could not read (or a frame it did
        // not take) without the request ID, as an error: it never reached
        // the command, so nothing took effect and the error is the answer.
        let unread =
            response.request_id.is_empty() && matches!(response.outcome, Outcome::Error { .. });
        if response.request_id != request.request_id && !unread {
            return Err(
                "daemon response does not match the request identity or protocol version".into(),
            );
        }
        remaining(deadline)?;
        Ok((response, granted))
    }

    fn timed_out() -> io::Error {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "coordination I/O deadline elapsed; retry with the same request_id",
        )
    }

    fn remaining(deadline: Instant) -> io::Result<Duration> {
        deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(timed_out)
    }

    fn wait_ready(stream: &UnixStream, flags: PollFlags, deadline: Instant) -> io::Result<()> {
        loop {
            let timeout = Timespec::try_from(remaining(deadline)?).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid coordination I/O deadline",
                )
            })?;
            let mut descriptors = [PollFd::new(stream, flags)];
            match poll(&mut descriptors, Some(&timeout)) {
                Ok(0) | Err(Errno::INTR) => continue,
                Ok(_) if descriptors[0].revents().contains(PollFlags::NVAL) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "invalid daemon socket",
                    ));
                }
                Ok(_) => return Ok(()),
                Err(error) => return Err(error.into()),
            }
        }
    }

    /// Whether the daemon accepts a connection before `deadline`: the same
    /// owner-only socket checks and nonblocking connect as a call, so a full
    /// listen backlog or a stalled daemon answers "no" in time.
    pub fn accepts(state_dir: &Path, deadline: Instant) -> bool {
        crate::local::check_endpoint(state_dir).is_ok()
            && connect(&state_dir.join("daemon.sock"), deadline).is_ok()
    }

    fn connect(path: &Path, deadline: Instant) -> io::Result<UnixStream> {
        remaining(deadline)?;
        let address = SocketAddrUnix::new(path)?;
        // Create both flags atomically where available. Other Unix platforms
        // set CLOEXEC before the descriptor can be used by this adapter.
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let descriptor = net::socket_with(
            AddressFamily::UNIX,
            SocketType::STREAM,
            net::SocketFlags::CLOEXEC | net::SocketFlags::NONBLOCK,
            None,
        )?;
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        let descriptor = {
            let descriptor = net::socket(AddressFamily::UNIX, SocketType::STREAM, None)?;
            rustix::io::fcntl_setfd(&descriptor, rustix::io::FdFlags::CLOEXEC)?;
            descriptor
        };
        let stream = UnixStream::from(descriptor);
        stream.set_nonblocking(true)?;
        match net::connect(&stream, &address) {
            Ok(()) => {}
            Err(Errno::INPROGRESS | Errno::ALREADY | Errno::INTR) => {
                wait_ready(&stream, PollFlags::OUT, deadline)?;
                net::sockopt::socket_error(&stream)??;
                // Readiness alone is not proof that the connection completed.
                stream.peer_addr()?;
            }
            // In particular, Unix EAGAIN can mean a full listen backlog.
            // Fail promptly; never mistake an unconnected writable fd for success.
            Err(error) => return Err(error.into()),
        }
        remaining(deadline)?;
        Ok(stream)
    }

    struct DeadlineStream {
        stream: UnixStream,
        deadline: Instant,
    }

    impl DeadlineStream {
        fn new(stream: UnixStream, deadline: Instant) -> io::Result<Self> {
            remaining(deadline)?;
            stream.set_nonblocking(true)?;
            Ok(Self { stream, deadline })
        }

        fn perform<T>(
            &mut self,
            flags: PollFlags,
            mut operation: impl FnMut(&mut UnixStream) -> io::Result<T>,
        ) -> io::Result<T> {
            loop {
                remaining(self.deadline)?;
                match operation(&mut self.stream) {
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        wait_ready(&self.stream, flags, self.deadline)?;
                    }
                    result => return result,
                }
            }
        }
    }

    impl Read for DeadlineStream {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.perform(PollFlags::IN, |stream| stream.read(buffer))
        }
    }

    impl Write for DeadlineStream {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.perform(PollFlags::OUT, |stream| stream.write(buffer))
        }

        fn flush(&mut self) -> io::Result<()> {
            self.perform(PollFlags::OUT, Write::flush)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn read_progress_does_not_restart_the_total_deadline() {
            let (client, mut peer) = UnixStream::pair().unwrap();
            peer.write_all(b"ab").unwrap();
            let deadline = Instant::now() + Duration::from_millis(100);
            let mut stream = DeadlineStream::new(client, deadline).unwrap();
            let mut byte = [0];
            assert_eq!(stream.read(&mut byte).unwrap(), 1);
            assert_eq!(byte, [b'a']);
            std::thread::sleep(
                deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(1),
            );
            let error = stream
                .read(&mut byte)
                .expect_err("buffered progress must not extend the deadline");
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        }

        #[test]
        fn write_progress_does_not_restart_the_total_deadline() {
            let (client, _peer) = UnixStream::pair().unwrap();
            let deadline = Instant::now() + Duration::from_millis(100);
            let mut stream = DeadlineStream::new(client, deadline).unwrap();
            assert_eq!(stream.write(b"a").unwrap(), 1);
            std::thread::sleep(
                deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(1),
            );
            let error = stream
                .write(b"b")
                .expect_err("write progress must not extend the deadline");
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        }

        #[test]
        fn an_idle_peer_times_out_without_resetting_the_poll_budget() {
            let (client, _peer) = UnixStream::pair().unwrap();
            let started = Instant::now();
            let budget = Duration::from_millis(20);
            let mut stream = DeadlineStream::new(client, started + budget).unwrap();
            let error = stream.read(&mut [0]).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
            // Load can lengthen this wait, so there is no scheduling-sensitive
            // upper latency assertion.
            assert!(started.elapsed() >= budget);
        }

        #[test]
        fn a_coordinator_that_drops_the_call_is_reported_as_unavailable() {
            use std::os::unix::fs::PermissionsExt;
            let dir = std::env::temp_dir().join(format!("ls-lost-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir(&dir).unwrap();
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
            let listener = std::os::unix::net::UnixListener::bind(dir.join("daemon.sock")).unwrap();
            std::fs::set_permissions(
                dir.join("daemon.sock"),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
            // A coordinator that goes away after the connect: it accepts,
            // then closes without answering.
            let peer = std::thread::spawn(move || drop(listener.accept().unwrap()));
            let request = crate::protocol::Request::new("read", langspan_core::Command::Dashboard);
            let error = call(&dir, &request).unwrap_err().to_string();
            peer.join().unwrap();
            std::fs::remove_dir_all(&dir).unwrap();
            assert!(error.starts_with("coordination unavailable at "), "{error}");
            assert!(error.contains("retry with the same request_id"), "{error}");
        }

        /// Roadmap D7 (review comment 4707749784), H9 (review comment
        /// 4711096877) and issue #222 (review comments 4711375982 and
        /// 4712860167), roadmap A11 (review comment 4714078108): a daemon
        /// of an earlier build answers only protocol version 3, or only up
        /// to 4, 5, 6 (roadmap F5's build), 7 (issue #222's), 8 (issue
        /// #262's) or 9 (roadmap E1's). It refuses this build's version in
        /// its own, as that build does, and the client asks again once in
        /// that version, with the same request, and reads that answer.
        #[test]
        fn a_daemon_of_an_earlier_build_is_asked_again_in_its_version() {
            for version in [
                crate::protocol::OLDEST_PROTOCOL_VERSION,
                crate::protocol::PROTOCOL_VERSION_4,
                crate::protocol::PROTOCOL_VERSION_5,
                crate::protocol::PROTOCOL_VERSION_6,
                crate::protocol::PROTOCOL_VERSION_7,
                crate::protocol::PROTOCOL_VERSION_8,
                crate::protocol::PROTOCOL_VERSION_9,
            ] {
                ask_an_earlier_daemon(version);
            }
        }

        /// A daemon of this build's version (roadmap A11's 10) answers a
        /// briefing of changes only as it is asked, once, with
        /// `changes_only` kept. A daemon of version 9 is asked again in its
        /// version, still for changes only
        /// (`a_daemon_of_an_earlier_build_is_asked_again_in_its_version`).
        #[test]
        fn a_daemon_of_this_version_is_asked_once_for_changes_only() {
            use std::io::{BufRead, BufReader};
            use std::os::unix::fs::PermissionsExt;
            let version = crate::protocol::PROTOCOL_VERSION;
            let dir = std::env::temp_dir().join(format!("ls-current-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir(&dir).unwrap();
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
            let listener = std::os::unix::net::UnixListener::bind(dir.join("daemon.sock")).unwrap();
            std::fs::set_permissions(
                dir.join("daemon.sock"),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
            let peer = std::thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(&stream).read_line(&mut line).unwrap();
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                let reply = serde_json::json!({"version": version, "request_id": request["request_id"],
                    "replayed": false, "generation": 1, "lease_valid": null,
                    "outcome": {"type": "error", "code": "answered", "message": "in its version"}});
                writeln!(&stream, "{reply}").unwrap();
                // A second request would be accepted here; none may come.
                listener.set_nonblocking(true).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(200));
                (request, listener.accept().is_ok())
            });
            let request = crate::protocol::Request::new(
                "read",
                langspan_core::Command::Brief {
                    session_id: "s".into(),
                    max_peers: 8,
                    deliver: true,
                    mid_turn: false,
                    left: Vec::new(),
                    recent: false,
                    recent_window_ms: None,
                    mcp: false,
                    changes_only: true,
                },
            )
            .with_version(version);
            let response = call(&dir, &request).unwrap();
            let (asked, asked_again) = peer.join().unwrap();
            std::fs::remove_dir_all(&dir).unwrap();
            assert!(!asked_again, "asked again of a daemon of this version");
            assert_eq!(asked["version"], version);
            assert_eq!(asked["command"]["changes_only"], true);
            assert_eq!(response.version, version);
            assert!(
                matches!(&response.outcome, Outcome::Error { code, .. } if code == "answered"),
                "{response:?}"
            );
        }

        /// Review comment 4712442596: a request to record a rebased landing
        /// is not sent again to a daemon of version 5 or earlier, which
        /// could not read it; the error says to restart the daemon.
        #[test]
        fn a_rebased_landing_is_not_sent_again_to_an_earlier_daemon() {
            use std::io::{BufRead, BufReader};
            use std::os::unix::fs::PermissionsExt;
            let dir = std::env::temp_dir().join(format!("ls-unfit-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir(&dir).unwrap();
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
            let listener = std::os::unix::net::UnixListener::bind(dir.join("daemon.sock")).unwrap();
            std::fs::set_permissions(
                dir.join("daemon.sock"),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
            let peer = std::thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(&stream).read_line(&mut line).unwrap();
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                let reply = serde_json::json!({"version": 5, "request_id": request["request_id"],
                    "replayed": false, "generation": 1, "lease_valid": null,
                    "outcome": {"type": "error", "code": "unsupported_version", "message": "older"}});
                writeln!(&stream, "{reply}").unwrap();
                // A second request would be accepted here; none may come.
                listener.set_nonblocking(true).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(200));
                listener.accept().is_ok()
            });
            let request = crate::protocol::Request::new(
                "land",
                langspan_core::Command::RecordLanding {
                    workspace_id: "app".into(),
                    onto: "main".into(),
                    commit_oid: "c".repeat(40),
                    tree_oid: "d".repeat(40),
                    inputs: vec!["e".repeat(40)],
                    check_id: "check".into(),
                    rebased: vec![langspan_core::Rewritten {
                        from: "e".repeat(40),
                        to: "c".repeat(40),
                    }],
                    adapted: Vec::new(),
                },
            );
            let error = call(&dir, &request).unwrap_err().to_string();
            let asked_again = peer.join().unwrap();
            std::fs::remove_dir_all(&dir).unwrap();
            assert!(!asked_again, "sent again in version 5");
            assert!(
                error.contains("protocol version 5") && error.contains("restart the daemon"),
                "{error}"
            );
        }

        /// ADR 0037: a daemon of this version that cannot read a request
        /// names the capabilities it granted, and is asked again once with
        /// those, the command reshaped to them: here a check recorded
        /// without the findings of a capability it lacks (issue #265).
        #[test]
        fn a_daemon_granting_fewer_capabilities_is_asked_again_with_those() {
            use std::io::{BufRead, BufReader};
            use std::os::unix::fs::PermissionsExt;
            let version = PROTOCOL_VERSION;
            let dir = std::env::temp_dir().join(format!("ls-granted-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir(&dir).unwrap();
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
            let listener = std::os::unix::net::UnixListener::bind(dir.join("daemon.sock")).unwrap();
            std::fs::set_permissions(
                dir.join("daemon.sock"),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
            let granted: Vec<String> = Capabilities::ALL
                .names()
                .into_iter()
                .filter(|name| name != "symbol_findings")
                .collect();
            let known = granted.clone();
            let peer = std::thread::spawn(move || {
                let mut asked = Vec::new();
                for _ in 0..2 {
                    let (stream, _) = listener.accept().unwrap();
                    let mut line = String::new();
                    BufReader::new(&stream).read_line(&mut line).unwrap();
                    let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                    let mut reply = serde_json::json!({"version": version, "request_id": request["request_id"],
                        "replayed": false, "generation": 1, "lease_valid": null,
                        "outcome": {"type": "error", "code": "answered", "message": "read"}});
                    if request["command"].get("symbol_findings").is_some() {
                        // As the daemon answers a request it cannot read:
                        // without its request ID (review comment 4715859499).
                        reply["request_id"] = serde_json::json!("");
                        reply["generation"] = serde_json::json!(0);
                        reply["outcome"] = serde_json::json!({"type": "error", "code": "invalid_request",
                            "message": "expected a typed version 1 request; unknown fields are rejected"});
                        reply["capabilities"] = serde_json::json!(known);
                    }
                    writeln!(&stream, "{reply}").unwrap();
                    asked.push(request);
                }
                asked
            });
            let mut wire: serde_json::Value =
                serde_json::from_slice(include_bytes!("../fixtures/record-check-command-v10.json"))
                    .unwrap();
            wire["command"]["symbol_findings"] = serde_json::json!({"findings": [], "more": 0,
                "files_indexed": 1, "files_skipped": 0});
            let command = serde_json::from_value(wire["command"].clone()).unwrap();
            let request = Request::new("record", command);
            let response = call(&dir, &request).unwrap();
            let asked = peer.join().unwrap();
            std::fs::remove_dir_all(&dir).unwrap();
            assert_eq!(
                asked[0]["capabilities"],
                serde_json::json!(Capabilities::ALL.names())
            );
            assert_eq!(asked[1]["capabilities"], serde_json::json!(granted));
            assert_eq!(asked[1]["version"], version);
            let mut without = asked[0]["command"].clone();
            without
                .as_object_mut()
                .unwrap()
                .shift_remove("symbol_findings");
            assert_eq!(asked[1]["command"], without);
            assert!(
                matches!(&response.outcome, Outcome::Error { code, .. } if code == "answered"),
                "{response:?}"
            );
        }

        /// Answers one connection with `reply`, then reports whether a
        /// second came within 200 ms.
        fn answer_once(name: &str, reply: serde_json::Value) -> (Result<Response, String>, bool) {
            use std::io::{BufRead, BufReader};
            use std::os::unix::fs::PermissionsExt;
            let dir = std::env::temp_dir().join(format!("ls-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir(&dir).unwrap();
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
            let listener = std::os::unix::net::UnixListener::bind(dir.join("daemon.sock")).unwrap();
            std::fs::set_permissions(
                dir.join("daemon.sock"),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
            let peer = std::thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(&stream).read_line(&mut line).unwrap();
                writeln!(&stream, "{reply}").unwrap();
                listener.set_nonblocking(true).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(200));
                listener.accept().is_ok()
            });
            let request = Request::new("read", langspan_core::Command::Dashboard);
            let result = call(&dir, &request).map_err(|error| error.to_string());
            let asked_again = peer.join().unwrap();
            std::fs::remove_dir_all(&dir).unwrap();
            (result, asked_again)
        }

        /// Review comment 4715859499: a daemon answers a request it cannot
        /// read, a frame it cannot take, or one that came too late, without
        /// the request ID, as `src/daemon.rs` does. Such an error is the
        /// answer, read before the identity check, and only a refusal of
        /// the version or the request asks again.
        #[test]
        fn an_error_without_the_request_id_is_the_answer() {
            for code in ["invalid_frame", "read_timeout"] {
                let reply = serde_json::json!({"version": PROTOCOL_VERSION, "request_id": "",
                    "replayed": false, "generation": 0, "lease_valid": null,
                    "outcome": {"type": "error", "code": code, "message": "not read"}});
                let (result, asked_again) = answer_once(code, reply);
                assert!(!asked_again, "{code} asked again");
                let response = result.unwrap();
                assert!(
                    matches!(&response.outcome, Outcome::Error { code: c, .. } if c == code),
                    "{response:?}"
                );
            }
        }

        /// Review comment 4715859499: only an error may come without the
        /// request ID; any other answer, or one naming another request,
        /// does not match the request.
        #[test]
        fn an_answer_for_another_request_is_refused() {
            for (name, id, outcome) in [
                (
                    "idless",
                    "",
                    serde_json::json!({"type": "version", "version": "x", "pid": 1}),
                ),
                (
                    "other",
                    "other",
                    serde_json::json!({"type": "error", "code": "invalid_request", "message": "m"}),
                ),
            ] {
                let reply = serde_json::json!({"version": PROTOCOL_VERSION, "request_id": id,
                    "replayed": false, "generation": 0, "lease_valid": null, "outcome": outcome});
                let (result, asked_again) = answer_once(name, reply);
                assert!(!asked_again, "{name} asked again");
                let error = result.unwrap_err();
                assert!(
                    error.contains("does not match the request identity"),
                    "{error}"
                );
            }
        }

        fn ask_an_earlier_daemon(version: u16) {
            use std::io::{BufRead, BufReader};
            use std::os::unix::fs::PermissionsExt;
            let dir =
                std::env::temp_dir().join(format!("ls-earlier-{version}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir(&dir).unwrap();
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
            let listener = std::os::unix::net::UnixListener::bind(dir.join("daemon.sock")).unwrap();
            std::fs::set_permissions(
                dir.join("daemon.sock"),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
            let peer = std::thread::spawn(move || {
                let mut asked = Vec::new();
                for _ in 0..2 {
                    let (stream, _) = listener.accept().unwrap();
                    let mut line = String::new();
                    BufReader::new(&stream).read_line(&mut line).unwrap();
                    let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                    // It reads requests with `deny_unknown_fields`, as
                    // every earlier build does, and answers one it cannot
                    // read without its request ID (review comment
                    // 4715859499).
                    let unread = request.get("capabilities").is_some();
                    let outcome = if unread {
                        serde_json::json!({"type": "error", "code": "invalid_request",
                            "message": "expected a typed version 1 request; unknown fields are rejected"})
                    } else if request["version"] == version {
                        serde_json::json!({"type": "error", "code": "answered", "message": "in its version"})
                    } else {
                        serde_json::json!({"type": "error", "code": "unsupported_version",
                            "message": "expected another protocol version; restart the daemon after upgrading LangSpan"})
                    };
                    let id = if unread {
                        serde_json::json!("")
                    } else {
                        request["request_id"].clone()
                    };
                    let reply = serde_json::json!({"version": version, "request_id": id,
                        "replayed": false, "generation": 1, "lease_valid": null, "outcome": outcome});
                    writeln!(&stream, "{reply}").unwrap();
                    asked.push(request);
                }
                asked
            });
            let request = crate::protocol::Request::new(
                "read",
                langspan_core::Command::Brief {
                    session_id: "s".into(),
                    max_peers: 8,
                    deliver: true,
                    mid_turn: false,
                    left: Vec::new(),
                    recent: false,
                    recent_window_ms: None,
                    mcp: false,
                    changes_only: true,
                },
            );
            let response = call(&dir, &request).unwrap();
            let asked = peer.join().unwrap();
            std::fs::remove_dir_all(&dir).unwrap();
            assert_eq!(
                asked
                    .iter()
                    .map(|r| r["version"].clone())
                    .collect::<Vec<_>>(),
                [
                    serde_json::json!(crate::protocol::PROTOCOL_VERSION),
                    serde_json::json!(version)
                ]
            );
            // The same request, except that a briefing of changes only,
            // which a daemon before version 7 cannot read, is asked of it
            // as the full view (review follow-up #259). A daemon of version
            // 7 or later, which gives changes only, is asked for them still.
            assert_eq!(asked[0]["command"]["changes_only"], true);
            assert!(asked[0].get("capabilities").is_some());
            assert!(asked[1].get("capabilities").is_none(), "{}", asked[1]);
            if version >= crate::protocol::PROTOCOL_VERSION_7 {
                assert_eq!(asked[0]["command"], asked[1]["command"]);
            } else {
                let mut full = asked[0]["command"].clone();
                full.as_object_mut().unwrap().shift_remove("changes_only");
                assert_eq!(full, asked[1]["command"]);
            }
            assert_eq!(asked[1]["request_id"], "read");
            assert_eq!(response.version, version);
            assert!(
                matches!(&response.outcome, Outcome::Error { code, .. } if code == "answered"),
                "{response:?}"
            );
        }
    }
}

#[cfg(unix)]
pub use unix::{accepts, call, call_until};

#[cfg(not(unix))]
pub fn call_until(
    state_dir: &std::path::Path,
    request: &crate::protocol::Request,
    _deadline: std::time::Instant,
) -> Result<crate::protocol::Response, crate::Error> {
    call(state_dir, request)
}

/// Pages `full_state` reads before it gives up: far more than any real
/// history, so that a cursor that never ends cannot hold a caller forever.
const MAX_STATUS_PAGES: usize = 100_000;

/// Bytes of one record's JSON that `read_all` joins from its parts: far
/// more than any record within the core's limits.
const MAX_RECORD_BYTES: usize = 8 * 1024 * 1024;

/// Every record of the coordinator's state, history included, read page by
/// page through `ask` as `status --all` reads it and placed back into one
/// `State`. A record given in parts is joined and then applied. A record
/// that changes between pages is read as it was when its page was; one that
/// changes while its parts are read is read again from its start. `ask`
/// answers one command; the daemon's own threads and clients share this.
pub fn read_all(
    mut ask: impl FnMut(langspan_core::Command) -> Result<langspan_core::Outcome, crate::Error>,
) -> Result<langspan_core::State, crate::Error> {
    use langspan_core::{Command, Outcome, StatusRecord};
    let mut state = langspan_core::State::default();
    let mut part = String::new();
    let mut after = None;
    for _ in 0..MAX_STATUS_PAGES {
        let (records, next) = match ask(Command::StatusRecords {
            after,
            page_bytes: None,
        })? {
            Outcome::StatusRecords { records, next } => (records, next),
            Outcome::Error { code, message } => return Err(format!("{code}: {message}").into()),
            other => return Err(format!("unexpected reply to a status page: {other:?}").into()),
        };
        for record in records {
            let record = match record {
                StatusRecord::Part { text, offset, last } => {
                    if offset == 0 {
                        part.clear();
                    } else if u64::try_from(part.len()).ok() != Some(offset) {
                        return Err(format!(
                            "a status record part starts at byte {offset}, but {} byte(s) of it were read; read the state again",
                            part.len()
                        )
                        .into());
                    }
                    if part.len() + text.len() > MAX_RECORD_BYTES {
                        return Err(format!(
                            "a status record is larger than {MAX_RECORD_BYTES} bytes"
                        )
                        .into());
                    }
                    part.push_str(&text);
                    if !last {
                        continue;
                    }
                    let record: StatusRecord = serde_json::from_str(&std::mem::take(&mut part))
                        .map_err(|error| format!("a status record's parts do not join: {error}"))?;
                    if matches!(record, StatusRecord::Part { .. }) {
                        return Err("a status record part holds another part".into());
                    }
                    record
                }
                // A record begun on the last page that is now given whole
                // changed meanwhile; the whole one is current.
                record => {
                    part.clear();
                    record
                }
            };
            record
                .apply(&mut state)
                .map_err(|error| format!("{}: {}", error.code, error.message))?;
        }
        match next {
            Some(cursor) => after = Some(cursor),
            None => return Ok(state),
        }
    }
    Err(format!("status records did not end within {MAX_STATUS_PAGES} pages").into())
}

/// One read-only request to the coordinator at `state_dir`, as `ask` gives
/// it to `read_all` and `status_with`. The generation it was answered in is
/// kept in `generation`.
fn read(
    state_dir: &std::path::Path,
    generation: &mut u64,
    command: langspan_core::Command,
) -> Result<langspan_core::Outcome, crate::Error> {
    let response = call(state_dir, &crate::protocol::Request::new("read", command))?;
    *generation = response.generation;
    Ok(response.outcome)
}

/// Every record of the state, as `read_all` reads it from the coordinator
/// at `state_dir`. The generation is the one the last page was answered in.
pub fn full_state(state_dir: &std::path::Path) -> Result<langspan_core::State, crate::Error> {
    let mut generation = 0;
    let mut state = read_all(|command| read(state_dir, &mut generation, command))?;
    state.generation = generation;
    Ok(state)
}

/// The bounded status view, with each part named in `needed` complete: if
/// the view left one out to fit a frame, it is read back from every record
/// (`read_all`). A caller that decides from one of these parts (a retry's
/// earlier landing, a workspace's scan) names it here rather than trusting
/// a view that may have left it out. Names are those of
/// `StatusSummary::omitted`.
pub fn status_with(
    mut ask: impl FnMut(langspan_core::Command) -> Result<langspan_core::Outcome, crate::Error>,
    needed: &[&str],
) -> Result<(langspan_core::State, langspan_core::StatusSummary), crate::Error> {
    use langspan_core::{Command, Outcome};
    let (mut view, mut summary) = match ask(Command::Status)? {
        Outcome::Status { state, summary } => (state, summary.unwrap_or_default()),
        Outcome::Error { code, message } => return Err(format!("{code}: {message}").into()),
        other => return Err(format!("unexpected reply to status: {other:?}").into()),
    };
    if summary
        .omitted
        .iter()
        .any(|name| needed.contains(&name.as_str()))
    {
        let full = read_all(&mut ask)?;
        langspan_core::restore_status(&mut view, &mut summary, &full, needed);
    }
    Ok((view, summary))
}

/// `status_with` from the coordinator at `state_dir`.
pub fn status_from(
    state_dir: &std::path::Path,
    needed: &[&str],
) -> Result<(langspan_core::State, langspan_core::StatusSummary), crate::Error> {
    let mut generation = 0;
    status_with(|command| read(state_dir, &mut generation, command), needed)
}

#[cfg(not(unix))]
pub fn call(
    _state_dir: &std::path::Path,
    _request: &crate::protocol::Request,
) -> Result<crate::protocol::Response, crate::Error> {
    Err("this initial transport requires Unix sockets; Windows support is not implemented".into())
}
