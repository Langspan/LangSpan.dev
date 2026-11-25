//! Redaction of likely secrets from owner and agent text before it is
//! recorded. Every client applies `redact_command` before a request reaches
//! the daemon, and the daemon applies it again on receipt, so a secret typed
//! into a prompt, note, message, handoff, intent summary or check command
//! line never reaches the event log, a briefing or a publication. A field the
//! daemon bounds (a summary or a check's command line) stays within its
//! bound after redaction.

//!
//! What is replaced with `[redacted]`:
//! - provider key shapes: `sk-`, `sk_live_`, `sk_test_`, `ghp_` (and GitHub's
//!   other `gh?_` kinds), `github_pat_`, `xox[abpr]-`, AWS `AKIA`/`ASIA` with
//!   exactly 16 more characters, Google `AIza`;
//! - JWTs: three base64url segments, the first starting `eyJ`;
//! - PEM blocks, from `-----BEGIN ` to the end of its `-----END ...-----`
//!   line, or to the end of the text when it is not terminated;
//! - the `user:pass` of a URL;
//! - the value of an assignment or query parameter whose name suggests a
//!   secret (a word ending in `password`, `passwd`, `secret`, `token` or
//!   `apikey`, the word `credential(s)`, or a last word `key`, so
//!   `STRIPE_KEY=`, `clientSecret=`, `?token=`);
//! - the value after `--password`, `--passwd`, `--token`, `--secret` and
//!   `--api-key`;
//! - long high-entropy tokens. The rule is deliberately conservative: a run
//!   of at least 32 characters from `[A-Za-z0-9_+-]` (padding `=` included)
//!   with an upper-case letter, a lower-case letter and a digit, no more than
//!   five lower-case letters in a row, and at least 4.0 bits of Shannon
//!   entropy per character. Words, identifiers and paths have longer
//!   lower-case runs (and `/` and `.` end a run); commit IDs, digests and
//!   UUIDs have no upper case. A secret in only hexadecimal or lower case is
//!   therefore not caught by this rule.
//!
//! The owner's home directory at the start of a path becomes `~`.
//!
//! Redaction is a safety net, not a guarantee: a secret in a shape not listed
//! here passes through. It is idempotent, so applying it twice is harmless.

use langspan_core::Command;

const MARK: &str = "[redacted]";

/// `text` with likely secrets replaced and the owner's home shortened.
pub fn redact(text: &str) -> String {
    let home = std::env::var("HOME").ok();
    redact_with_home(text, home.as_deref())
}

/// Whether `text` holds something redaction would replace, apart from the
/// owner's home directory.
pub fn contains_secret(text: &str) -> bool {
    redact_with_home(text, None) != text
}

/// Redact every field of `command` that carries owner or agent text.
pub fn redact_command(command: &mut Command) {
    match command {
        Command::SessionPrompt { summary, .. } => {
            *summary = redact_bounded(summary, langspan_core::MAX_PROMPT_SUMMARY_BYTES);
        }
        Command::IntentSet { summary, .. } => {
            *summary = redact_bounded(summary, langspan_core::MAX_INTENT_SUMMARY_BYTES);
        }
        Command::Remember { text, .. }
        | Command::SendMessage { text, .. }
        | Command::Handoff { text, .. } => *text = redact(text),
        Command::RecordCheck { command, .. } => {
            *command = redact_bounded(command, langspan_core::MAX_CHECK_COMMAND_BYTES);
        }
        _ => {}
    }
}

/// Redact the command line of a check record that may have been stored
/// before check commands were redacted, so it is never shown as typed.
pub fn redact_check(check: &mut langspan_core::CheckRecord) {
    check.command = redact_bounded(&check.command, langspan_core::MAX_CHECK_COMMAND_BYTES);
}

fn redact_bounded(text: &str, limit: usize) -> String {
    let home = std::env::var("HOME").ok();
    bounded(text, limit, home.as_deref())
}

