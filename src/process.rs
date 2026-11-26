//! Process-incarnation observations; no credentials or process arguments are read.
use langspan_core::{LivenessStatus, ProcessIdentity};

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::{
        fs,
        io::{self, Read},
        os::unix::fs::MetadataExt,
    };
    pub struct Sample {
        pub identity: ProcessIdentity,
        pub parent: u32,
        pub name: String,
        pub zombie: bool,
    }
    fn bounded(path: &str) -> io::Result<String> {
        let mut text = String::new();
        fs::File::open(path)?.take(8193).read_to_string(&mut text)?;
        if text.len() > 8192 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "process metadata exceeds bound",
            ));
        }
        Ok(text)
    }
    pub fn sample(pid: u32) -> io::Result<Sample> {
        if pid == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "PID must be positive",
            ));
        }
        let metadata = fs::metadata(format!("/proc/{pid}"))?;
        if metadata.uid() != rustix::process::geteuid().as_raw() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "process belongs to another user",
            ));
        }
        let stat = bounded(&format!("/proc/{pid}/stat"))?;
        let (prefix, fields) = stat
            .rsplit_once(") ")
            .ok_or_else(|| io::Error::other("invalid process metadata"))?;
        let name = prefix
            .split_once('(')
            .map(|(_, name)| name)
            .ok_or_else(|| io::Error::other("invalid process name"))?;
        let fields: Vec<_> = fields.split_whitespace().collect();
        let parent = fields
            .get(1)
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| io::Error::other("invalid parent identity"))?;
        let start = fields
            .get(19)
            .filter(|v| v.bytes().all(|b| b.is_ascii_digit()))
            .ok_or_else(|| io::Error::other("missing process start identity"))?;
        let boot = bounded("/proc/sys/kernel/random/boot_id")?;
        let input = format!("langspan-process-v1\0{}\0{}\0{}", boot.trim(), pid, start);
        let start_token = crate::sha256::hex(&crate::sha256::digest(input.as_bytes()));
        Ok(Sample {
            identity: ProcessIdentity { pid, start_token },
            parent,
            name: name.into(),
            zombie: matches!(fields.first(), Some(&"Z") | Some(&"X")),
        })
    }
}

