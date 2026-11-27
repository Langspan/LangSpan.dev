//! Properties of the hand-written parsers that read text from hosts, agents
//! and the repository (issue #337): the secret redactor, the Codex
//! `apply_patch` parser and the shell tokenizer. Each never panics on
//! arbitrary text, nor on text assembled from the fragments its computed
//! slices depend on, and each gets back what a generated valid input was
//! built from.
//!
//! A failing case is saved under `proptest-regressions/parser_properties.txt`
//! and replayed first on every later run; commit that file with the fix.

use langspan::codex::{patch_edits, patch_paths};
use langspan::regions::{Edit, Hunk, HunkLine};
use langspan::shell::{git_steps, heredoc_write, looked_at, moves_checkout, write_targets};
use proptest::prelude::*;
use proptest::test_runner::FileFailurePersistence;
use std::collections::BTreeSet;

// The redactor's module is private to the crate; its source is compiled here
// as well, so the bounded form (`bounded`) can be tried at every limit. Its
// own unit tests come with it and run here a second time.
#[allow(dead_code)]
#[path = "../src/text/redact.rs"]
mod redact;

// The redactor's unit tests load this helper too; it holds one pure function.
#[allow(clippy::duplicate_mod)]
#[path = "support/secret_shape.rs"]
mod secret_shape;
use secret_shape::secret_shape;

fn config() -> ProptestConfig {
    ProptestConfig {
        cases: 256,
        failure_persistence: Some(Box::new(FileFailurePersistence::Direct(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/proptest-regressions/parser_properties.txt"
        )))),
        ..ProptestConfig::default()
    }
}

/// Text joined from `fragments` and arbitrary characters, so that the
/// markers each parser slices around meet multi-byte characters, quotes and
/// line ends far more often than arbitrary text would.
fn assembled(fragments: &'static [&'static str]) -> impl Strategy<Value = String> {
    let piece = prop_oneof![
        3 => prop::sample::select(fragments).prop_map(str::to_owned),
        1 => "\\PC{0,3}",
        1 => prop::sample::select(&["é", "日本", "🦀", "\u{0301}", "\r", "\n", "\t"][..])
            .prop_map(str::to_owned),
    ];
    prop::collection::vec(piece, 0..24).prop_map(|pieces| pieces.concat())
}

// ---------------------------------------------------------------------------
// Redactor
// ---------------------------------------------------------------------------

const REDACT_FRAGMENTS: &[&str] = &[
    "://",
    "@",
    ":",
    "/",
    "?",
    "#",
    "=",
    "==",
    "!=",
    "&",
    "\"",
    "'",
    " ",
    "--",
    "--token ",
    "--password",
    "password=",
    "TOKEN=",
    "key=",
    "-----BEGIN ",
    "-----END ",
    "-----",
    "sk-",
    "sk_live_",
    "ghp_",
    "github_pat_",
    "xoxb-",
    "AKIA",
    "AIza",
    "eyJ",
    ".",
    "abcdefghij",
    "ABCDEFGHIJ",
    "0123456789",
    "_-",
    "+",
    "/Users/owner",
];

/// A secret in one of the shapes the redactor documents, and the text it is
/// written in. The secret's value is what must not survive.
fn seeded_secret() -> impl Strategy<Value = (String, String)> {
    // Long enough for every prefix's least length (`AIza` takes 30).
    let alnum = "[A-Za-z0-9]{35,40}";
    prop_oneof![
        (
            prop::sample::select(&["sk-", "ghp_", "gho_", "xoxb-", "AIza"][..]),
            alnum
        )
            .prop_map(|(prefix, value)| {
                let secret = secret_shape(prefix, &value);
                (secret.clone(), secret)
            }),
        "[A-Z0-9]{16}".prop_map(|value| {
            let secret = secret_shape("AK", &format!("IA{value}"));
            (secret.clone(), secret)
        }),
        (
            "[A-Za-z0-9_-]{10,30}",
            "[A-Za-z0-9_-]{10,30}",
            "[A-Za-z0-9_-]{0,30}"
        )
            .prop_map(|(header, payload, signature)| {
                let jwt = secret_shape("eyJ", &format!("{header}.{payload}.{signature}"));
                (jwt.clone(), jwt)
            }),
        ("[A-Za-z0-9+/=\n]{8,80}").prop_map(|body| {
            let begin = secret_shape("-----BEGIN RSA PRIVATE", " KEY-----");
            let end = secret_shape("-----END RSA PRIVATE", " KEY-----");
            (format!("{begin}\n{body}\n{end}"), body)
        }),
        ("[a-z]{3,10}", "[A-Za-z0-9]{8,24}").prop_map(|(user, password)| {
            (
                format!("https://{user}:{password}@git.example.com/repo.git"),
                password,
            )
        }),
        (
            prop::sample::select(
                &[
                    "DB_PASSWORD",
                    "apiToken",
                    "STRIPE_KEY",
                    "client_secret",
                    "credentials"
                ][..]
            ),
            "[A-Za-z0-9]{8,24}",
            any::<bool>()
        )
            .prop_map(|(name, value, quoted)| {
                let text = if quoted {
                    format!("{name}=\"{value}\"")
                } else {
                    format!("{name}={value}")
                };
                (text, value)
            }),
        (
            prop::sample::select(&["--token", "--password", "--secret", "--api-key"][..]),
            "[A-Za-z0-9]{8,24}"
        )
            .prop_map(|(flag, value)| (format!("{flag} {value}"), value)),
    ]
}

