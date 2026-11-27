//! `langspan hook`: the driver each host's hook runs, one event per process.

use crate::flags::{ParsedOptions, number, parse_options, state_dir};
use langspan::cli::options::{no_unused_options, outline_reads, presence, switch};
use langspan::cli::setup::start_coordinator;
use std::{path::PathBuf, process::ExitCode};

pub(crate) fn hook(input: Vec<String>) -> ExitCode {
    use std::os::unix::fs::OpenOptionsExt;
    // An earlier release's Gemini CLI hook: allowed, nothing coordinated or
    // recorded, whatever its options say (issue #246).
    if input
        .get(1)
        .and_then(|name| langspan::install::Vendor::from_name(name))
        .is_some_and(langspan::install::Vendor::removed)
    {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1500);
        let event = langspan::hook::read_stdin_until(deadline).unwrap_or_default();
        let root = input
            .windows(2)
            .find(|pair| pair[0] == "--root")
            .map(|pair| pair[1].as_str());
        print!("{}", langspan::gemini_removed::ANSWER);
        if let Some(notice) = langspan::gemini_removed::notice_for(&event, root) {
            eprintln!("{notice}");
        }
        return ExitCode::SUCCESS;
    }
    // Antigravity documents a JSON answer for every event, and 1.5.14 reads
    // `{}` on `PreToolUse` as a refusal, so it gets its event's fallback:
    // the explicit allow there, else `{}`.
    let fallback: Option<&'static str> = match input.get(1).map(String::as_str) {
        Some("antigravity") => Some(langspan::antigravity::fallback(
            input
                .windows(2)
                .find(|pair| pair[0] == "--event")
                .map(|pair| pair[1].as_str()),
        )),
        _ => None,
    };
    // The between-step watcher runs in the background, where exit 2 wakes
    // the model: it exits 2 only to tell a notice, and 0 for every failure,
    // its options' included (issue #288).
    let background_watcher = input.get(1).map(String::as_str) == Some("claude-code")
        && input
            .windows(2)
            .any(|pair| pair[0] == "--mode" && pair[1] == langspan::between_steps::MODE);
    if let Some(fallback) = fallback {
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            use std::io::Write;
            let mut stdout = std::io::stdout().lock();
            let _ = stdout
                .write_all(fallback.as_bytes())
                .and_then(|()| stdout.flush());
            default(info);
        }));
    }
    let result = (|| -> Result<u8, langspan::Error> {
        let ParsedOptions {
            args, mut options, ..
        } = parse_options(input)?;
        let vendor = match args.as_slice() {
            [hook, name] if hook == "hook" => langspan::install::Vendor::from_name(name),
            _ => None,
        }
        .ok_or(
            "usage: langspan hook claude-code|codex|cursor|antigravity [--event NAME] [--workspace ID --root DIR]",
        )?;
        // Only a host whose events do not name themselves takes `--event`,
        // and it must be one the install registers.
        let event = match (vendor.names_event(), options.remove("--event")) {
            (true, Some(event)) if vendor.hook_events().contains(&event.as_str()) => Some(event),
            (true, Some(event)) => {
                return Err(format!(
                    "--event {event:?} is not an event LangSpan handles for {}",
                    vendor.name()
                )
                .into());
            }
            (true, None) => {
                return Err(format!("{} hooks need --event NAME", vendor.name()).into());
            }
            (false, Some(_)) => {
                return Err(format!("--event is not used for {}", vendor.name()).into());
            }
            (false, None) => None,
        };
        // Without a workspace and root, the hook is the user-level form and
        // takes both from each event's working directory.
        let (workspace_id, root, user_level) =
            match (options.remove("--workspace"), options.remove("--root")) {
                (Some(workspace), Some(root)) => (workspace, root.into(), false),
                (None, None) => (String::new(), PathBuf::new(), true),
                _ => {
                    return Err(
                        "--workspace and --root go together; omit both for the user-level form"
                            .into(),
                    );
                }
            };
        let presence = presence(&mut options)?;
        // The between-step watcher (issue #214): Claude Code only.
        let watcher = match options.remove("--mode") {
            None => false,
            Some(mode)
                if mode == langspan::between_steps::MODE
                    && vendor == langspan::install::Vendor::ClaudeCode =>
            {
                true
            }
            Some(mode) => {
                return Err(format!(
                    "--mode {mode:?} is not a hook mode for {}; only claude-code takes --mode {}",
                    vendor.name(),
                    langspan::between_steps::MODE
                )
                .into());
            }
        };
        let config = langspan::claude_code::Config {
            state_dir: state_dir(&mut options)?,
            workspace_id,
            root,
            vendor,
            user_level,
            ttl_ms: number(&mut options, "--ttl-ms", 30_000)?,
            budget: std::time::Duration::from_millis(number(&mut options, "--budget-ms", 1500)?),
            share_prompt: switch(&mut options, "--prompt-summary")?,
            code_briefing: switch(&mut options, "--code-briefing")?,
            presence: presence.shown,
            presence_window_ms: presence.window_ms(),
            outline_reads: outline_reads(&mut options)?,
        };
        no_unused_options(&options)?;
        if !(1..=60_000).contains(&config.ttl_ms)
            || !(1..=4000).contains(&config.budget.as_millis())
        {
            return Err("hook limits: --ttl-ms 1..60000 and --budget-ms 1..4000".into());
        }
        let deadline = std::time::Instant::now() + config.budget;
        let input = langspan::hook::read_stdin_until(deadline);
        // A watcher starts no coordinator and writes nothing to stdout. On
        // exit 2 its stderr is the notice Claude Code shows the model, so
        // the log line is written only when it does not wake the session.
        if watcher {
            let watched = match &input {
                // A panic is a bug, but must not become the host's error.
                Ok(bytes) => std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    langspan::between_steps::watch(bytes, &config)
                }))
                .unwrap_or_else(|_| langspan::between_steps::Watched {
                    exit: 0,
                    stderr: String::new(),
                    detail: "the watcher stopped on an internal error (a panic); report it".into(),
                }),
                Err(error) => langspan::between_steps::Watched {
                    exit: 0,
                    stderr: String::new(),
                    detail: error.to_string(),
                },
            };
            if watched.exit == langspan::between_steps::WAKE {
                eprint!("{}", watched.stderr);
                return Ok(langspan::between_steps::WAKE);
            }
            eprintln!(
                "{}",
                serde_json::json!({"level":"info","event":"langspan_between_steps","detail":watched.detail})
            );
            return Ok(0);
        }
        // At most half of what is left, so the event itself keeps the rest.
        let now = std::time::Instant::now();
        start_coordinator(
            &config.state_dir,
            now + deadline.saturating_duration_since(now) / 2,
        );
        // Opt-in, for capturing what a real host sends so it can become a
        // fixture. Off unless the variable names an existing directory; a
        // failure to write is ignored and never changes the hook's answer.
        if let (Ok(bytes), Some(dir)) = (&input, std::env::var_os("LANGSPAN_CAPTURE_DIR")) {
            let dir = PathBuf::from(dir);
            if dir.is_dir() {
                use std::io::Write;
                let name = format!(
                    "{}-{}-{}.json",
                    args[1],
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or(0)
                );
                let _ = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(dir.join(name))
                    .and_then(|mut file| file.write_all(bytes));
            }
        }
        let reply = match input {
            Ok(bytes) => match vendor {
                langspan::install::Vendor::Codex => {
                    langspan::codex::run_until(bytes.as_slice(), &config, deadline)
                }
                langspan::install::Vendor::Cursor => {
                    langspan::cursor::run_until(bytes.as_slice(), &config, deadline)
                }
                // Answered before any of this, above.
                langspan::install::Vendor::Gemini => langspan::hook::Reply {
                    stdout: langspan::gemini_removed::ANSWER.into(),
                    ..langspan::hook::Reply::none("unsupported", "Gemini CLI support was removed")
                },
                langspan::install::Vendor::Antigravity => langspan::antigravity::run_until(
                    bytes.as_slice(),
                    &config,
                    event.as_deref().unwrap_or_default(),
                    deadline,
                ),
                langspan::install::Vendor::ClaudeCode => {
                    langspan::claude_code::run_until(bytes.as_slice(), &config, deadline)
                }
            },
            Err(error) => langspan::hook::Reply {
                stdout: fallback.unwrap_or_default().into(),
                ..langspan::hook::Reply::none(
                    if error.kind() == std::io::ErrorKind::TimedOut {
                        "timeout"
                    } else {
                        "unknown"
                    },
                    error.to_string(),
                )
            },
        };
        print!("{}", reply.stdout);
        eprintln!(
            "{}",
            serde_json::json!({"level":"info","event":"langspan_hook","coverage":reply.coverage,"detail":reply.detail})
        );
        Ok(0)
    })();
    match result {
        Ok(code) => ExitCode::from(code),
        // Exit 1 is a non-blocking error for Claude Code. Only the background
        // between-step watcher exits 2, to wake the model (above).
        Err(error) if background_watcher => {
            eprintln!(
                "{}",
                serde_json::json!({"level":"error","event":"langspan_between_steps","detail":error.to_string()})
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            if let Some(fallback) = fallback {
                print!("{fallback}");
            }
            eprintln!(
                "{}",
                serde_json::json!({"level":"error","event":"langspan_hook","coverage":"unknown","message":error.to_string()})
            );
            ExitCode::FAILURE
        }
    }
}