/// macOS: the kernel's process table (`sysctl` `KERN_PROC`) as `/bin/ps`
/// reports it, since LangSpan's own code stays free of `unsafe` and so
/// cannot call `proc_pidinfo` itself. Only the ID, parent, owner, state,
/// start time and accounting name are asked for; never arguments or
/// environment. The child has a clean environment, no input and a deadline.
#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use std::{
        collections::BTreeMap,
        io::{self, Read},
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    pub struct Sample {
        pub identity: ProcessIdentity,
        pub parent: u32,
        pub uid: u32,
        pub name: String,
        pub zombie: bool,
    }
    /// One `ps` run's output is at most this; a longer one is an error.
    const OUTPUT_LIMIT: usize = 1024 * 1024;
    /// A `ps` run that has not finished by then is stopped.
    const PS_TIMEOUT: Duration = Duration::from_secs(2);

    fn ps(select: &[&str]) -> io::Result<String> {
        let mut args = select.to_vec();
        args.extend(["-o", "pid=,ppid=,uid=,state=,lstart=,ucomm="]);
        listing("/bin/ps", &args)
    }

    /// Runs one listing program and returns its whole output.
    pub(super) fn listing(program: &str, args: &[&str]) -> io::Result<String> {
        let mut child = Command::new(program)
            .args(args)
            .env_clear()
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("ps output pipe missing"))?;
        // Read while it runs, so a long listing cannot fill the pipe.
        let reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout
                .by_ref()
                .take(OUTPUT_LIMIT as u64 + 1)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
        });
        let deadline = Instant::now() + PS_TIMEOUT;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break Some(status);
            }
            if Instant::now() >= deadline {
                break None;
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        if status.is_none() {
            let _ = child.kill();
        }
        let _ = child.wait();
        let bytes = reader
            .join()
            .map_err(|_| io::Error::other("ps output reader failed"))??;
        let Some(status) = status else {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "process listing timed out",
            ));
        };
        // `ps -p` exits 1, listing nothing, when none of the processes
        // exists; any other failure, or a signal, leaves the table
        // incomplete, and a process missing from it would read as exited.
        let none_found = status.code() == Some(1) && bytes.is_empty();
        if !status.success() && !none_found {
            return Err(io::Error::other(format!(
                "process listing failed ({status})"
            )));
        }
        if bytes.len() > OUTPUT_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "process listing exceeds bound",
            ));
        }
        String::from_utf8(bytes)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "process listing not UTF-8"))
    }

    /// One line: `PID PPID UID STATE Www Mmm dd hh:mm:ss yyyy NAME`, the
    /// name last because it may hold spaces.
    pub fn parse(line: &str) -> Option<Sample> {
        let mut rest = line.trim();
        let mut fields = Vec::with_capacity(9);
        for _ in 0..9 {
            let (field, tail) = rest.split_once(char::is_whitespace)?;
            fields.push(field);
            rest = tail.trim_start();
        }
        let name = rest.trim_end();
        let pid: u32 = fields[0].parse().ok()?;
        if pid == 0 || name.is_empty() {
            return None;
        }
        let start = fields[4..9].join(" ");
        let input = format!("langspan-process-v1-macos\0{pid}\0{start}");
        Some(Sample {
            identity: ProcessIdentity {
                pid,
                start_token: crate::sha256::hex(&crate::sha256::digest(input.as_bytes())),
            },
            parent: fields[1].parse().ok()?,
            uid: fields[2].parse().ok()?,
            name: name.into(),
            zombie: fields[3].contains('Z'),
        })
    }

    fn own(sample: Sample) -> io::Result<Sample> {
        if sample.uid != rustix::process::geteuid().as_raw() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "process belongs to another user",
            ));
        }
        Ok(sample)
    }

    /// The listed processes, by ID. A process that is gone is absent.
    pub fn samples(pids: &[u32]) -> io::Result<BTreeMap<u32, Sample>> {
        let list: Vec<String> = pids
            .iter()
            .filter(|pid| **pid != 0)
            .map(u32::to_string)
            .collect();
        if list.is_empty() {
            return Ok(BTreeMap::new());
        }
        let text = ps(&["-x", "-p", &list.join(",")])?;
        Ok(text
            .lines()
            .filter_map(parse)
            .map(|sample| (sample.identity.pid, sample))
            .collect())
    }

    pub fn sample(pid: u32) -> io::Result<Sample> {
        if pid == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "PID must be positive",
            ));
        }
        let sample = samples(&[pid])?
            .remove(&pid)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such process"))?;
        own(sample)
    }

    /// This user's processes only: an ancestor owned by anyone else ends
    /// the walk.
    pub fn own_processes() -> io::Result<BTreeMap<u32, Sample>> {
        let uid = rustix::process::geteuid().as_raw().to_string();
        let text = ps(&["-x", "-U", &uid])?;
        Ok(text
            .lines()
            .filter_map(parse)
            .filter_map(|sample| own(sample).ok())
            .map(|sample| (sample.identity.pid, sample))
            .collect())
    }
}

pub fn identify(pid: u32) -> Result<ProcessIdentity, String> {
    #[cfg(target_os = "linux")]
    {
        linux::sample(pid)
            .map(|s| s.identity)
            .map_err(|e| format!("cannot identify process: {e}"))
    }
    #[cfg(target_os = "macos")]
    {
        macos::sample(pid)
            .map(|s| s.identity)
            .map_err(|e| format!("cannot identify process: {e}"))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        Err("process-incarnation observation is supported on Linux and macOS only".into())
    }
}

