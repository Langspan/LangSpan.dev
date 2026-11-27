// Lexing cases for test_ratchets.py. Not compiled.
//! A doc comment with a brace { and fn fake() {
pub use crate::other::Thing;
pub(crate) fn hidden() {}
pub struct Counted;
pub const LIMIT: usize = 3;

/* a block /* nested { */ comment */
pub fn strings() -> usize {
    let raw = r#"a "quoted" { brace"#;
    let bytes = br"\{";
    let plain = "escaped \" { brace";
    let ch = '{';
    let escaped = '\'';
    raw.len() + bytes.len() + plain.len() + ch.len_utf8() + escaped.len_utf8()
}

pub async fn borrowed<'a>(x: &'a str) -> &'a str {
    x
}

#[cfg(test)]
fn test_helper() {
    let _ = 1;
}

#[cfg(all(test, unix))]
#[allow(dead_code)]
mod tests {
    pub fn not_counted() {}
}

#[cfg(test)]
mod split_tests;

pub enum Last {
    A,
}
