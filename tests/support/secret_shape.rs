//! Shared by the redaction tests (`src/hook/redact.rs`, `tests/remote.rs`).
//!
//! Secret scanners such as GitHub push protection and gitleaks flag any
//! literal shaped like a live credential, even an invented one. A fixture
//! that must look like a key is therefore written as two halves split inside
//! the provider prefix (`"sk_"` + `"live_..."`), so no literal in the source
//! matches a secret pattern. The joined value is unchanged, so the redaction
//! tests still see exactly the shape they test.

/// `head` followed by `tail`, joined at run time.
pub fn secret_shape(head: &str, tail: &str) -> String {
    format!("{head}{tail}")
}