/// Words around a seeded secret: they hold no character that would join
/// the secret's own run, so it keeps the shape it was given.
fn surrounding() -> impl Strategy<Value = String> {
    prop::collection::vec("[a-z.,;!]{0,8}|é|日本", 0..6).prop_map(|words| words.join(" "))
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn redaction_never_panics(text in assembled(REDACT_FRAGMENTS)) {
        let once = redact::redact(&text);
        // Redaction is documented as idempotent.
        prop_assert_eq!(redact::redact(&once), once.clone());
        redact::contains_secret(&text);
    }

    #[test]
    fn redaction_never_panics_on_arbitrary_text(text in "\\PC{0,200}") {
        redact::redact(&text);
    }

    #[test]
    fn every_seeded_secret_is_gone(
        before in surrounding(),
        (text, secret) in seeded_secret(),
        after in surrounding(),
        limit in 10_usize..400,
    ) {
        let text = format!("{before} {text} {after}");
        let redacted = redact::redact(&text);
        prop_assert!(!redacted.contains(&secret), "{secret:?} survived in {redacted:?}");
        prop_assert!(redact::contains_secret(&text));
        let bounded = redact::bounded(&text, limit, None);
        prop_assert!(!bounded.contains(&secret), "{secret:?} survived in {bounded:?}");
    }

    /// Every limit the mark fits in. Below that, see the test after this.
    #[test]
    fn bounded_redaction_fits_its_limit(
        text in assembled(REDACT_FRAGMENTS),
        limit in 10_usize..200,
    ) {
        let bounded = redact::bounded(&text, limit, Some("/Users/owner"));
        prop_assert!(bounded.len() <= limit, "{} bytes for a limit of {limit}: {bounded:?}", bounded.len());
        // A fixed point, so a retry redacted twice equals its record.
        prop_assert_eq!(redact::bounded(&bounded, limit, Some("/Users/owner")), bounded.clone());
    }
}

/// Every limit below the mark's length, where the mark itself is cut to fit
/// (#392). Every caller's limit is at least 256 bytes, so this guards the
/// function's contract rather than a path a caller takes today.
#[test]
fn bounded_redaction_fits_a_limit_shorter_than_the_mark() {
    proptest!(config(), |(text in assembled(REDACT_FRAGMENTS), limit in 0_usize..10)| {
        let bounded = redact::bounded(&text, limit, None);
        prop_assert!(bounded.len() <= limit, "{} bytes for a limit of {limit}: {bounded:?}", bounded.len());
        prop_assert_eq!(redact::bounded(&bounded, limit, None), bounded.clone());
    });
}

// ---------------------------------------------------------------------------
// Codex apply_patch parser
// ---------------------------------------------------------------------------

const PATCH_FRAGMENTS: &[&str] = &[
    "*** Begin Patch\n",
    "*** End Patch",
    "*** End Patch\n",
    "*** Add File: ",
    "*** Delete File: ",
    "*** Update File: ",
    "*** Move to: ",
    "*** End of File\n",
    "*** ",
    "@@\n",
    "@@ ",
    "+",
    "-",
    " ",
    "\n",
    "\r\n",
    "a.rs",
    "src/b.rs\n",
    "+line\n",
    "-line\n",
    " line\n",
];

#[derive(Debug, Clone)]
enum Section {
    Add(Vec<String>),
    Delete,
    Update {
        moved_to: Option<String>,
        /// Whether the first hunk is written without its `@@` line.
        bare_first: bool,
        hunks: Vec<Vec<(HunkLine, String)>>,
    },
}

fn hunk_line() -> impl Strategy<Value = (HunkLine, String)> {
    (
        prop::sample::select(&[HunkLine::Context, HunkLine::Removed, HunkLine::Added][..]),
        "\\PC{0,16}",
    )
}

