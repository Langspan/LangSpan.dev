//! The `langspan` command's flags and option parsing (issues #297, #300).
//! A module of the binary, not the library: only `main.rs` reads it.

use std::{collections::BTreeMap, path::PathBuf};

/// The flags one command takes, read once its words are known: a new flag
/// is added to its command's entry in [`COMMANDS`].
pub(crate) struct Flags {
    /// Whether the command's words name this entry: all of them, and
    /// those before a `--`.
    form: fn(&[&str], &[&str]) -> bool,
    /// Flags without a value.
    switches: &'static [&'static str],
    /// Flags that take a value.
    values: &'static [&'static str],
    /// Whether `run` lets `--apply` through to the command.
    pub(crate) apply: bool,
    pub(crate) symbol: Repeat,
    pub(crate) tag: Repeat,
}

/// How often a command takes `--symbol` or `--tag`.
#[derive(Clone, Copy)]
pub(crate) enum Repeat {
    No,
    /// At most once; the text says why.
    Once(&'static str),
    Any,
}

impl Repeat {
    pub(crate) fn check(self, name: &str, given: usize) -> Result<(), langspan::Error> {
        match self {
            Repeat::No if given > 0 => {
                Err(format!("option {name} is not valid for this command").into())
            }
            Repeat::Once(why) if given > 1 => Err(why.into()),
            _ => Ok(()),
        }
    }
}

impl Flags {
    /// A command with no flags of its own.
    const NONE: Flags = Flags {
        form: |_, _| false,
        switches: &[],
        values: &[],
        apply: false,
        symbol: Repeat::No,
        tag: Repeat::No,
    };
}

/// Flags every command reads: `--state-dir`, and the `--request-id` of a
/// request to the coordinator. `--json`, `--apply`, `--symbol` and
/// `--tag` are the parser's own; `-m` is `--message`.
const EVERY_COMMAND: &[&str] = &["--state-dir", "--request-id"];

/// The installers' options for the hosts they set up.
const INSTALL_VALUES: &[&str] = &[
    "--home",
    "--hosts",
    "--prompt-summary",
    "--presence",
    "--presence-minutes",
    "--outline-reads",
    "--statusline",
    "--mcp-tools",
];

/// What each command takes. A form that decides `--apply`, `--symbol` or
/// `--tag` matches the words exactly as the command's dispatch in `run`
/// does; the first match wins.
const COMMANDS: &[Flags] = &[
    Flags {
        form: |words, _| words.first() == Some(&"hook"),
        values: &[
            "--event",
            "--workspace",
            "--root",
            "--mode",
            "--ttl-ms",
            "--budget-ms",
            "--prompt-summary",
            "--code-briefing",
            "--presence",
            "--presence-minutes",
            "--outline-reads",
        ],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words == ["install"],
        switches: &["--skill"],
        values: INSTALL_VALUES,
        apply: true,
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words == ["uninstall"],
        switches: &["--skill", "--purge"],
        values: INSTALL_VALUES,
        apply: true,
        ..Flags::NONE
    },
    Flags {
        form: |words, _| {
            matches!(words, ["hooks", "install" | "remove", host]
                if langspan::install::Vendor::from_name(host).is_some())
        },
        switches: &["--skill"],
        values: &[
            "--root",
            "--settings",
            "--workspace",
            "--prompt-summary",
            "--code-briefing",
            "--presence",
            "--presence-minutes",
            "--outline-reads",
        ],
        apply: true,
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words == ["setup"],
        switches: &["--yes", "--skill", "--verbose"],
        values: INSTALL_VALUES,
        ..Flags::NONE
    },
    Flags {
        form: |words, before| words.first() == Some(&"guidance") && words.len() == before.len(),
        switches: &["--create"],
        values: &["--format", "--root", "--file"],
        apply: true,
        ..Flags::NONE
    },
    Flags {
        form: |_, before| before == ["doctor"],
        values: &["--root", "--home"],
        ..Flags::NONE
    },
    Flags {
        form: |_, before| before == ["hosts"],
        values: &["--home"],
        ..Flags::NONE
    },
    Flags {
        form: |_, before| matches!(before, ["check", "auto", _]),
        switches: &["--off"],
        values: &["--name", "--timeout-s"],
        ..Flags::NONE
    },
    Flags {
        form: |_, before| matches!(before, ["check", "flaky", _]),
        values: &["--name", "--forget"],
        ..Flags::NONE
    },
    Flags {
        form: |_, before| matches!(before, ["check", "run" | "status", _]),
        values: &["--name", "--timeout-s"],
        ..Flags::NONE
    },
    Flags {
        form: |_, before| matches!(before, ["integrate", "check", _, ..]),
        switches: &["--rebase", "--adapt-renames", "--symbols"],
        values: &["--name", "--onto", "--remote", "--timeout-s", "--session"],
        ..Flags::NONE
    },
    Flags {
        form: |_, before| matches!(before, ["land", _, ..]),
        switches: &["--check-again", "--rebase", "--adapt-renames"],
        values: &[
            "--name",
            "--onto",
            "--remote",
            "--timeout-s",
            "--session",
            "--wait-s",
        ],
        apply: true,
        ..Flags::NONE
    },
    Flags {
        form: |_, before| matches!(before, ["land-queue" | "land-cancel", _]),
        values: &["--onto"],
        ..Flags::NONE
    },
    Flags {
        form: |_, before| before == ["statusline"],
        values: &["--root", "--deadline-ms"],
        ..Flags::NONE
    },
    Flags {
        form: |_, before| before == ["count"],
        values: &["--month"],
        ..Flags::NONE
    },
    Flags {
        form: |_, before| matches!(before, ["notify", _]),
        values: &["--sound"],
        ..Flags::NONE
    },
    Flags {
        form: |_, before| before == ["commit"],
        switches: &["--mine"],
        values: &["--message", "--root", "--session"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words.first() == Some(&"adopt"),
        values: &["--root", "--session"],
        ..Flags::NONE
    },
    Flags {
        form: |_, before| before == ["status"],
        switches: &["--mine", "--all"],
        values: &["--root", "--session", "--after"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words == ["dashboard"],
        switches: &["--open", "--allow-network", "--demo"],
        values: &["--listen"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words == ["mcp"],
        values: &["--tools", "--host"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words == ["workspace", "list"],
        values: &["--after"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words.starts_with(&["workspace", "move"]),
        switches: &["--switch"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| matches!(words, ["workspace", "remove", _]),
        apply: true,
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words.starts_with(&["intent", "set"]),
        values: &["--summary"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words.first() == Some(&"admit"),
        values: &["--ttl-ms"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words.first() == Some(&"brief"),
        switches: &["--recent"],
        values: &["--max-peers"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words.starts_with(&["remote", "enable"]),
        switches: &["--publish-as-branch"],
        values: &[
            "--root",
            "--remote",
            "--name",
            "--share-task",
            "--share-messages",
        ],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words.starts_with(&["remote", "sent"]),
        values: &["--limit"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words.starts_with(&["graph", "symbol"]),
        switches: &["--detail", "--rank", "--renamed"],
        values: &["--limit", "--session"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| {
            words.starts_with(&["graph", "references"])
                || words.starts_with(&["graph", "blind-spots"])
        },
        values: &["--limit"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words.starts_with(&["graph", "impact"]),
        values: &["--limit", "--depth"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words.starts_with(&["graph", "path"]),
        values: &["--max-hops"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words.starts_with(&["graph", "map"]),
        values: &["--depth"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words.starts_with(&["graph", "export"]),
        values: &["--out"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| matches!(words, ["graph", "import", _]),
        values: &["--in"],
        apply: true,
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words.starts_with(&["graph", "history"]),
        values: &["--path", "--limit", "--before"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words.first() == Some(&"check-write"),
        symbol: Repeat::Any,
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words.first() == Some(&"recap"),
        switches: &["--since-away"],
        values: &["--since-seq"],
        tag: Repeat::Once("recap filters by one tag: give --tag once"),
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words.first() == Some(&"remember"),
        values: &["--session"],
        symbol: Repeat::Once("a note is anchored to one definition: give --symbol once"),
        tag: Repeat::Any,
        ..Flags::NONE
    },
    Flags {
        form: |words, _| {
            matches!(
                words.first(),
                Some(&"trail" | &"events" | &"changes" | &"recall")
            )
        },
        values: &["--after", "--limit", "--path"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| matches!(words.first(), Some(&"today" | &"report")),
        values: &["--since"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words.starts_with(&["policy", "set"]),
        values: &["--wait-ms"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words.first() == Some(&"message"),
        values: &["--from", "--to", "--remote"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| {
            words.starts_with(&["messages", "status"])
                || words.starts_with(&["messages", "retract"])
        },
        values: &["--from"],
        ..Flags::NONE
    },
    Flags {
        form: |_, before| before == ["compact"],
        values: &["--keep-days"],
        apply: true,
        ..Flags::NONE
    },
];

/// The flags of the command `words` name: all the words, and `before`,
/// those before a `--`.
pub(crate) fn flags_of(words: &[&str], before: &[&str]) -> &'static Flags {
    COMMANDS
        .iter()
        .find(|flags| (flags.form)(words, before))
        .unwrap_or(&Flags::NONE)
}

/// Whether some command declares `name` as a flag that takes a value
/// (`Some(true)`) or none (`Some(false)`). The parser reads this before
/// the command's words are known.
fn takes_value(name: &str) -> Option<bool> {
    let name = if name == "-m" { "--message" } else { name };
    if EVERY_COMMAND.contains(&name) || COMMANDS.iter().any(|flags| flags.values.contains(&name)) {
        Some(true)
    } else if COMMANDS.iter().any(|flags| flags.switches.contains(&name)) {
        Some(false)
    } else {
        None
    }
}

pub(crate) struct ParsedOptions {
    pub(crate) args: Vec<String>,
    /// Everything after a literal `--`: a check's command line.
    pub(crate) command: Vec<String>,
    pub(crate) options: BTreeMap<String, String>,
    /// Each `--symbol NAME`, in order: one of the two options that may
    /// repeat.
    pub(crate) symbols: Vec<String>,
    /// Each `--tag TAG`, in order (`remember`; `recap` takes one).
    pub(crate) tags: Vec<String>,
    pub(crate) json: bool,
    pub(crate) apply: bool,
}

pub(crate) fn parse_options(input: Vec<String>) -> Result<ParsedOptions, langspan::Error> {
    let mut positionals = Vec::new();
    let mut command = Vec::new();
    let mut options = BTreeMap::new();
    let mut symbols = Vec::new();
    let mut tags = Vec::new();
    let mut json = false;
    let mut apply = false;
    let mut raw = false;
    let mut input = input.into_iter();
    while let Some(arg) = input.next() {
        if raw {
            command.push(arg.clone());
            positionals.push(arg);
            continue;
        }
        match arg.as_str() {
            "--" => raw = true,
            "--json" => json = true,
            "--symbol" => symbols.push(input.next().ok_or("--symbol needs a value")?),
            "--tag" => tags.push(input.next().ok_or("--tag needs a value")?),
            "--apply" => apply = true,
            // A flag some command declares. Its words are not known yet, so
            // these are kept with the options, and only the commands that
            // read them accept them.
            name if takes_value(name) == Some(false) => {
                if options.insert(arg.clone(), String::new()).is_some() {
                    return Err(format!("duplicate option {arg}").into());
                }
            }
            name if takes_value(name) == Some(true) => {
                let arg = if arg == "-m" {
                    "--message".to_owned()
                } else {
                    arg
                };
                let value = input.next().ok_or_else(|| format!("{arg} needs a value"))?;
                if options.insert(arg.clone(), value).is_some() {
                    return Err(format!("duplicate option {arg}").into());
                }
            }
            _ if arg.starts_with('-') => {
                return Err(format!(
                    "unknown option {arg}; use -- before a filename beginning with '-' "
                )
                .into());
            }
            _ => positionals.push(arg),
        }
    }
    Ok(ParsedOptions {
        args: positionals,
        command,
        options,
        symbols,
        tags,
        json,
        apply,
    })
}
/// A required file option as an absolute path, so the daemon, which runs
/// in another directory, opens the file the owner named.
pub(crate) fn absolute_option(
    options: &mut BTreeMap<String, String>,
    name: &str,
) -> Result<String, langspan::Error> {
    let value = options
        .remove(name)
        .ok_or_else(|| format!("{name} FILE is required"))?;
    let path = std::path::absolute(&value).map_err(|e| format!("cannot resolve {name}: {e}"))?;
    path.into_os_string()
        .into_string()
        .map_err(|_| format!("{name} must be a UTF-8 path").into())
}

pub(crate) fn number(
    options: &mut BTreeMap<String, String>,
    name: &str,
    default: u64,
) -> Result<u64, langspan::Error> {
    match options.remove(name) {
        None => Ok(default),
        Some(value) => value
            .parse()
            .map_err(|_| format!("{name} must be an integer").into()),
    }
}

pub(crate) fn state_dir(
    options: &mut BTreeMap<String, String>,
) -> Result<PathBuf, langspan::Error> {
    Ok(options
        .remove("--state-dir")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("LANGSPAN_STATE_DIR").map(PathBuf::from))
        .or_else(|| std::env::var_os("XDG_STATE_HOME").map(|p| PathBuf::from(p).join("langspan")))
        .or_else(|| {
            std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/state/langspan"))
        })
        .ok_or("choose --state-dir; no state directory environment is available")?)
}

#[cfg(test)]
mod option_tests {
    fn parsed(input: &[&str]) -> std::collections::BTreeMap<String, String> {
        super::parse_options(input.iter().map(|s| s.to_string()).collect())
            .unwrap()
            .options
    }

    #[test]
    fn flags_and_values_from_setup_brief_retention_and_guidance_all_parse() {
        let options = parsed(&[
            "x",
            "--yes",
            "--recent",
            "--create",
            "--event",
            "e",
            "--before",
            "b",
            "--keep-days",
            "7",
            "--month",
            "2026-11",
            "--format",
            "cursor",
            "--file",
            "AGENTS.md",
            "--wait-ms",
            "300",
        ]);
        for flag in ["--yes", "--recent", "--create"] {
            assert_eq!(options[flag], "", "{flag}");
        }
        for (key, value) in [
            ("--event", "e"),
            ("--before", "b"),
            ("--keep-days", "7"),
            ("--month", "2026-11"),
            ("--format", "cursor"),
            ("--file", "AGENTS.md"),
            ("--wait-ms", "300"),
        ] {
            assert_eq!(options[key], value, "{key}");
        }
    }
}
