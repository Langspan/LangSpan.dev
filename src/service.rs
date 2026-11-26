//! The coordinator as a per-user login service (issue #159): a LaunchAgent
//! on macOS and a `systemd --user` unit on Linux. `langspan setup` writes
//! and starts it; `langspan uninstall --apply` stops and removes it.
//!
//! The service runs one installed binary as `langspan daemon --state-dir
//! DIR`, restarts it when it fails, and appends its output to `daemon.log`
//! in that state directory. LangSpan recognizes its own file by the marker
//! line it writes and leaves any other file at the same path alone.

use crate::hosts::UserHome;
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

/// The LaunchAgent label, also its file name without `.plist`.
pub const LABEL: &str = "dev.langspan.coordinator";
/// The systemd unit name.
pub const UNIT: &str = "langspan.service";
/// The marker line in every file this module writes.
pub const MARKER: &str = "Written by `langspan setup`; `langspan uninstall --apply` removes it.";
/// The bound on one service-manager command.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
/// The largest service file read.
const MAX_FILE_BYTES: u64 = 64 << 10;

/// Which service manager this platform uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Manager {
    Launchd,
    Systemd,
}

impl Manager {
    /// The one for the platform this binary was built for.
    pub fn current() -> Option<Self> {
        if cfg!(target_os = "macos") {
            Some(Manager::Launchd)
        } else if cfg!(target_os = "linux") {
            Some(Manager::Systemd)
        } else {
            None
        }
    }

    /// Where the service file lives in `home`.
    pub fn file(self, home: &UserHome) -> PathBuf {
        match self {
            Manager::Launchd => home
                .home
                .join("Library/LaunchAgents")
                .join(format!("{LABEL}.plist")),
            Manager::Systemd => home
                .env_path("XDG_CONFIG_HOME")
                .filter(|path| path.is_absolute())
                .unwrap_or_else(|| home.home.join(".config"))
                .join("systemd/user")
                .join(UNIT),
        }
    }

    /// The service file's text for `executable` and `state_dir`, both
    /// absolute.
    pub fn render(self, executable: &Path, state_dir: &Path) -> Result<String, crate::Error> {
        match self {
            Manager::Launchd => launch_agent(executable, state_dir),
            Manager::Systemd => systemd_unit(executable, state_dir),
        }
    }

    /// The state directory a service file this module wrote names, or
    /// `None` for a file it did not write.
    pub fn state_dir_of(self, text: &str) -> Option<PathBuf> {
        if !text.contains(MARKER) {
            return None;
        }
        match self {
            Manager::Launchd => {
                let rest = text.split_once("<string>--state-dir</string>")?.1;
                let value = rest.split_once("<string>")?.1.split_once("</string>")?.0;
                Some(PathBuf::from(xml_unescape(value)))
            }
            Manager::Systemd => {
                let line = text.lines().find(|line| line.starts_with("ExecStart="))?;
                let value = line.split_once("--state-dir \"")?.1.split_once('"')?.0;
                Some(PathBuf::from(value))
            }
        }
    }
}

/// A LaunchAgent: started at login (`RunAtLoad`), started again when it
/// exits with a failure (`KeepAlive` / `SuccessfulExit` false).
fn launch_agent(executable: &Path, state_dir: &Path) -> Result<String, crate::Error> {
    let exe = xml_escape(text_of(executable)?);
    let dir = xml_escape(text_of(state_dir)?);
    let log = xml_escape(text_of(&state_dir.join(crate::autostart::LOG))?);
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- {MARKER} -->
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{exe}</string>
		<string>daemon</string>
		<string>--state-dir</string>
		<string>{dir}</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<key>ProcessType</key>
	<string>Background</string>
	<key>StandardOutPath</key>
	<string>{log}</string>
	<key>StandardErrorPath</key>
	<string>{log}</string>
</dict>
</plist>
"#
    ))
}

/// A `systemd --user` unit, wanted by `default.target` so it starts at
/// login, restarted five seconds after a failure.
fn systemd_unit(executable: &Path, state_dir: &Path) -> Result<String, crate::Error> {
    let exe = unit_word(executable)?;
    let dir = unit_word(state_dir)?;
    let log_path = state_dir.join(crate::autostart::LOG);
    let log = unit_word(&log_path)?;
    Ok(format!(
        "# {MARKER}
[Unit]
Description=LangSpan coordinator
Documentation=https://langspan.dev

[Service]
Type=simple
ExecStart=\"{exe}\" daemon --state-dir \"{dir}\"
Restart=on-failure
RestartSec=5
StandardOutput=append:{log}
StandardError=append:{log}

[Install]
WantedBy=default.target
"
    ))
}

fn text_of(path: &Path) -> Result<&str, crate::Error> {
    if !path.is_absolute() {
        return Err(format!("{} is not an absolute path", path.display()).into());
    }
    path.to_str()
        .ok_or_else(|| format!("{} is not valid UTF-8", path.display()).into())
}

