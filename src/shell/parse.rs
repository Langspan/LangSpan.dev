//! The files a few literal shell forms will write, and the paths a command
//! reads. Pure and conservative: anything outside the grammar is unknown.

use std::collections::BTreeSet;

/// A longer command is not parsed.
pub const MAX_COMMAND_BYTES: usize = 8 * 1024;
/// At most this many write targets, the core's limit for one admission.
pub const MAX_TARGETS: usize = 64;

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
enum Token {
    Word(String),
    /// `;`, `&&` or `|`: the parts all run, so their writes are a union.
    Separator,
    /// `>` or `>>`, with the file descriptor written before it, if any.
    Redirect(Option<u32>),
    /// `N>&M`: a descriptor copied, no file named.
    Duplicate,
}

/// The files a command will write, when the whole command is a sequence of
/// supported forms whose targets are literal paths. `Ok(empty)` means it
/// writes no file (`echo`, `cat a`); `Err` means the write set is unknown.
/// Paths are as written (`./` segments removed), not yet resolved.
pub fn write_targets(command: &str) -> Result<Vec<String>, &'static str> {
    if command.len() > MAX_COMMAND_BYTES {
        return Err("command longer than the parsed limit");
    }
    let tokens = tokenize(command)?;
    let mut targets = BTreeSet::new();
    for part in tokens.split(|token| *token == Token::Separator) {
        simple_command(part, &mut targets)?;
        if targets.len() > MAX_TARGETS {
            return Err("command names more than 64 write targets");
        }
    }
    Ok(targets.into_iter().collect())
}

/// At most this many words of one command are taken as paths it looked at.
pub const MAX_LOOKED_WORDS: usize = 16;

/// The words of a command that name what it reads, for presence (roadmap
/// A13): every operand of a program that only reads or lists the files it
/// is given (`cat`, `head`, `ls`, ...), and the operands after the pattern
/// of a search (`grep`, `rg`) whose options are all flags that program is
/// known to take without a value, so the pattern is never among them. A
/// search with any other option gives nothing. Options are not operands.
/// After a command that can change the shell's directory (`cd`, `pushd`,
/// `popd`, `source`, `.`, `eval`), only absolute operands are taken, since
/// a relative one would be resolved against the wrong directory. Anything
/// the parser does not take, or any other program, gives nothing. The
/// words are as written; the caller keeps only those that name an existing
/// path in the checkout.
pub fn looked_at(command: &str) -> Vec<String> {
    const READERS: [&str; 13] = [
        "cat", "head", "tail", "less", "more", "wc", "nl", "ls", "stat", "file", "diff", "tree",
        "du",
    ];
    // Each search program's flags that take no value. A flag of one may
    // take a value in another (`rg -E ENCODING`, `rg -r REPLACEMENT`).
    const GREP: (&str, &[&str]) = (
        "nrRilLcHhsFEwvoxqI",
        &[
            "--line-number",
            "--ignore-case",
            "--recursive",
            "--files-with-matches",
            "--count",
            "--fixed-strings",
            "--word-regexp",
            "--invert-match",
        ],
    );
    const RG: (&str, &[&str]) = (
        "nilcHIFwvoxqusSLN",
        &[
            "--line-number",
            "--ignore-case",
            "--files-with-matches",
            "--count",
            "--fixed-strings",
            "--word-regexp",
            "--invert-match",
            "--hidden",
            "--no-heading",
        ],
    );
    const DIRECTORY: [&str; 5] = ["cd", "pushd", "popd", "source", "eval"];
    if command.len() > MAX_COMMAND_BYTES {
        return Vec::new();
    }
    let Ok(tokens) = tokenize(command) else {
        return Vec::new();
    };
    let mut looked = Vec::new();
    // Set once a command may have changed the shell's directory.
    let mut moved = false;
    for part in tokens.split(|token| *token == Token::Separator) {
        let mut words = Vec::new();
        let mut tokens = part.iter();
        while let Some(token) = tokens.next() {
            match token {
                Token::Word(word) => words.push(word.as_str()),
                // A redirection's file is written, not read.
                Token::Redirect(_) => {
                    tokens.next();
                }
                Token::Duplicate | Token::Separator => {}
            }
        }
        let Some((&program, args)) = words.split_first() else {
            continue;
        };
        let name = program.rsplit('/').next().unwrap_or(program);
        let search = match name {
            "grep" | "egrep" | "fgrep" => Some(GREP),
            "rg" => Some(RG),
            _ => None,
        };
        let operands: Vec<&str> = if READERS.contains(&name) {
            args.iter()
                .copied()
                .filter(|arg| !arg.starts_with('-'))
                .collect()
        } else if let Some((flags, long)) = search {
            match operands(args, flags, long) {
                // The first operand is the pattern.
                Ok(operands) => operands.into_iter().skip(1).collect(),
                Err(_) => Vec::new(),
            }
        } else {
            Vec::new()
        };
        let relative_ok = !moved;
        // `command cd`, `builtin cd` or `X=1 cd` move the shell as well.
        moved |= program == "." || words.iter().any(|word| DIRECTORY.contains(word));
        for operand in operands
            .into_iter()
            .filter(|operand| relative_ok || operand.starts_with('/'))
        {
            let operand = operand.trim_start_matches("./");
            if !operand.is_empty() && !looked.iter().any(|seen| seen == operand) {
                looked.push(operand.to_owned());
            }
        }
    }
    looked.truncate(MAX_LOOKED_WORDS);
    looked
}

