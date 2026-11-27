//! The coordinator as a per-user login service (issue #159): a LaunchAgent
//! on macOS and a `systemd --user` unit on Linux. `langspan setup` writes
//! and starts it; `langspan uninstall --apply` stops and removes it.
//!
//! The service runs one installed binary as `langspan daemon --state-dir
//! DIR`, restarts it when it fails, and appends its output to `daemon.log`
//! in that state directory. LangSpan recognizes its own file by the marker
//! line it writes and leaves any other file at the same path alone.

use crate::install::detect::UserHome;
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
pub use crate::hosts::HOST_DIRS;
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
    /// absolute. `env` reads the environment setup runs with. The service
    /// keeps the usable directories of its `PATH` (see `service_path`), so a
    /// check the coordinator runs finds the same tools as one run from that
    /// shell, and each host directory override in `HOST_DIRS`, so the
    /// coordinator's host checks read the directories setup installed into.
    pub fn render(
        self,
        executable: &Path,
        state_dir: &Path,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<String, crate::Error> {
        let mut environment = Vec::new();
        if let Some(path) = env("PATH").as_deref().and_then(service_path) {
            environment.push(("PATH", path));
        }
        for name in HOST_DIRS {
            let Some(value) = env(name).filter(|value| !value.is_empty()) else {
                continue;
            };
            // `UserHome` takes a relative override as setup's working
            // directory resolves it; the service runs in `/`.
            let absolute = std::path::absolute(&value)?;
            let value = absolute
                .to_str()
                .ok_or_else(|| format!("{name}={} is not valid UTF-8", absolute.display()))?;
            if self == Manager::Systemd && !unit_value(value) {
                return Err(format!(
                    "cannot write a systemd unit with {name}={value}: it holds a control character, a quote, `\\`, `%` or `$`, which systemd would expand; choose another directory, then run setup again"
                )
                .into());
            }
            environment.push((name, value.to_owned()));
        }
        match self {
            Manager::Launchd => launch_agent(executable, state_dir, &environment),
            Manager::Systemd => systemd_unit(executable, state_dir, &environment),
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
fn launch_agent(
    executable: &Path,
    state_dir: &Path,
    environment: &[(&str, String)],
) -> Result<String, crate::Error> {
    let exe = xml_escape(text_of(executable)?);
    let dir = xml_escape(text_of(state_dir)?);
    let log = xml_escape(text_of(&state_dir.join(crate::autostart::LOG))?);
    let environment = if environment.is_empty() {
        String::new()
    } else {
        let entries: String = environment
            .iter()
            .map(|(name, value)| {
                format!(
                    "\t\t<key>{name}</key>\n\t\t<string>{}</string>\n",
                    xml_escape(value)
                )
            })
            .collect();
        format!("\t<key>EnvironmentVariables</key>\n\t<dict>\n{entries}\t</dict>\n")
    };
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
{environment}	<key>StandardOutPath</key>
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
fn systemd_unit(
    executable: &Path,
    state_dir: &Path,
    environment: &[(&str, String)],
) -> Result<String, crate::Error> {
    let exe = unit_word(executable)?;
    let dir = unit_word(state_dir)?;
    let log_path = state_dir.join(crate::autostart::LOG);
    let log = unit_word(&log_path)?;
    let environment: String = environment
        .iter()
        .map(|(name, value)| format!("Environment=\"{name}={value}\"\n"))
        .collect();
    Ok(format!(
        "# {MARKER}
[Unit]
Description=LangSpan coordinator
Documentation=https://langspan.dev

[Service]
Type=simple
ExecStart=\"{exe}\" daemon --state-dir \"{dir}\"
{environment}Restart=on-failure
RestartSec=5
StandardOutput=append:{log}
StandardError=append:{log}

[Install]
WantedBy=default.target
"
    ))
}

/// The directories of `search_path` a service can name unchanged, joined
/// with `:`, or `None` when none is left. A relative directory, which the
/// manager would resolve against `/`, is left out, as is one holding a
/// control character, a quote, `\`, `%` or `$`, which systemd would expand.
fn service_path(search_path: &str) -> Option<String> {
    let kept: Vec<&str> = search_path
        .split(':')
        .filter(|dir| dir.starts_with('/') && unit_value(dir))
        .collect();
    (!kept.is_empty()).then(|| kept.join(":"))
}

/// Whether systemd reads `value` unchanged inside a quoted
/// `Environment="NAME=value"`: no control character, quote, `\`, `%` or `$`.
fn unit_value(value: &str) -> bool {
    !value
        .chars()
        .any(|c| c.is_control() || matches!(c, '"' | '\'' | '\\' | '%' | '$'))
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

/// Replaces a service file with `text` at once; a new file is owner-only.
/// `planned` is what the file held when setup showed its plan (`None`:
/// absent). If the file differs from that just before the rename, nothing
/// is written and the error says to run setup again, so an edit made while
/// setup waited is never overwritten.
pub fn write(path: &Path, planned: Option<&str>, text: &str) -> Result<(), crate::Error> {
    let refuse_changed = |now: Option<&[u8]>| -> Result<Vec<u8>, crate::Error> {
        if now != planned.map(str::as_bytes) {
            return Err(format!(
                "{} changed after setup showed its plan; nothing was written. Run setup again to see the new plan",
                path.display()
            )
            .into());
        }
        Ok(text.as_bytes().to_vec())
    };
    crate::install::write_planned(path, planned.map(str::as_bytes), &refuse_changed)?;
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

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn write_refuses_a_service_file_that_changed_after_the_plan() {
        let base = std::env::temp_dir().join(format!("ls-service-write-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        // The plan saw "planned"; the file is edited before setup applies it.
        let path = base.join("langspan.service");
        std::fs::write(&path, "edited\n").unwrap();
        let error = write(&path, Some("planned\n"), "new\n").unwrap_err();
        assert!(error.to_string().contains("changed"), "{error}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "edited\n");
        // The plan saw no file; one appears before setup applies it.
        let appeared = base.join("appeared.service");
        std::fs::write(&appeared, "not ours\n").unwrap();
        assert!(write(&appeared, None, "new\n").is_err());
        assert_eq!(std::fs::read_to_string(&appeared).unwrap(), "not ours\n");
        // Unchanged since the plan: replaced; a new file is owner-only.
        write(&path, Some("edited\n"), "new\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\n");
        let absent = base.join("absent.service");
        write(&absent, None, "new\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&absent).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn launch_agent_runs_the_daemon_at_login_and_after_a_failure() {
        let text = Manager::Launchd
            .render(
                Path::new("/opt/bin/langspan"),
                Path::new("/srv/a & b/.local/state/langspan"),
                &no_env,
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
            .render(Path::new("/x/<y>/langspan"), Path::new("/s/'q\""), &no_env)
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
                &no_env,
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
                .render(Path::new("/opt/langspan"), Path::new(dir), &no_env)
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("cannot write a systemd unit"),
                "{dir:?}: {error}"
            );
        }
        assert!(
            Manager::Systemd
                .render(Path::new("relative/langspan"), Path::new("/s"), &no_env)
                .is_err()
        );
    }

    #[test]
    fn both_services_keep_the_tool_search_path_setup_ran_with() {
        let search = "/home/u/.cargo/bin:relative/bin:/opt/a b/bin:/x/%h:/y/$HOME:/usr/bin";
        let env = |name: &str| (name == "PATH").then(|| search.to_owned());
        let launchd = Manager::Launchd
            .render(Path::new("/opt/langspan"), Path::new("/s"), &env)
            .unwrap();
        assert!(
            launchd.contains(
                "\t<key>EnvironmentVariables</key>\n\t<dict>\n\t\t<key>PATH</key>\n\t\t<string>/home/u/.cargo/bin:/opt/a b/bin:/usr/bin</string>\n\t</dict>\n"
            ),
            "{launchd}"
        );
        let systemd = Manager::Systemd
            .render(Path::new("/opt/langspan"), Path::new("/s"), &env)
            .unwrap();
        assert!(
            systemd.contains("Environment=\"PATH=/home/u/.cargo/bin:/opt/a b/bin:/usr/bin\"\n"),
            "{systemd}"
        );
        assert_eq!(
            Manager::Systemd.state_dir_of(&systemd),
            Some(PathBuf::from("/s"))
        );
        // No usable directory: no entry, and the manager's own default.
        for manager in [Manager::Launchd, Manager::Systemd] {
            for search in [None, Some(""), Some("relative:/a\"b")] {
                let text = manager
                    .render(
                        Path::new("/opt/langspan"),
                        Path::new("/s"),
                        &|name: &str| search.filter(|_| name == "PATH").map(str::to_owned),
                    )
                    .unwrap();
                assert!(!text.contains("PATH"), "{search:?}: {text}");
            }
        }
    }

    #[test]
    fn both_services_keep_the_host_directories_setup_installed_into() {
        let env = |name: &str| match name {
            "PATH" => Some("/usr/bin".to_owned()),
            "CLAUDE_CONFIG_DIR" => Some("/cfg/claude & co".to_owned()),
            "CODEX_HOME" => Some("/cfg/codex".to_owned()),
            "GEMINI_CLI_HOME" => Some(String::new()),
            "ANTHROPIC_API_KEY" => Some("secret".to_owned()),
            _ => None,
        };
        let launchd = Manager::Launchd
            .render(Path::new("/opt/langspan"), Path::new("/s"), &env)
            .unwrap();
        assert!(
            launchd.contains(
                "\t<key>EnvironmentVariables</key>\n\t<dict>\n\t\t<key>PATH</key>\n\t\t<string>/usr/bin</string>\n\t\t<key>CLAUDE_CONFIG_DIR</key>\n\t\t<string>/cfg/claude &amp; co</string>\n\t\t<key>CODEX_HOME</key>\n\t\t<string>/cfg/codex</string>\n\t</dict>\n"
            ),
            "{launchd}"
        );
        let systemd_env = |name: &str| match name {
            "CLAUDE_CONFIG_DIR" => Some("/cfg/claude co".to_owned()),
            "CODEX_HOME" => Some("/cfg/codex".to_owned()),
            _ => None,
        };
        let systemd = Manager::Systemd
            .render(Path::new("/opt/langspan"), Path::new("/s"), &systemd_env)
            .unwrap();
        assert!(
            systemd.contains(
                "Environment=\"CLAUDE_CONFIG_DIR=/cfg/claude co\"\nEnvironment=\"CODEX_HOME=/cfg/codex\"\n"
            ),
            "{systemd}"
        );
        for text in [&launchd, &systemd] {
            assert!(!text.contains("GEMINI_CLI_HOME"), "{text}");
            assert!(!text.contains("secret"), "{text}");
        }
        // A directory systemd would expand is refused, not dropped: the
        // service would otherwise read another directory than setup used.
        let error = Manager::Systemd
            .render(
                Path::new("/opt/langspan"),
                Path::new("/s"),
                &|name: &str| (name == "CODEX_HOME").then(|| "/cfg/$HOME".to_owned()),
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("CODEX_HOME"), "{error}");
        // A relative directory is resolved where setup runs, as install
        // resolved it.
        let relative = Manager::Launchd
            .render(
                Path::new("/opt/langspan"),
                Path::new("/s"),
                &|name: &str| (name == "CODEX_HOME").then(|| "rel/codex".to_owned()),
            )
            .unwrap();
        let absolute = std::path::absolute("rel/codex").unwrap();
        assert!(
            relative.contains(&format!("<string>{}</string>", absolute.display())),
            "{relative}"
        );
    }

    #[test]
    fn a_file_without_the_marker_is_not_ours() {
        let text = "[Service]\nExecStart=\"/x\" daemon --state-dir \"/s\"\n";
        assert_eq!(Manager::Systemd.state_dir_of(text), None);
        assert_eq!(Manager::Launchd.state_dir_of("<plist/>"), None);
    }
}
