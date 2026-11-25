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

pub fn identify(pid: u32) -> Result<ProcessIdentity, String> {
    #[cfg(target_os = "linux")]
    {
        linux::sample(pid)
            .map(|s| s.identity)
            .map_err(|e| format!("cannot identify process: {e}"))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        Err("process-incarnation observation is currently supported on Linux only".into())
    }
}

pub fn probe(process: &ProcessIdentity) -> LivenessStatus {
    #[cfg(target_os = "linux")]
    {
        match linux::sample(process.pid) {
            Ok(sample) if sample.identity != *process || sample.zombie => LivenessStatus::Exited,
            Ok(_) => LivenessStatus::Alive,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => LivenessStatus::Exited,
            Err(_) => LivenessStatus::Unknown,
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = process;
        LivenessStatus::Unknown
    }
}

/// Only accept a recognised native host ancestor, never the short-lived hook or
/// shell itself. Ambiguous process trees remain unknown and can be attached
/// explicitly through the CLI. Process arguments/environment are never inspected.
pub fn host_parent(source: &str) -> Option<ProcessIdentity> {
    #[cfg(target_os = "linux")]
    {
        let mut pid = u32::try_from(rustix::process::getppid()?.as_raw_nonzero().get()).ok()?;
        for _ in 0..8 {
            let sample = linux::sample(pid).ok()?;
            let recognised = match source {
                "codex" => sample.name == "codex" || sample.name.starts_with("codex-"),
                "claude-code" => sample.name == "claude" || sample.name == "claude-code",
                _ => false,
            };
            if recognised && !sample.zombie {
                return Some(sample.identity);
            }
            if sample.parent <= 1 || sample.parent == pid {
                return None;
            }
            pid = sample.parent;
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = source;
        None
    }
}

#[cfg(all(test, target_os = "linux"))]
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
}
