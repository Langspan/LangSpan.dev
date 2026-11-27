//! `langspan dashboard`, `daemon` and `mcp`: the long-running servers.

use langspan::cli::options::no_unused_options;
use langspan::cli::setup::start_coordinator;
use std::{collections::BTreeMap, path::PathBuf};

/// `dashboard`: the live, read-only page of every agent, on loopback.
pub(crate) async fn dashboard(
    state_dir: PathBuf,
    mut options: BTreeMap<String, String>,
    json: bool,
) -> Result<u8, langspan::Error> {
    let listen = match options.remove("--listen") {
        None => langspan::dashboard::default_listen(),
        Some(value) => value.parse().map_err(|_| {
            format!(
                "--listen must be an IP address and port, such as 127.0.0.1:{}",
                langspan::dashboard::DEFAULT_PORT
            )
        })?,
    };
    let open = options.remove("--open").is_some();
    let allow_network = options.remove("--allow-network").is_some();
    let demo = options.remove("--demo").is_some();
    no_unused_options(&options)?;
    let options = langspan::dashboard::Options {
        listen,
        open,
        allow_network,
        demo,
    };
    if json {
        return Err("--json is not valid for `dashboard`; its JSON is served at /api/state".into());
    }
    tokio::task::spawn_blocking(move || langspan::dashboard::serve(state_dir, options)).await??;
    Ok(0)
}

/// `daemon`: the local coordinator, in the foreground.
pub(crate) async fn daemon(
    state_dir: PathBuf,
    options: &BTreeMap<String, String>,
) -> Result<u8, langspan::Error> {
    no_unused_options(options)?;
    langspan::daemon::run(state_dir).await?;
    Ok(0)
}

/// `mcp`: MCP over stdin/stdout, with the coordinator started first.
pub(crate) async fn mcp(
    state_dir: PathBuf,
    mut options: BTreeMap<String, String>,
) -> Result<u8, langspan::Error> {
    let list = langspan::mcp::ToolList::parse(
        options.remove("--tools").as_deref(),
        options.remove("--host").as_deref(),
    )?;
    no_unused_options(&options)?;
    let dir = state_dir.clone();
    tokio::task::spawn_blocking(move || {
        start_coordinator(
            &dir,
            std::time::Instant::now() + std::time::Duration::from_secs(2),
        )
    })
    .await?;
    langspan::mcp::serve(state_dir, list).await?;
    Ok(0)
}
