//! The commands answered on this machine without a daemon request:
//! `commit --mine`, `status --mine`, `adopt`, `statusline`, `count`,
//! `events verify`, `notify` and `demo`.

use crate::check::unique;
use crate::flags::number;
use langspan::cli::options::no_unused_options;
use std::{collections::BTreeMap, path::PathBuf};

/// `commit --mine`: commit only this session's own changes (ADR 0026).
pub(crate) fn commit_mine(
    state_dir: &std::path::Path,
    options: &mut BTreeMap<String, String>,
    json: bool,
) -> Result<u8, langspan::Error> {
    if options.remove("--mine").is_none() {
        return Err(
            "`commit` needs --mine: langspan commit --mine -m MESSAGE [--session ID] [--root DIR]"
                .into(),
        );
    }
    let message = options
        .remove("--message")
        .ok_or("a commit message is required: -m MESSAGE")?;
    let root = match options.remove("--root") {
        Some(root) => PathBuf::from(root),
        None => std::env::current_dir()?,
    };
    let session = options.remove("--session");
    no_unused_options(options)?;
    let session = match session {
        Some(session) => session,
        None => langspan::commit::running_session(state_dir, &root)?,
    };
    let report = langspan::commit::commit_mine(state_dir, &root, &session, &message)?;
    if json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!("{}", report.text());
    }
    Ok(report.exit_code())
}

/// `statusline`: sessions here and refusals today, for the person (H2).
/// Exits 0 with the unavailable line when the coordinator cannot answer.
pub(crate) fn status_line(
    state_dir: &std::path::Path,
    options: &mut BTreeMap<String, String>,
    json: bool,
) -> Result<u8, langspan::Error> {
    use langspan::status_line::{DEADLINE_RANGE_MS, DEFAULT_DEADLINE_MS, run};
    let started = std::time::Instant::now();
    let root = options.remove("--root").map(PathBuf::from);
    let deadline_ms = number(options, "--deadline-ms", DEFAULT_DEADLINE_MS)?;
    no_unused_options(options)?;
    if !DEADLINE_RANGE_MS.contains(&deadline_ms) {
        return Err(format!(
            "--deadline-ms must be between {} and {}",
            DEADLINE_RANGE_MS.start(),
            DEADLINE_RANGE_MS.end()
        )
        .into());
    }
    let answer = run(
        state_dir,
        root,
        std::time::Duration::from_millis(deadline_ms),
        started,
    );
    if json {
        println!("{}", answer.json());
    } else {
        println!("{}", answer.text());
    }
    Ok(0)
}

/// `count`: refusals in one local month, read from the event log (H5).
/// The coordinator being down is an error (exit 1); no earlier figure is
/// shown instead.
pub(crate) fn refusal_count(
    state_dir: &std::path::Path,
    options: &mut BTreeMap<String, String>,
    json: bool,
) -> Result<u8, langspan::Error> {
    use langspan::count;
    let month = count::month(
        options.remove("--month").as_deref(),
        langspan::store::wall_now_ms(),
    )?;
    no_unused_options(options)?;
    let answer = count::ask(state_dir, month, count::day_starts(month)?)?;
    if json {
        println!("{}", serde_json::to_string(&answer)?);
    } else {
        println!("{}", answer.text(&month.label()));
    }
    Ok(0)
}

/// `events verify`: recompute the event log's hash chain, read-only, without
/// the coordinator (J9). Exits 1 when the chain is broken.
pub(crate) fn events_verify(
    state_dir: &std::path::Path,
    options: &BTreeMap<String, String>,
    json: bool,
) -> Result<u8, langspan::Error> {
    no_unused_options(options)?;
    let verdict = langspan::store::chain::verify(&state_dir.join("state.sqlite3"))
        .map_err(|error| format!("could not verify the event log: {error}"))?;
    if json {
        println!("{}", serde_json::to_string(&verdict)?);
    } else {
        println!("{}", verdict.text());
    }
    Ok(if verdict.ok { 0 } else { 1 })
}