fn patch_path() -> impl Strategy<Value = String> {
    "[a-z]{1,6}(/[a-z]{1,6}){0,2}\\.rs"
}

fn section() -> impl Strategy<Value = Section> {
    prop_oneof![
        prop::collection::vec("\\PC{0,16}", 0..4).prop_map(Section::Add),
        Just(Section::Delete),
        (
            prop::option::weighted(0.25, patch_path()),
            any::<bool>(),
            prop::collection::vec(prop::collection::vec(hunk_line(), 1..5), 1..4),
        )
            .prop_map(|(moved_to, bare_first, hunks)| Section::Update {
                moved_to,
                bare_first,
                hunks,
            }),
    ]
}

/// A valid patch of distinct paths, with what it was built from.
fn valid_patch() -> impl Strategy<Value = (String, Vec<(String, Section)>)> {
    prop::collection::vec((patch_path(), section()), 1..6)
        .prop_filter("distinct paths", |sections| {
            let mut seen = BTreeSet::new();
            sections.iter().all(|(path, section)| {
                seen.insert(path.clone())
                    && match section {
                        Section::Update {
                            moved_to: Some(to), ..
                        } => seen.insert(to.clone()),
                        _ => true,
                    }
            })
        })
        .prop_map(|sections| {
            let mut text = String::from("*** Begin Patch\n");
            for (path, section) in &sections {
                match section {
                    Section::Add(lines) => {
                        text += &format!("*** Add File: {path}\n");
                        for line in lines {
                            text += &format!("+{line}\n");
                        }
                    }
                    Section::Delete => text += &format!("*** Delete File: {path}\n"),
                    Section::Update {
                        moved_to,
                        bare_first,
                        hunks,
                    } => {
                        text += &format!("*** Update File: {path}\n");
                        if let Some(to) = moved_to {
                            text += &format!("*** Move to: {to}\n");
                        }
                        for (index, hunk) in hunks.iter().enumerate() {
                            if !(index == 0 && *bare_first) {
                                text += "@@\n";
                            }
                            for (kind, line) in hunk {
                                let mark = match kind {
                                    HunkLine::Context => ' ',
                                    HunkLine::Removed => '-',
                                    HunkLine::Added => '+',
                                };
                                text += &format!("{mark}{line}\n");
                            }
                        }
                    }
                }
            }
            text += "*** End Patch\n";
            (text, sections)
        })
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn the_patch_parser_never_panics(text in assembled(PATCH_FRAGMENTS)) {
        let _ = patch_paths(&text);
        patch_edits(&text);
        let _ = patch_paths(&format!("*** Begin Patch\n{text}\n*** End Patch"));
        patch_edits(&format!("*** Begin Patch\n{text}\n*** End Patch"));
    }

    #[test]
    fn the_patch_parser_never_panics_on_arbitrary_text(text in "\\PC{0,200}") {
        let _ = patch_paths(&text);
        patch_edits(&text);
    }

    #[test]
    fn a_valid_patch_parses_to_its_paths_and_hunks((text, sections) in valid_patch()) {
        let mut paths = BTreeSet::new();
        let mut edits = Vec::new();
        for (path, section) in &sections {
            paths.insert(path.clone());
            if let Section::Update { moved_to, hunks, .. } = section {
                match moved_to {
                    Some(to) => {
                        paths.insert(to.clone());
                    }
                    None => edits.push((
                        path.clone(),
                        Edit::Hunks(
                            hunks.iter().map(|lines| Hunk { lines: lines.clone() }).collect(),
                        ),
                    )),
                }
            }
        }
        prop_assert_eq!(patch_paths(&text), Ok(paths.into_iter().collect::<Vec<_>>()));
        prop_assert_eq!(patch_edits(&text), edits);
    }
}

// ---------------------------------------------------------------------------
// Shell tokenizer
// ---------------------------------------------------------------------------

const SHELL_FRAGMENTS: &[&str] = &[
    "'",
    "\"",
    " ",
    "\t",
    "\n",
    ";",
    "&",
    "&&",
    "|",
    "||",
    "|&",
    ">",
    ">>",
    "2>",
    "1>",
    ">&",
    "2>&1",
    ">|",
    "<<",
    "<<'EOF'",
    "<<\"EOF\"",
    "<<-EOF",
    "EOF",
    "\nEOF",
    "cat",
    "cat > a",
    "echo",
    "tee",
    "rm -f",
    "touch",
    "mv",
    "cp",
    "sed -i",
    "s/a/b/",
    "truncate -s 0",
    "git",
    "git checkout",
    "git switch -c",
    "commit",
    "--",
    "-",
    "./",
    "/",
    "..",
    "a",
    "b/c",
    "/dev/null",
    "=",
    "$",
    "\\",
    "*",
    "(",
];