fn tokenize(command: &str) -> Result<Vec<Token>, &'static str> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    // A word exists once any of its characters or quotes has been read, so
    // `''` is an empty word rather than nothing.
    let mut in_word = false;
    let mut quoted = false;
    let mut chars = command.chars().peekable();
    let end_word =
        |tokens: &mut Vec<Token>, word: &mut String, in_word: &mut bool, quoted: &mut bool| {
            if *in_word {
                tokens.push(Token::Word(std::mem::take(word)));
            }
            *in_word = false;
            *quoted = false;
        };
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => end_word(&mut tokens, &mut word, &mut in_word, &mut quoted),
            '\n' | '\r' => return Err("a command of more than one line"),
            ';' => {
                end_word(&mut tokens, &mut word, &mut in_word, &mut quoted);
                tokens.push(Token::Separator);
            }
            '&' => {
                end_word(&mut tokens, &mut word, &mut in_word, &mut quoted);
                if chars.next_if_eq(&'&').is_none() {
                    return Err("a background command or `&>`");
                }
                tokens.push(Token::Separator);
            }
            '|' => {
                end_word(&mut tokens, &mut word, &mut in_word, &mut quoted);
                if chars.peek().is_some_and(|next| matches!(next, '|' | '&')) {
                    return Err("`||` or `|&`");
                }
                tokens.push(Token::Separator);
            }
            '>' => {
                // `2>` names a descriptor only when the digits stand alone.
                let fd = if in_word && !quoted && word.bytes().all(|b| b.is_ascii_digit()) {
                    let fd = word.parse::<u32>().map_err(|_| "a malformed redirection")?;
                    word.clear();
                    in_word = false;
                    Some(fd)
                } else {
                    end_word(&mut tokens, &mut word, &mut in_word, &mut quoted);
                    None
                };
                chars.next_if_eq(&'>');
                if chars.next_if_eq(&'&').is_some() {
                    let mut digits = 0;
                    while chars.next_if(char::is_ascii_digit).is_some() {
                        digits += 1;
                    }
                    if digits == 0 {
                        return Err("a malformed descriptor copy");
                    }
                    tokens.push(Token::Duplicate);
                } else if chars.peek() == Some(&'|') {
                    return Err("`>|`");
                } else {
                    tokens.push(Token::Redirect(fd));
                }
            }
            '\'' => {
                in_word = true;
                quoted = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => word.push(c),
                        None => return Err("an unterminated quote"),
                    }
                }
            }
            '"' => {
                in_word = true;
                quoted = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('$' | '`' | '\\') => {
                            return Err("an expansion or escape inside double quotes");
                        }
                        Some(c) => word.push(c),
                        None => return Err("an unterminated quote"),
                    }
                }
            }
            // A heredoc, input redirection or process substitution stops the
            // parse here, before any body that follows is read as shell.
            '<' => return Err("input redirection, a heredoc or process substitution"),
            '$' | '`' => return Err("a variable or command substitution"),
            '\\' => return Err("a backslash escape"),
            '*' | '?' | '[' | ']' | '{' | '}' | '~' => return Err("a glob or expansion"),
            '(' | ')' => return Err("a subshell or function"),
            '!' | '#' => return Err("history expansion or a comment"),
            c if c.is_control() => return Err("a control character"),
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    end_word(&mut tokens, &mut word, &mut in_word, &mut quoted);
    Ok(tokens)
}