/// `text` redacted and kept within `limit` bytes, for a field the daemon
/// validates against that limit: a check's command line, a prompt summary
/// or an intent summary. Redaction can lengthen text (`--token x` becomes
/// `--token [redacted]`), and text within its limit must not be refused, nor
/// a record an earlier build accepted become invalid when it is read back,
/// so text that no longer fits keeps its longest prefix that does, cut at a
/// character boundary and followed by the mark. The cut is taken from the
/// redacted text, so it shows nothing redaction hid. The result is a fixed
/// point: applying this again changes nothing, so a retry redacted by its
/// client and again by the daemon still equals its recorded request.
fn bounded(text: &str, limit: usize, home: Option<&str>) -> String {
    let fit = |text: String| {
        if text.len() <= limit {
            return text;
        }
        let mut cut = limit - MARK.len();
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("{}{MARK}", &text[..cut])
    };
    let mut text = text.to_owned();
    // Each pass only replaces text with the mark or cuts it, so a fixed
    // point comes within a pass or two; the bound keeps that a guarantee.
    for _ in 0..4 {
        let next = fit(redact_with_home(&text, home));
        if next == text {
            return text;
        }
        text = next;
    }
    MARK.to_owned()
}

fn redact_with_home(text: &str, home: Option<&str>) -> String {
    let mut spans = Vec::new();
    pem_blocks(text, &mut spans);
    url_userinfo(text, &mut spans);
    secret_assignments(text, &mut spans);
    secret_arguments(text, &mut spans);
    provider_keys(text, &mut spans);
    jwts(text, &mut spans);
    high_entropy(text, &mut spans);
    spans.sort_unstable();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    let mut current: Option<(usize, usize)> = None;
    for (start, end) in spans {
        match current {
            Some((_, current_end)) if start <= current_end => {
                current = current.map(|(s, e)| (s, e.max(end)));
            }
            _ => {
                if let Some((s, e)) = current.take() {
                    out.push_str(&text[at..s]);
                    out.push_str(MARK);
                    at = e;
                }
                current = Some((start, end));
            }
        }
    }
    if let Some((s, e)) = current {
        out.push_str(&text[at..s]);
        out.push_str(MARK);
        at = e;
    }
    out.push_str(&text[at..]);
    tilde(out, home)
}

type Spans = Vec<(usize, usize)>;
/// A key prefix, the characters allowed after it, the least number of them,
/// and whether that number is exact.
type Shape<'a> = (&'a str, &'a dyn Fn(u8) -> bool, usize, bool);