/// What one sample says of the incarnation `process`: a different
/// incarnation in its PID, or a zombie, is the one that exited; a sample that
/// could not be read, or of another user's process, says nothing.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn status_of(
    process: &ProcessIdentity,
    sample: Result<(&ProcessIdentity, bool), std::io::ErrorKind>,
) -> LivenessStatus {
    match sample {
        Ok((identity, zombie)) if identity != process || zombie => LivenessStatus::Exited,
        Ok(_) => LivenessStatus::Alive,
        Err(std::io::ErrorKind::NotFound) => LivenessStatus::Exited,
        Err(_) => LivenessStatus::Unknown,
    }
}

pub fn probe(process: &ProcessIdentity) -> LivenessStatus {
    probe_all(std::slice::from_ref(process))
        .pop()
        .unwrap_or(LivenessStatus::Unknown)
}

/// Each process's status, in order. On macOS one process listing answers
/// for all of them; its failure leaves every one `Unknown`, never `Exited`.
pub fn probe_all(processes: &[ProcessIdentity]) -> Vec<LivenessStatus> {
    #[cfg(target_os = "linux")]
    {
        processes
            .iter()
            .map(|process| {
                let sample = linux::sample(process.pid);
                status_of(
                    process,
                    sample
                        .as_ref()
                        .map(|sample| (&sample.identity, sample.zombie))
                        .map_err(std::io::Error::kind),
                )
            })
            .collect()
    }
    #[cfg(target_os = "macos")]
    {
        let pids: Vec<u32> = processes.iter().map(|process| process.pid).collect();
        let listed = match macos::samples(&pids) {
            Ok(listed) => listed,
            Err(_) => return vec![LivenessStatus::Unknown; processes.len()],
        };
        let euid = rustix::process::geteuid().as_raw();
        processes
            .iter()
            .map(|process| {
                let sample = match listed.get(&process.pid) {
                    None if process.pid == 0 => Err(std::io::ErrorKind::InvalidInput),
                    None => Err(std::io::ErrorKind::NotFound),
                    Some(sample) if sample.uid != euid => Err(std::io::ErrorKind::PermissionDenied),
                    Some(sample) => Ok((&sample.identity, sample.zombie)),
                };
                status_of(process, sample)
            })
            .collect()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        vec![LivenessStatus::Unknown; processes.len()]
    }
}

/// Whether a process name is the named host's. Cursor and Gemini CLI are
/// added once their process names are seen on a real host (roadmap J12).
fn is_host(source: &str, name: &str) -> bool {
    match source {
        "codex" => name == "codex" || name.starts_with("codex-"),
        "claude-code" => name == "claude" || name == "claude-code",
        _ => false,
    }
}

/// Only accept a recognised native host ancestor, never the short-lived hook or
/// shell itself. Ambiguous process trees remain unknown and can be attached
/// explicitly through the CLI. Process arguments/environment are never inspected.
pub fn host_parent(source: &str) -> Option<ProcessIdentity> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        // No host whose processes are known: nothing to look for.
        if !matches!(source, "codex" | "claude-code") {
            return None;
        }
        #[cfg(target_os = "macos")]
        let mut own = macos::own_processes().ok()?;
        let mut pid = u32::try_from(rustix::process::getppid()?.as_raw_nonzero().get()).ok()?;
        for _ in 0..8 {
            #[cfg(target_os = "linux")]
            let sample = linux::sample(pid).ok()?;
            #[cfg(target_os = "macos")]
            let sample = own.remove(&pid)?;
            if is_host(source, &sample.name) && !sample.zombie {
                return Some(sample.identity);
            }
            if sample.parent <= 1 || sample.parent == pid {
                return None;
            }
            pid = sample.parent;
        }
        None
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = source;
        None
    }
}

#[cfg(all(test, target_os = "macos"))]
mod macos_tests {
    use super::*;