#[derive(Debug, Clone)]
struct Written {
    /// The path as the command writes it: quoted, or after `./`.
    written: String,
    /// The path `write_targets` reports.
    path: String,
}

fn shell_path() -> impl Strategy<Value = Written> + Clone {
    (
        "[a-z]{1,6}(/[a-z]{1,6}){0,2}",
        prop::sample::select(&["", "'", "\""][..]),
        any::<bool>(),
    )
        .prop_map(|(path, quote, dotted)| {
            let dot = if dotted { "./" } else { "" };
            Written {
                written: format!("{quote}{dot}{path}{quote}"),
                path,
            }
        })
}

/// One simple command of the supported set, and the paths it writes.
fn simple_command() -> impl Strategy<Value = (String, Vec<String>)> {
    let one = shell_path();
    let some = prop::collection::vec(shell_path(), 1..4);
    fn names(paths: &[Written]) -> String {
        let written: Vec<&str> = paths.iter().map(|p| p.written.as_str()).collect();
        written.join(" ")
    }
    prop_oneof![
        (
            one.clone(),
            prop::sample::select(&[">", ">>", "1>", "2>"][..])
        )
            .prop_map(|(to, redirect)| (
                format!("echo hello {redirect} {}", to.written),
                vec![to.path]
            )),
        some.clone().prop_map(move |paths| (
            format!("touch {}", names(&paths)),
            paths.into_iter().map(|p| p.path).collect()
        )),
        some.clone().prop_map(move |paths| (
            format!("rm -f {}", names(&paths)),
            paths.into_iter().map(|p| p.path).collect()
        )),
        some.prop_map(move |paths| (
            format!("tee -a {}", names(&paths)),
            paths.into_iter().map(|p| p.path).collect()
        )),
        (one.clone(), one.clone())
            .prop_map(|(from, to)| (format!("cp {} {}", from.written, to.written), vec![to.path])),
        (one.clone(), one.clone()).prop_map(|(from, to)| (
            format!("mv -f {} {}", from.written, to.written),
            vec![from.path, to.path]
        )),
        one.clone()
            .prop_map(|to| (format!("truncate -s 0 {}", to.written), vec![to.path])),
        one.prop_map(|to| (format!("sed -i 's/a/b/g' {}", to.written), vec![to.path])),
        Just(("echo done".to_owned(), Vec::new())),
        Just(("cat /dev/null > /dev/null 2>&1".to_owned(), Vec::new())),
    ]
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn the_shell_parser_never_panics(text in assembled(SHELL_FRAGMENTS)) {
        let _ = write_targets(&text);
        looked_at(&text);
        heredoc_write(&text);
        let _ = git_steps(&text);
        moves_checkout(&text);
    }

    #[test]
    fn the_shell_parser_never_panics_on_arbitrary_text(text in "\\PC{0,200}") {
        let _ = write_targets(&text);
        looked_at(&text);
        heredoc_write(&text);
        let _ = git_steps(&text);
        moves_checkout(&text);
    }

    #[test]
    fn a_generated_commands_write_targets_are_found(
        parts in prop::collection::vec(
            (simple_command(), prop::sample::select(&[" ; ", " && ", " | ", ";", "&&", "|"][..])),
            1..5,
        ),
    ) {
        let mut command = String::new();
        let mut targets = BTreeSet::new();
        for (index, ((part, written), separator)) in parts.iter().enumerate() {
            if index > 0 {
                command += separator;
            }
            command += part;
            targets.extend(written.iter().cloned());
        }
        prop_assert_eq!(
            write_targets(&command),
            Ok(targets.into_iter().collect::<Vec<_>>()),
            "for {}", command
        );
    }

    #[test]
    fn a_quoted_heredoc_writes_its_body_to_its_target(
        to in "[a-z]{1,6}(/[a-z]{1,6}){0,2}",
        quote in prop::sample::select(&["'", "\""][..]),
        before in any::<bool>(),
        body in prop::collection::vec("[^\n]{0,20}", 0..5),
    ) {
        prop_assume!(body.iter().all(|line| line != "EOF"));
        let head = if before {
            format!("cat > {to} <<{quote}EOF{quote}")
        } else {
            format!("cat <<{quote}EOF{quote} > {to}")
        };
        let lines: String = body.iter().map(|line| format!("{line}\n")).collect();
        let command = format!("{head}\n{lines}EOF\n");
        prop_assert_eq!(heredoc_write(&command), Some((to, lines)));
    }
}