/// A path as systemd reads it unchanged in `ExecStart=` and `append:`: no
/// whitespace, quote, backslash, `%` or `$`, which systemd would expand or
/// split.
fn unit_word(path: &Path) -> Result<&str, crate::Error> {
    let text = text_of(path)?;
    if let Some(bad) = text
        .chars()
        .find(|c| c.is_whitespace() || c.is_control() || matches!(c, '"' | '\'' | '\\' | '%' | '$'))
    {
        return Err(format!(
            "cannot write a systemd unit for {:?}: it contains {bad:?}. Choose a state directory and install location without spaces, quotes, `\\`, `%` or `$`",
            text
        )
        .into());
    }
    Ok(text)
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn xml_unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// The service file's text, if there is one, read with the bounds `install`
/// uses.
pub fn read(path: &Path) -> Result<Option<String>, crate::Error> {
    crate::install::read_text(path)
}

/// Writes a service file owner-only, replacing the whole file at once.
pub fn write(path: &Path, text: &str) -> Result<(), crate::Error> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let temporary = path.with_extension("langspan-new");
    let _ = std::fs::remove_file(&temporary);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&temporary, path)?;
    Ok(())
}

/// Whether a login service this module wrote is installed in `home` for
/// `state_dir`. Any failure to read it counts as not installed.
pub fn installed_for(home: &UserHome, state_dir: &Path) -> bool {
    let Some(manager) = Manager::current() else {
        return false;
    };
    let path = manager.file(home);
    let Ok(Some(text)) = read_bounded(&path) else {
        return false;
    };
    manager
        .state_dir_of(&text)
        .is_some_and(|named| crate::install::same_directory(&named, state_dir))
}

