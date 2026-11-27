//! The hosts LangSpan installs hooks for, by name, and the shell quoting
//! their installed commands use.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vendor {
    ClaudeCode,
    Codex,
    Cursor,
    /// Gemini CLI, no longer supported (issue #246, ADR 0033). For one
    /// release LangSpan still recognizes what an earlier install wrote, so
    /// `uninstall`, `hooks remove` and `doctor` can deal with it; nothing
    /// installs it, and `langspan hook gemini-cli` does nothing.
    Gemini,
    Antigravity,
}

impl Vendor {
    /// Every supported host, in the order help text and checks list them.
    /// Gemini CLI is not one of them: see `Vendor::Gemini`.
    pub const ALL: [Self; 4] = [
        Self::ClaudeCode,
        Self::Codex,
        Self::Cursor,
        Self::Antigravity,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
            Self::Gemini => "gemini-cli",
            Self::Antigravity => "antigravity",
        }
    }

    /// The host a command-line name stands for, Gemini CLI's included so
    /// that removing it works; `gemini` also names it.
    pub fn from_name(name: &str) -> Option<Self> {
        if name == "gemini" {
            return Some(Self::Gemini);
        }
        Self::ALL
            .into_iter()
            .chain([Self::Gemini])
            .find(|vendor| vendor.name() == name)
    }

    /// Whether the host is no longer supported: only removal works for it.
    pub fn removed(self) -> bool {
        self == Self::Gemini
    }
}

pub fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}