fn simple_command(tokens: &[Token], targets: &mut BTreeSet<String>) -> Result<(), &'static str> {
    let mut words = Vec::new();
    let mut tokens = tokens.iter();
    while let Some(token) = tokens.next() {
        match token {
            Token::Word(word) => words.push(word.as_str()),
            Token::Duplicate => {}
            Token::Redirect(fd) => {
                let Some(Token::Word(target)) = tokens.next() else {
                    return Err("a redirection without a file");
                };
                if !matches!(fd, None | Some(1) | Some(2)) {
                    return Err("a redirection of an unusual descriptor");
                }
                if target != "/dev/null" {
                    targets.insert(literal_path(target)?);
                }
            }
            Token::Separator => return Err("a separator inside one command"),
        }
    }
    let Some((&name, args)) = words.split_first() else {
        // `> file` alone truncates it; an empty part is a syntax error.
        return if targets.is_empty() {
            Err("an empty command")
        } else {
            Ok(())
        };
    };
    if name.contains('=') {
        return Err("a variable assignment");
    }
    let mut add = |paths: &[&str]| -> Result<(), &'static str> {
        for path in paths {
            targets.insert(literal_path(path)?);
        }
        Ok(())
    };
    match name {
        "echo" | "printf" | "cat" | "true" => Ok(()),
        "tee" => add(&operands(
            args,
            "aip",
            &["--append", "--ignore-interrupts"],
        )?),
        "rm" => add(&nonempty(operands(
            args,
            "fiv",
            &["--force", "--verbose"],
        )?)?),
        "touch" => add(&nonempty(operands(args, "acm", &["--no-create"])?)?),
        "mv" | "cp" => {
            let flags = if name == "mv" { "fniv" } else { "fnipv" };
            let files = operands(args, flags, &["--force", "--verbose"])?;
            let [source, destination] = files.as_slice() else {
                return Err("mv or cp with other than one source and one destination");
            };
            if name == "mv" {
                add(&[source, destination])
            } else {
                add(&[destination])
            }
        }
        "truncate" => add(&truncate_operands(args)?),
        "sed" => {
            let files = sed_targets(args)?;
            add(&files.iter().map(String::as_str).collect::<Vec<_>>())
        }
        _ => Err("a command outside the supported set"),
    }
}

/// Non-option arguments, after checking every option is one of `flags`
/// (single letters, combinable) or `long`. `--` ends the options.
fn operands<'a>(
    args: &[&'a str],
    flags: &str,
    long: &[&str],
) -> Result<Vec<&'a str>, &'static str> {
    let mut files = Vec::new();
    let mut options = true;
    for &arg in args {
        if options && arg == "--" {
            options = false;
        } else if options && arg.starts_with("--") {
            if !long.contains(&arg) {
                return Err("an unsupported option");
            }
        } else if options && arg.starts_with('-') && arg.len() > 1 {
            if !arg[1..].chars().all(|c| flags.contains(c)) {
                return Err("an unsupported option");
            }
        } else {
            files.push(arg);
        }
    }
    Ok(files)
}

fn nonempty(files: Vec<&str>) -> Result<Vec<&str>, &'static str> {
    if files.is_empty() {
        return Err("a command with no file operand");
    }
    Ok(files)
}