    #[test]
    fn a_listing_line_is_read_with_a_name_that_holds_spaces() {
        let sample =
            macos::parse("  4242   501   501 S    Wed Nov 25 09:12:01 2026     Google Chrome H")
                .unwrap();
        assert_eq!(sample.identity.pid, 4242);
        assert_eq!(sample.parent, 501);
        assert_eq!(sample.uid, 501);
        assert_eq!(sample.name, "Google Chrome H");
        assert!(!sample.zombie);
        assert_eq!(sample.identity.start_token.len(), 64);
        let zombie = macos::parse("7 1 501 Z+ Wed Nov 25 09:12:01 2026 sh").unwrap();
        assert!(zombie.zombie);
        for bad in [
            "",
            "4242 1 501 S Wed Nov 25 09:12:01 2026",
            "x 1 501 S Wed Nov 25 09:12:01 2026 sh",
            "0 1 501 S Wed Nov 25 09:12:01 2026 sh",
        ] {
            assert!(macos::parse(bad).is_none(), "{bad:?}");
        }
    }

    const LINE: &str = "4242 1 501 S Wed Nov 25 09:12:01 2026 sh";

    /// A listing that fails or is killed part-way is no listing: its
    /// partial output must never read as "these processes are gone".
    #[test]
    fn a_failed_or_killed_listing_is_an_error_not_an_empty_table() {
        for script in [
            format!("printf '{LINE}\\n'; kill -9 $$"),
            format!("printf '{LINE}\\n'; exit 2"),
            format!("printf '{LINE}\\n'; exit 1"),
            "kill -9 $$".to_string(),
            "exit 2".to_string(),
        ] {
            let result = macos::listing("/bin/sh", &["-c", &script]);
            assert!(result.is_err(), "{script:?} gave {result:?}");
        }
        assert_eq!(
            macos::listing("/bin/sh", &["-c", &format!("printf '{LINE}\\n'")]).unwrap(),
            format!("{LINE}\n")
        );
        // `ps -p` exits 1 with nothing listed when none of the processes
        // exists: that is a complete, empty table.
        assert_eq!(macos::listing("/bin/sh", &["-c", "exit 1"]).unwrap(), "");
    }

    /// The real `ps` really does exit 1 for a process that is not there,
    /// and that probe still says `exited`.
    #[test]
    fn a_listing_of_only_missing_processes_is_empty() {
        assert!(macos::samples(&[u32::MAX - 1]).unwrap().is_empty());
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    fn observes_a_real_process_incarnation_and_its_exit() {
        let mut child = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        let result = identify(child.id());
        if result.is_err() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let identity = result.unwrap();
        assert_eq!(probe(&identity), LivenessStatus::Alive);
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(probe(&identity), LivenessStatus::Exited);
    }

    #[test]
    fn a_reused_pid_is_not_the_same_process() {
        let mut identity = identify(std::process::id()).unwrap();
        identity.start_token = "0".repeat(64);
        assert_eq!(probe(&identity), LivenessStatus::Exited);
    }

    #[test]
    fn zero_is_never_a_valid_process_identity() {
        assert!(identify(0).is_err());
    }

    /// Process 1 belongs to the superuser. A probe of it is refused and says
    /// nothing either way: `unknown`, never `exited`.
    #[test]
    fn another_users_process_is_refused_and_unknown() {
        if rustix::process::geteuid().is_root() {
            return;
        }
        let refused = identify(1).unwrap_err();
        assert!(refused.contains("another user"), "{refused}");
        let theirs = ProcessIdentity {
            pid: 1,
            start_token: "0".repeat(64),
        };
        assert_eq!(probe(&theirs), LivenessStatus::Unknown);
    }

    /// One listing answers for several processes, each in order.
    #[test]
    fn several_processes_are_probed_at_once() {
        let mut child = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        let alive = identify(child.id());
        child.kill().unwrap();
        child.wait().unwrap();
        let gone = alive.unwrap();
        let mine = identify(std::process::id()).unwrap();
        assert_eq!(
            probe_all(&[mine, gone]),
            vec![LivenessStatus::Alive, LivenessStatus::Exited]
        );
        assert!(probe_all(&[]).is_empty());
    }
}
