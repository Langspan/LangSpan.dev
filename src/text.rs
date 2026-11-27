//! Text for people: cleaned, quoted and cut peer text, and ages.
pub(crate) mod redact;

pub(crate) const SUMMARY_LIMIT: usize = 200;

/// Characters that can forge a line break or reorder what a reader sees.
fn deceptive(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{2028}' | '\u{2029}' | '\u{feff}')
        || ('\u{200b}'..='\u{200f}').contains(&c)
        || ('\u{202a}'..='\u{202e}').contains(&c)
        || ('\u{2066}'..='\u{2069}').contains(&c)
}

/// Text from another session or the repository, made safe to place on one
/// line: deceptive characters become spaces and the result is at most
/// `limit` bytes including its ellipsis.
pub(crate) fn clean(text: &str, limit: usize) -> String {
    let text: String = text
        .chars()
        .map(|c| if deceptive(c) { ' ' } else { c })
        .collect();
    let text = text.trim();
    if text.len() <= limit {
        return text.into();
    }
    let mut end = limit.saturating_sub('…'.len_utf8());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// An age for a person: seconds up to 89, then minutes.
pub fn age(ms: u64) -> String {
    match ms / 1000 {
        seconds @ 0..=89 => format!("{seconds}s ago"),
        seconds => format!("{}m ago", seconds / 60),
    }
}

pub(crate) fn quoted(text: &str) -> String {
    format!("{:?}", clean(text, SUMMARY_LIMIT))
}

/// `text` in at most `limit` bytes, the ellipsis that marks a cut included.
pub(crate) fn truncate(text: &str, limit: usize) -> String {
    let end = kept_bytes(text, limit);
    if end == text.len() {
        return text.into();
    }
    format!("{}…", &text[..end])
}

/// How many leading bytes of `text` `truncate` keeps within `limit`.
pub(crate) fn kept_bytes(text: &str, limit: usize) -> usize {
    if text.len() <= limit {
        return text.len();
    }
    let mut end = limit.saturating_sub('…'.len_utf8());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}