fn truncate_operands<'a>(args: &[&'a str]) -> Result<Vec<&'a str>, &'static str> {
    let mut files = Vec::new();
    let mut sized = false;
    let mut args = args.iter();
    while let Some(&arg) = args.next() {
        if arg == "-s" {
            args.next().ok_or("truncate -s without a size")?;
            sized = true;
        } else if arg.starts_with("--size=") || (arg.starts_with("-s") && arg.len() > 2) {
            sized = true;
        } else if arg == "-c" || arg == "--no-create" {
        } else if arg.starts_with('-') {
            return Err("an unsupported option");
        } else {
            files.push(arg);
        }
    }
    if !sized {
        return Err("truncate without a size");
    }
    nonempty(files)
}

/// `sed` writes files only with `-i`, and then only those it is given, when
/// its script is made of commands that write nowhere else.
fn sed_targets(args: &[&str]) -> Result<Vec<String>, &'static str> {
    let mut in_place = false;
    let mut suffix = String::new();
    let mut script = false;
    let mut files = Vec::new();
    let mut args = args.iter().peekable();
    let mut options = true;
    while let Some(&arg) = args.next() {
        match arg {
            "--" if options => options = false,
            "-i" | "--in-place" if options => {
                in_place = true;
                // BSD sed takes the backup suffix as the next argument; an
                // empty one is the usual `sed -i '' …`.
                args.next_if(|next| next.is_empty());
            }
            "-e" if options => {
                sed_script(args.next().ok_or("sed -e without a script")?)?;
                script = true;
            }
            _ if options && arg.starts_with("--in-place=") => {
                in_place = true;
                suffix = arg["--in-place=".len()..].into();
            }
            _ if options && arg.starts_with("--expression=") => {
                sed_script(&arg["--expression=".len()..])?;
                script = true;
            }
            _ if options && arg.starts_with("-i") => {
                in_place = true;
                suffix = arg[2..].into();
            }
            "--quiet" | "--silent" | "--regexp-extended" | "--posix" | "--separate"
            | "--null-data"
                if options => {}
            _ if options && arg.starts_with('-') && arg.len() > 1 => {
                if !arg[1..].chars().all(|c| "nErsuz".contains(c)) {
                    return Err("an unsupported sed option");
                }
            }
            _ if !script => {
                sed_script(arg)?;
                script = true;
            }
            _ => files.push(arg),
        }
    }
    if !script {
        return Err("sed without a script");
    }
    if !in_place {
        return Ok(Vec::new());
    }
    if files.is_empty() {
        return Err("sed -i without a file");
    }
    if suffix.contains(['/', '*']) {
        return Err("a sed backup suffix naming another place");
    }
    let mut targets: Vec<String> = files.iter().map(|file| (*file).to_owned()).collect();
    if !suffix.is_empty() {
        targets.extend(files.iter().map(|file| format!("{file}{suffix}")));
    }
    Ok(targets)
}

/// Accept a script of `s`, `y`, `d` and `p` commands with simple addresses.
/// Anything that could write or run elsewhere (`w`, `W`, `r`, `e`, an `s`
/// flag of `w` or `e`, a script file) is refused.
fn sed_script(script: &str) -> Result<(), &'static str> {
    const UNSUPPORTED: &str = "a sed script with commands outside s, y, d and p";
    let mut chars = script.chars().peekable();
    let skip_blank = |chars: &mut std::iter::Peekable<std::str::Chars>| {
        while chars
            .next_if(|c| matches!(c, ' ' | '\t' | '\n' | ';'))
            .is_some()
        {}
    };
    // Read up to an unescaped `delimiter`, returning false if it never comes.
    let delimited = |chars: &mut std::iter::Peekable<std::str::Chars>, delimiter: char| loop {
        match chars.next() {
            Some('\\') => {
                if chars.next().is_none() {
                    return false;
                }
            }
            Some('\n') | None => return false,
            Some(c) if c == delimiter => return true,
            Some(_) => {}
        }
    };
    let address = |chars: &mut std::iter::Peekable<std::str::Chars>| -> Result<(), &'static str> {
        if chars.next_if(char::is_ascii_digit).is_some() {
            while chars.next_if(char::is_ascii_digit).is_some() {}
        } else if chars.next_if_eq(&'$').is_some() {
        } else if chars.next_if_eq(&'/').is_some() && !delimited(chars, '/') {
            return Err(UNSUPPORTED);
        }
        Ok(())
    };
    skip_blank(&mut chars);
    while chars.peek().is_some() {
        address(&mut chars)?;
        if chars.next_if_eq(&',').is_some() {
            address(&mut chars)?;
        }
        chars.next_if_eq(&'!');
        match chars.next() {
            Some(command @ ('s' | 'y')) => {
                let delimiter = chars.next().ok_or(UNSUPPORTED)?;
                if matches!(delimiter, '\\' | '\n' | ' ') {
                    return Err(UNSUPPORTED);
                }
                if !delimited(&mut chars, delimiter) || !delimited(&mut chars, delimiter) {
                    return Err(UNSUPPORTED);
                }
                if command == 's' {
                    while chars
                        .next_if(|c| c.is_ascii_digit() || matches!(c, 'g' | 'i' | 'I' | 'p'))
                        .is_some()
                    {}
                }
            }
            Some('d' | 'p') => {}
            _ => return Err(UNSUPPORTED),
        }
        while chars.next_if(|c| matches!(c, ' ' | '\t')).is_some() {}
        match chars.peek() {
            None | Some(';' | '\n') => skip_blank(&mut chars),
            Some(_) => return Err(UNSUPPORTED),
        }
    }
    Ok(())
}