/// As `read`, for the hook's hot path: one bounded read, no other checks.
fn read_bounded(path: &Path) -> std::io::Result<Option<String>> {
    match std::fs::File::open(path) {
        Ok(file) => {
            let mut text = String::new();
            file.take(MAX_FILE_BYTES).read_to_string(&mut text)?;
            Ok(Some(text))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// What a service-manager command did.
#[derive(Debug)]
pub struct Ran {
    pub success: bool,
    /// The start of its output, trimmed, for a message.
    pub said: String,
}

/// Runs `program` with `args`, bounded by `COMMAND_TIMEOUT`, with no
/// standard input. A program that cannot be started is an error.
pub fn run(program: &str, args: &[&str]) -> Result<Ran, crate::Error> {
    use std::os::unix::process::CommandExt;
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|error| format!("cannot run {program}: {error}"))?;
    loop {
        if let Some(status) = child.try_wait()? {
            let mut said = String::new();
            for pipe in [
                child.stdout.take().map(|p| Box::new(p) as Box<dyn Read>),
                child.stderr.take().map(|p| Box::new(p) as Box<dyn Read>),
            ]
            .into_iter()
            .flatten()
            {
                let _ = pipe.take(4096).read_to_string(&mut said);
            }
            let said = said.trim();
            return Ok(Ran {
                success: status.success(),
                said: said.chars().take(400).collect(),
            });
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "{program} {} did not finish within {} seconds",
                args.join(" "),
                COMMAND_TIMEOUT.as_secs()
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The steps that start an installed service file, as `(program, args)`.
pub fn start_commands(manager: Manager, path: &Path) -> Vec<(String, Vec<String>)> {
    match manager {
        Manager::Launchd => {
            let domain = format!("gui/{}", rustix::process::getuid().as_raw());
            vec![
                // Loaded already (an earlier version of the file): unload it
                // first, or bootstrap fails. Its failure when nothing is
                // loaded is expected.
                (
                    "launchctl".into(),
                    vec!["bootout".into(), format!("{domain}/{LABEL}")],
                ),
                (
                    "launchctl".into(),
                    vec!["bootstrap".into(), domain, path.display().to_string()],
                ),
            ]
        }
        Manager::Systemd => vec![
            (
                "systemctl".into(),
                vec!["--user".into(), "daemon-reload".into()],
            ),
            (
                "systemctl".into(),
                vec!["--user".into(), "enable".into(), UNIT.into()],
            ),
            // Starts it, or restarts one running from an earlier file.
            (
                "systemctl".into(),
                vec!["--user".into(), "restart".into(), UNIT.into()],
            ),
        ],
    }
}

/// The steps that stop a service and keep it from starting at login.
pub fn stop_commands(manager: Manager) -> Vec<(String, Vec<String>)> {
    match manager {
        Manager::Launchd => vec![(
            "launchctl".into(),
            vec![
                "bootout".into(),
                format!("gui/{}/{LABEL}", rustix::process::getuid().as_raw()),
            ],
        )],
        Manager::Systemd => vec![(
            "systemctl".into(),
            vec![
                "--user".into(),
                "disable".into(),
                "--now".into(),
                UNIT.into(),
            ],
        )],
    }
}

/// Whether the service manager for this user can be reached at all: for
/// systemd, a user manager is running; launchd always is on macOS.
pub fn manager_available(manager: Manager) -> Result<(), String> {
    match manager {
        Manager::Launchd => Ok(()),
        Manager::Systemd => match run("systemctl", &["--user", "show", "--property=Version"]) {
            Ok(ran) if ran.success => Ok(()),
            Ok(ran) => Err(format!(
                "`systemctl --user` cannot reach a user manager: {}",
                ran.said
            )),
            Err(error) => Err(error.to_string()),
        },
    }
}

/// Whether the service is loaded and running now.
pub fn running(manager: Manager) -> bool {
    match manager {
        Manager::Launchd => run(
            "launchctl",
            &[
                "print",
                &format!("gui/{}/{LABEL}", rustix::process::getuid().as_raw()),
            ],
        )
        .is_ok_and(|ran| ran.success && ran.said.contains("state = running")),
        Manager::Systemd => {
            run("systemctl", &["--user", "is-active", "--quiet", UNIT]).is_ok_and(|ran| ran.success)
                && run("systemctl", &["--user", "is-enabled", "--quiet", UNIT])
                    .is_ok_and(|ran| ran.success)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_agent_runs_the_daemon_at_login_and_after_a_failure() {
        let text = Manager::Launchd
            .render(
                Path::new("/opt/bin/langspan"),
                Path::new("/srv/a & b/.local/state/langspan"),
            )
            .unwrap();
        let expected = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- Written by `langspan setup`; `langspan uninstall --apply` removes it. -->
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>dev.langspan.coordinator</string>
	<key>ProgramArguments</key>
	<array>
		<string>/opt/bin/langspan</string>
		<string>daemon</string>
		<string>--state-dir</string>
		<string>/srv/a &amp; b/.local/state/langspan</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<key>ProcessType</key>
	<string>Background</string>
	<key>StandardOutPath</key>
	<string>/srv/a &amp; b/.local/state/langspan/daemon.log</string>
	<key>StandardErrorPath</key>
	<string>/srv/a &amp; b/.local/state/langspan/daemon.log</string>
</dict>
</plist>
"#;
        assert_eq!(text, expected);
        assert_eq!(
            Manager::Launchd.state_dir_of(&text),
            Some(PathBuf::from("/srv/a & b/.local/state/langspan"))
        );
    }

    #[test]
    fn launch_agent_escapes_markup_in_paths() {
        let text = Manager::Launchd
            .render(Path::new("/x/<y>/langspan"), Path::new("/s/'q\""))
            .unwrap();
        assert!(text.contains("<string>/x/&lt;y&gt;/langspan</string>"));
        assert!(text.contains("<string>/s/&apos;q&quot;</string>"));
        assert_eq!(
            Manager::Launchd.state_dir_of(&text),
            Some(PathBuf::from("/s/'q\""))
        );
    }

    #[test]
    fn systemd_unit_names_the_binary_state_directory_and_log() {
        let text = Manager::Systemd
            .render(
                Path::new("/opt/bin/langspan"),
                Path::new("/home/u/.local/state/langspan"),
            )
            .unwrap();
        assert!(text.starts_with("# Written by `langspan setup`"));
        assert!(text.contains(
            "ExecStart=\"/opt/bin/langspan\" daemon --state-dir \"/home/u/.local/state/langspan\"\n"
        ));
        assert!(text.contains("Restart=on-failure\n"));
        assert!(text.contains("StandardError=append:/home/u/.local/state/langspan/daemon.log\n"));
        assert!(text.contains("WantedBy=default.target\n"));
        assert_eq!(
            Manager::Systemd.state_dir_of(&text),
            Some(PathBuf::from("/home/u/.local/state/langspan"))
        );
    }

    #[test]
    fn systemd_unit_refuses_paths_systemd_would_change() {
        for dir in ["/a b", "/a%h", "/a$HOME", "/a\"b", "/a\\b", "/a\nb"] {
            let error = Manager::Systemd
                .render(Path::new("/opt/langspan"), Path::new(dir))
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("cannot write a systemd unit"),
                "{dir:?}: {error}"
            );
        }
        assert!(
            Manager::Systemd
                .render(Path::new("relative/langspan"), Path::new("/s"))
                .is_err()
        );
    }

    #[test]
    fn a_file_without_the_marker_is_not_ours() {
        let text = "[Service]\nExecStart=\"/x\" daemon --state-dir \"/s\"\n";
        assert_eq!(Manager::Systemd.state_dir_of(text), None);
        assert_eq!(Manager::Launchd.state_dir_of("<plist/>"), None);
    }
}