/// `notify on|off|status [--sound on|off]`: desktop notices (H3). Writes
/// only `notify` and `notify_sound` in `config.json`; the daemon reads them
/// at each notice.
pub(crate) fn notify(
    state_dir: &std::path::Path,
    action: &str,
    options: &mut BTreeMap<String, String>,
    json: bool,
) -> Result<u8, langspan::Error> {
    use langspan::notify;
    let sound = match options.remove("--sound").as_deref() {
        None => None,
        Some("on") => Some(true),
        Some("off") => Some(false),
        Some(other) => return Err(format!("--sound takes on or off, not {other:?}").into()),
    };
    no_unused_options(options)?;
    let (on, sound) = match (action, sound) {
        ("on" | "off", sound) => notify::configure(state_dir, action == "on", sound)?,
        ("status", None) => notify::configured(state_dir)?,
        ("status", Some(_)) => {
            return Err("--sound is valid with `notify on` and `notify off`".into());
        }
        _ => return Err(format!("notify takes on, off or status, not {action:?}").into()),
    };
    let found = notify::find(std::env::var_os("PATH").as_deref());
    let program = notify::Program::here().name();
    if json {
        let notifier = found.as_ref().map(|(program, path)| {
            serde_json::json!({"name": program.name(), "path": path.to_string_lossy()})
        });
        println!(
            "{}",
            serde_json::json!({"notify": on, "notify_sound": sound, "notifier": notifier})
        );
        return Ok(0);
    }
    let word = |on: bool| if on { "on" } else { "off" };
    println!("notices: {}", word(on));
    println!("sound: {}", word(sound));
    match &found {
        Some((program, path)) => println!("notifier: {} ({})", program.name(), path.display()),
        None => println!("notifier: none found (no {program} on PATH); no notice can be shown"),
    }
    Ok(0)
}

/// `adopt SESSION`: take over an ended session's uncommitted lines (issue
/// #161). The adopting session is `--session`, or the one running this
/// command through its shell tool.
pub(crate) fn adopt(
    state_dir: &std::path::Path,
    from: &str,
    options: &mut BTreeMap<String, String>,
    json: bool,
) -> Result<u8, langspan::Error> {
    let root = match options.remove("--root") {
        Some(root) => PathBuf::from(root),
        None => std::env::current_dir()?,
    };
    let session = options.remove("--session");
    let request_id = options.remove("--request-id");
    no_unused_options(options)?;
    let session = match session {
        Some(session) => session,
        None => langspan::commit::running_session(state_dir, &root)?,
    };
    let request_id = request_id.unwrap_or_else(|| unique("adopt", &[&session, from]));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let report = langspan::adopt::adopt(state_dir, &request_id, &session, from, deadline)?;
    if json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!("{}", report.text());
    }
    Ok(0)
}

/// `status --mine`: this session's uncommitted lines, read-only (issue #40).
pub(crate) fn status_mine(
    state_dir: &std::path::Path,
    options: &mut BTreeMap<String, String>,
    json: bool,
) -> Result<u8, langspan::Error> {
    let root = match options.remove("--root") {
        Some(root) => PathBuf::from(root),
        None => std::env::current_dir()?,
    };
    let session = options.remove("--session");
    no_unused_options(options)?;
    let session = match session {
        Some(session) => session,
        None => langspan::commit::running_session(state_dir, &root)?,
    };
    let status = langspan::commit::status_mine(state_dir, &root, &session)?;
    if json {
        println!("{}", serde_json::to_string(&status)?);
    } else {
        println!("{}", status.text());
    }
    Ok(0)
}

/// `demo`: a real refusal in a temporary workspace, with no agent.
pub(crate) fn demo(options: &BTreeMap<String, String>, json: bool) -> Result<u8, langspan::Error> {
    if json {
        return Err("--json is not valid for `demo`".into());
    }
    if options.contains_key("--state-dir") {
        return Err(
            "`demo` uses a temporary state directory of its own; leave out --state-dir".into(),
        );
    }
    no_unused_options(options)?;
    let executable = std::env::current_exe()?;
    let mut stdout = std::io::stdout().lock();
    langspan::demo::run(&executable, &mut stdout)?;
    Ok(0)
}