/// A literal path with `.` segments removed. `..`, an empty path and one
/// that names only `.` are refused.
fn literal_path(path: &str) -> Result<String, &'static str> {
    if path.is_empty() || path.len() > 4096 {
        return Err("an empty or overlong path");
    }
    let absolute = path.starts_with('/');
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err("a path with `..`"),
            part => parts.push(part),
        }
    }
    if parts.is_empty() {
        return Err("a path naming a directory");
    }
    let joined = parts.join("/");
    Ok(if absolute {
        format!("/{joined}")
    } else {
        joined
    })
}

/// The file a quoted heredoc writes whole, and what it writes: `cat > PATH
/// <<'EOF'` or `cat <<'EOF' > PATH` (the delimiter in single or double
/// quotes), the body, then a line that is exactly the delimiter, with
/// nothing after it. A quoted delimiter means the shell expands nothing in
/// the body, so the body is the file's new content. Used for the write-time
/// break check ([ADR 0029](../docs/adr/0029-break-alerts-at-write-time.md));
/// it admits nothing. Any other shape is `None`: `>>`, `<<-`, an unquoted
/// delimiter, a second heredoc or command, or text after the closing line.
/// The path is as written, not yet resolved.
pub fn heredoc_write(command: &str) -> Option<(String, String)> {
    if command.len() > langspan_graph::MAX_SOURCE_BYTES + MAX_COMMAND_BYTES {
        return None;
    }
    let (head, rest) = command.split_once('\n')?;
    if head.len() > MAX_COMMAND_BYTES || head.contains(">>") {
        return None;
    }
    let at = head.find("<<")?;
    let word = head[at + 2..].trim_start_matches([' ', '\t']);
    let quote = word.chars().next().filter(|c| matches!(c, '\'' | '"'))?;
    let (delimiter, tail) = word[1..].split_once(quote)?;
    if delimiter.is_empty()
        || !delimiter
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        || tail.starts_with(|c: char| !matches!(c, ' ' | '\t'))
    {
        return None;
    }
    // What is left of the first line: `cat` and one `>` redirection. A
    // second `<` stops the tokenizer.
    let tokens = tokenize(&format!("{} {tail}", &head[..at])).ok()?;
    let [
        Token::Word(name),
        Token::Redirect(None | Some(1)),
        Token::Word(target),
    ] = tokens.as_slice()
    else {
        return None;
    };
    if name != "cat" || target == "/dev/null" {
        return None;
    }
    let target = literal_path(target).ok()?;
    let mut lines = rest.split('\n');
    let mut body = String::new();
    loop {
        let line = lines.next()?;
        if line == delimiter {
            break;
        }
        body.push_str(line);
        body.push('\n');
    }
    match (lines.next(), lines.next()) {
        (None, _) | (Some(""), None) => Some((target, body)),
        _ => None,
    }
}