fn base64url(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

fn run_len(bytes: &[u8], from: usize, allowed: impl Fn(u8) -> bool) -> usize {
    bytes[from.min(bytes.len())..]
        .iter()
        .take_while(|&&byte| allowed(byte))
        .count()
}

fn occurrences<'a>(text: &'a str, needle: &'a str) -> impl Iterator<Item = usize> + 'a {
    text.match_indices(needle).map(|(index, _)| index)
}

fn pem_blocks(text: &str, spans: &mut Spans) {
    let mut from = 0;
    while let Some(offset) = text[from..].find("-----BEGIN ") {
        let start = from + offset;
        let end = text[start..]
            .find("-----END ")
            .and_then(|end| {
                let after = start + end + "-----END ".len();
                text[after..].find("-----").map(|close| after + close + 5)
            })
            .unwrap_or(text.len());
        spans.push((start, end));
        from = end;
    }
}

fn url_userinfo(text: &str, spans: &mut Spans) {
    let bytes = text.as_bytes();
    for index in occurrences(text, "://") {
        let start = index + 3;
        let length = bytes[start..]
            .iter()
            .position(|&byte| {
                matches!(byte, b'/' | b'?' | b'#' | b'@') || byte.is_ascii_whitespace()
            })
            .unwrap_or(bytes.len() - start);
        if bytes.get(start + length) == Some(&b'@') && bytes[start..start + length].contains(&b':')
        {
            spans.push((start, start + length));
        }
    }
}

/// Whether an assignment or parameter name suggests its value is a secret.
fn secret_name(name: &str) -> bool {
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut after_lower = false;
    for c in name.chars() {
        if !c.is_ascii_alphanumeric() {
            words.extend((!word.is_empty()).then(|| std::mem::take(&mut word)));
            after_lower = false;
            continue;
        }
        if c.is_ascii_uppercase() && after_lower {
            words.push(std::mem::take(&mut word));
        }
        after_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
        word.push(c.to_ascii_lowercase());
    }
    words.extend((!word.is_empty()).then_some(word));
    words.iter().any(|word| {
        ["password", "passwd", "secret", "token", "apikey"]
            .iter()
            .any(|suffix| word.ends_with(suffix))
            || word == "credential"
            || word == "credentials"
    }) || words.last().is_some_and(|word| word == "key")
}

/// A value runs to whitespace, `&` (the next query parameter) or a quote.
fn value_end(bytes: &[u8], from: usize) -> usize {
    from + run_len(bytes, from, |byte| {
        !byte.is_ascii_whitespace() && !matches!(byte, b'&' | b'"' | b'\'')
    })
}

fn secret_assignments(text: &str, spans: &mut Spans) {
    let bytes = text.as_bytes();
    for (index, _) in text.match_indices('=') {
        if bytes.get(index + 1) == Some(&b'=')
            || index
                .checked_sub(1)
                .is_some_and(|before| matches!(bytes[before], b'=' | b'!' | b'<' | b'>'))
        {
            continue;
        }
        let name_start = bytes[..index]
            .iter()
            .rposition(|&byte| !(base64url(byte) || byte == b'.'))
            .map_or(0, |before| before + 1);
        if !secret_name(&text[name_start..index]) {
            continue;
        }
        let from = index + 1;
        let (start, end) = match bytes.get(from) {
            Some(&quote @ (b'"' | b'\'')) => {
                let start = from + 1;
                let end = bytes[start..]
                    .iter()
                    .position(|&byte| byte == quote)
                    .map_or(bytes.len(), |end| start + end);
                (start, end)
            }
            _ => (from, value_end(bytes, from)),
        };
        if end > start {
            spans.push((start, end));
        }
    }
}

fn secret_arguments(text: &str, spans: &mut Spans) {
    let bytes = text.as_bytes();
    for index in occurrences(text, "--") {
        if index > 0 && !bytes[index - 1].is_ascii_whitespace() {
            continue;
        }
        let name_start = index + 2;
        let name_end = name_start + run_len(bytes, name_start, base64url);
        if !matches!(
            &text[name_start..name_end],
            "password" | "passwd" | "token" | "secret" | "api-key" | "api_key"
        ) {
            continue;
        }
        let start = name_end + run_len(bytes, name_end, |byte| byte == b' ' || byte == b'\t');
        if start == name_end || bytes.get(start).is_none_or(|&byte| byte == b'-') {
            continue;
        }
        let end = start + run_len(bytes, start, |byte| !byte.is_ascii_whitespace());
        spans.push((start, end));
    }
}

fn provider_keys(text: &str, spans: &mut Spans) {
    let bytes = text.as_bytes();
    let word = |byte: u8| byte.is_ascii_alphanumeric();
    let upper = |byte: u8| byte.is_ascii_uppercase() || byte.is_ascii_digit();
    let pat = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let slack = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'-';
    let shapes: [Shape; 16] = [
        ("sk-", &base64url, 20, false),
        ("sk_live_", &word, 10, false),
        ("sk_test_", &word, 10, false),
        ("ghp_", &word, 20, false),
        ("gho_", &word, 20, false),
        ("ghu_", &word, 20, false),
        ("ghs_", &word, 20, false),
        ("ghr_", &word, 20, false),
        ("github_pat_", &pat, 20, false),
        ("xoxa-", &slack, 10, false),
        ("xoxb-", &slack, 10, false),
        ("xoxp-", &slack, 10, false),
        ("xoxr-", &slack, 10, false),
        ("AKIA", &upper, 16, true),
        ("ASIA", &upper, 16, true),
        ("AIza", &base64url, 30, false),
    ];
    for (prefix, allowed, minimum, exact) in shapes {
        for start in occurrences(text, prefix) {
            if start > 0 && bytes[start - 1].is_ascii_alphanumeric() {
                continue;
            }
            let after = start + prefix.len();
            let length = run_len(bytes, after, allowed);
            let fits = if exact {
                length == minimum && bytes.get(after + length).is_none_or(|&b| !word(b))
            } else {
                length >= minimum
            };
            if fits {
                spans.push((start, after + length));
            }
        }
    }
}

fn jwts(text: &str, spans: &mut Spans) {
    let bytes = text.as_bytes();
    for start in occurrences(text, "eyJ") {
        if start > 0 && base64url(bytes[start - 1]) {
            continue;
        }
        let header = run_len(bytes, start, base64url);
        let payload_start = start + header + 1;
        if header < 10 || bytes.get(start + header) != Some(&b'.') {
            continue;
        }
        let payload = run_len(bytes, payload_start, base64url);
        let signature_start = payload_start + payload + 1;
        if payload < 10 || bytes.get(payload_start + payload) != Some(&b'.') {
            continue;
        }
        let signature = run_len(bytes, signature_start, base64url);
        spans.push((start, signature_start + signature));
    }
}

fn high_entropy(text: &str, spans: &mut Spans) {
    let bytes = text.as_bytes();
    let token = |byte: u8| base64url(byte) || byte == b'+';
    let mut index = 0;
    while index < bytes.len() {
        if !token(bytes[index]) {
            index += 1;
            continue;
        }
        let start = index;
        let mut end = start + run_len(bytes, start, token);
        index = end;
        if end - start < 32 {
            continue;
        }
        end += run_len(bytes, end, |byte| byte == b'=');
        let run = &bytes[start..end];
        let mut lower_run = 0;
        let mut longest_lower = 0;
        for &byte in run {
            lower_run = if byte.is_ascii_lowercase() {
                lower_run + 1
            } else {
                0
            };
            longest_lower = longest_lower.max(lower_run);
        }
        if run.iter().any(u8::is_ascii_uppercase)
            && run.iter().any(u8::is_ascii_lowercase)
            && run.iter().any(u8::is_ascii_digit)
            && longest_lower <= 5
            && entropy(run) >= 4.0
        {
            spans.push((start, end));
        }
    }
}

/// Shannon entropy in bits per byte.
fn entropy(run: &[u8]) -> f64 {
    let mut counts = [0_u32; 256];
    for &byte in run {
        counts[usize::from(byte)] += 1;
    }
    let total = run.len() as f64;
    counts
        .iter()
        .filter(|&&count| count > 0)
        .map(|&count| {
            let p = f64::from(count) / total;
            -p * p.log2()
        })
        .sum()
}

/// Replace the home directory with `~` where it begins a path.
fn tilde(text: String, home: Option<&str>) -> String {
    let Some(home) = home
        .map(|home| home.trim_end_matches('/'))
        .filter(|home| home.len() > 1 && home.starts_with('/'))
    else {
        return text;
    };
    let bytes = text.as_bytes();
    let path = |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.');
    let mut out = String::with_capacity(text.len());
    let mut from = 0;
    for start in occurrences(&text, home) {
        if start < from {
            continue;
        }
        let end = start + home.len();
        let begins = start == 0 || !(path(bytes[start - 1]) || bytes[start - 1] == b'/');
        let ends = bytes
            .get(end)
            .is_none_or(|&byte| byte == b'/' || !path(byte));
        out.push_str(&text[from..start]);
        out.push_str(if begins && ends { "~" } else { home });
        from = end;
    }
    out.push_str(&text[from..]);
    out
}

#[cfg(test)]
#[path = "../../tests/support/secret_shape.rs"]
mod secret_shape;

#[cfg(test)]
mod tests {
    use super::secret_shape::secret_shape;
    use super::*;

    fn r(text: &str) -> String {
        redact_with_home(text, Some("/Users/owner"))
    }

    #[test]
    fn provider_key_shapes_are_redacted() {
        for secret in [
            secret_shape("sk-", "proj-AbCdEfGhIjKlMnOpQrStUvWx0123"),
            secret_shape("sk-", "ant-api03-AbCdEfGhIjKlMnOpQrSt"),
            secret_shape("sk_", "live_51HqLZ2EYpK8dQ8X5vT3nR9bC"),
            secret_shape("sk_", "test_4eC39HqLyjWDarjtT1zdp7dc"),
            secret_shape("gh", "p_16C7e42F292c6912E7710c838347Ae178B4a"),
            secret_shape("gh", "o_16C7e42F292c6912E7710c838347Ae178B4a"),
            secret_shape(
                "github_",
                "pat_11ABCDEFG0abcdefghijkl_mnopqrstuvwxyz0123456789",
            ),
            secret_shape("xo", "xb-123456789012-1234567890123-AbCdEfGhIjKl"),
            secret_shape("xo", "xp-1234-5678-abcdefgh"),
            secret_shape("AK", "IAIOSFODNN7EXAMPLE"),
            secret_shape("AS", "IAY34FZKBOKMUTVV7A"),
            secret_shape("AI", "zaSyD-9tSrke72PouQMnMX-a7eZSW0jkFMBWY"),
        ] {
            let text = format!("use {secret} for the call");
            assert_eq!(r(&text), "use [redacted] for the call", "{secret}");
        }
    }

    #[test]
    fn a_jwt_is_redacted() {
        let jwt = secret_shape(
            "eyJhbGciOiJIUzI1NiJ9.",
            "eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U",
        );
        assert_eq!(
            r(&format!("token {jwt} expired")),
            "token [redacted] expired"
        );
    }

    #[test]
    fn a_pem_block_is_redacted_whole_and_an_unterminated_one_to_the_end() {
        let pem = secret_shape(
            "-----BEGIN RSA PRIVATE",
            " KEY-----\nMIIEowIBAAKCAQEA\n-----END RSA PRIVATE KEY-----",
        );
        assert_eq!(r(&format!("key: {pem} done")), "key: [redacted] done");
        assert_eq!(
            r(&secret_shape(
                "paste -----BEGIN OPENSSH PRIVATE",
                " KEY----- b3BlbnNzaC1rZXk"
            )),
            "paste [redacted]"
        );
    }

    #[test]
    fn url_userinfo_is_redacted_and_the_host_kept() {
        assert_eq!(
            r("clone https://jon:hunter2@git.example.com/repo.git now"),
            "clone https://[redacted]@git.example.com/repo.git now"
        );
        // An address without a password is left alone.
        assert_eq!(
            r("ssh://git@example.com/repo.git"),
            "ssh://git@example.com/repo.git"
        );
    }

    #[test]
    fn secret_named_assignments_and_query_values_are_redacted() {
        assert_eq!(r("STRIPE_KEY=abc123 run"), "STRIPE_KEY=[redacted] run");
        assert_eq!(
            r("export GITHUB_TOKEN=\"two words\" ok"),
            "export GITHUB_TOKEN=\"[redacted]\" ok"
        );
        assert_eq!(
            r("GET /login?user=jon&password=hunter2&next=/"),
            "GET /login?user=jon&password=[redacted]&next=/"
        );
        assert_eq!(r("?token=abc&x=1"), "?token=[redacted]&x=1");
        assert_eq!(r("secret=s3"), "secret=[redacted]");
        assert_eq!(r("api_key=k"), "api_key=[redacted]");
        assert_eq!(r("PGPASSWORD=pw psql"), "PGPASSWORD=[redacted] psql");
        assert_eq!(r("clientSecret=xyz"), "clientSecret=[redacted]");
        assert_eq!(r("--password=pw"), "--password=[redacted]");
    }

    #[test]
    fn password_and_token_arguments_are_redacted() {
        assert_eq!(
            r("mysql --password hunter2 -u root"),
            "mysql --password [redacted] -u root"
        );
        assert_eq!(r("gh auth --token abc123"), "gh auth --token [redacted]");
    }

    #[test]
    fn long_high_entropy_tokens_are_redacted() {
        assert_eq!(
            r("value aB3dE5fG7hJ9kL1mN3pQ5rS7tU9vW1xY3 here"),
            "value [redacted] here"
        );
    }

    #[test]
    fn ordinary_text_paths_commit_ids_and_uuids_are_kept() {
        for text in [
            "fix the login retry in src/auth/session.rs",
            "/etc/hosts has a typo",
            "rename MyVeryLongClassNameForTesting1234 to Short",
            "XMLHttpRequestFactoryProviderBean2 is unused",
            "test_a_prompt_summary_is_the_first_line_only_and_at_most_its_cap",
            "revert 2580adc4f1e9a4b0c3d2e1f00112233445566778",
            "sha256 e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "session 123e4567-e89b-12d3-a456-426614174000 ended",
            "max_tokens=100 is fine? no: tokens are not a token",
            "task-list and sk-learn are words",
            "compare a == b and x=1",
            "see https://example.com/path?q=rust&page=2",
            "components/UserProfile2Settings/IndexPageWrapper.tsx",
        ] {
            assert_eq!(r(text), text);
        }
    }

    #[test]
    fn the_owners_home_prefix_becomes_a_tilde() {
        assert_eq!(
            r("edit /Users/owner/work/app/src/main.rs"),
            "edit ~/work/app/src/main.rs"
        );
        assert_eq!(r("cd /Users/owner"), "cd ~");
        assert_eq!(
            r("/Users/ownership/x and /Users/owner2"),
            "/Users/ownership/x and /Users/owner2",
            "only the whole home directory is replaced"
        );
        assert_eq!(redact_with_home("/ is root", Some("/")), "/ is root");
    }

    #[test]
    fn redaction_is_idempotent() {
        for text in [
            secret_shape("STRIPE_KEY=sk_", "live_51HqLZ2EYpK8dQ8X5vT3nR9bC now"),
            "https://a:b@h/x --token t password=p /Users/owner/x".to_owned(),
        ] {
            let once = r(&text);
            assert_eq!(r(&once), once);
        }
    }

    #[test]
    fn every_command_carrying_owner_or_agent_text_is_redacted() {
        let secret = "token=abc";
        let mut commands = [
            Command::SessionPrompt {
                session_id: "s".into(),
                summary: secret.into(),
                line_digests: Vec::new(),
            },
            Command::IntentSet {
                session_id: "s".into(),
                paths: vec![],
                summary: secret.into(),
            },
            Command::Remember {
                workspace_id: "w".into(),
                author: None,
                text: secret.into(),
                paths: vec![],
            },
            Command::SendMessage {
                workspace_id: "w".into(),
                from: None,
                to: None,
                text: secret.into(),
                paths: vec![],
            },
            Command::Handoff {
                session_id: "s".into(),
                text: secret.into(),
                paths: vec![],
            },
        ];
        for command in &mut commands {
            redact_command(command);
            let json = serde_json::to_string(command).unwrap();
            assert!(json.contains("token=[redacted]"), "{json}");
        }
    }

    #[test]
    fn bounded_redaction_stays_within_each_limit_and_is_a_fixed_point() {
        let home = Some("/Users/owner");
        let token = secret_shape("ghp_", "AbCdEfGhIjKlMnOpQrStUvWxYz0123456789");
        for limit in [
            langspan_core::MAX_CHECK_COMMAND_BYTES,
            langspan_core::MAX_PROMPT_SUMMARY_BYTES,
            langspan_core::MAX_INTENT_SUMMARY_BYTES,
        ] {
            let mut texts = vec![
                String::from("cargo test --workspace"),
                format!("{} --token x", "a".repeat(limit - 11)),
                format!("{} --token x", "é".repeat((limit - 10) / 2)),
                format!("{} token=", "a".repeat(limit - 7)),
                format!("{}{token}", "a ".repeat((limit - token.len()) / 2)),
                format!("{} --password p --secret s", "b".repeat(limit - 32)),
                MARK.to_owned(),
            ];
            // Every length from 24 under the limit to the limit, with a
            // two-byte character wherever the cut falls and several secrets.
            for width in limit - 24..=limit {
                let tail = " --token x password=y";
                let lead = if (width - tail.len()).is_multiple_of(2) {
                    ""
                } else {
                    "a"
                };
                let pad = "é".repeat((width - lead.len() - tail.len()) / 2);
                texts.push(format!("{lead}{pad}{tail}"));
            }
            for text in texts {
                assert!(text.len() <= limit, "fixture within the limit");
                let once = bounded(&text, limit, home);
                assert!(
                    !once.is_empty() && once.len() <= limit,
                    "{} bytes",
                    once.len()
                );
                assert_eq!(bounded(&once, limit, home), once, "{once}");
                for secret in [" x", "=y", "=p", " s", &token] {
                    assert!(!once.contains(secret), "{once}");
                }
                if text.len() + 9 > limit && text.contains("--token x") {
                    assert!(once.ends_with(MARK), "{once}");
                    // The longest redacted prefix that fits, cut at a
                    // character boundary; a cut inside a mark merges into it.
                    let redacted = r(&text);
                    let mut cut = limit - MARK.len();
                    while !redacted.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    let kept = once.strip_suffix(MARK).unwrap();
                    assert!(redacted.starts_with(kept), "{once}");
                    assert!(
                        kept.len() == cut || redacted[kept.len()..cut].starts_with('['),
                        "{once}"
                    );
                }
            }
        }
    }

    #[test]
    fn non_ascii_text_is_kept_intact() {
        assert_eq!(r("café token=été ünïcode"), "café token=[redacted] ünïcode");
    }
}
