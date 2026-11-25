"""Content checks for this repository's tree and for pull request text.

    python3 -I -B scripts/content-check.py [--blocklist FILE] [--text-env NAME]... TREE
    python3 -I -B scripts/content-check.py --entry PHRASE

The first form checks every file under TREE (the `.git` directory aside),
then the text of each named environment variable (CI passes a pull request's
title and body this way, never on the command line or inside a script).
It exits 1 naming each hit by file and line, without quoting the text.
--blocklist reads another list than scripts/content-blocklist.txt (tests use
it with made-up terms).
The second form prints the blocklist line for PHRASE, to add a term.

Checks:

- blocked terms from scripts/content-blocklist.txt. The list holds salted
  SHA-256 digests, not the terms, so this repository does not carry the
  words it refuses. A one-word term matches inside any word, in any case; a
  longer term matches those words in sequence, separated by anything that
  is not a letter or digit, line breaks included;
- home directories other than the placeholders tests use (/Users/owner,
  /home/user and similar);
- links to agent sandboxes anywhere, and links to agent sessions in the tree
  (pull request text may end with one as attribution);
- names of the project's earlier repositories, and wording that says this
  repository is closed or needs access;
- literal live-key-shaped strings (build test values at run time instead);
- in the tree only: a file over 1 MiB, a relative Markdown link or a
  CLAUDE.md import that does not resolve, and a missing Apache-2.0 licence.

It does not judge prose and does not replace secret scanning.
"""

import hashlib
import os
import re
import sys

SALT = b"langspan-content-check-1:"
HERE = os.path.dirname(os.path.abspath(__file__))
BLOCKLIST = os.path.join(HERE, "content-blocklist.txt")
MAX_BYTES = 1024 * 1024

WORD = re.compile(r"[a-z0-9]+")
PLACEHOLDERS = {
    "user", "u", "owner", "owner2", "ownership", "me", "you", "alice", "bob",
    "example", "runner", "name", "someone",
}
HOME = re.compile(r"/(?:Users|home)/([A-Za-z0-9._-]+)")
# Each pattern is written so that it does not match its own text here.
PATTERNS = [
    # A session link opens only for the account that owns it. It is the
    # attribution an agent's pull request ends with, so pull request text may
    # carry it; a file in the tree may not.
    ("agent session link", re.compile(r"claude\.ai/code/session_", re.I)),
    ("agent sandbox link", re.compile(
        r"cursor\.com/agents|/workspace/(?:reports|scratch)", re.I)),
    ("earlier repository name", re.compile(
        r"langspan/[l]angspan(?:\.git)?(?:[^.a-z0-9_-]|$)", re.I)),
    ("externally hosted report link", re.compile(r"grok\.bot", re.I)),
    ("restricted-access wording", re.compile(
        r"repository access is [r]equired|private (?:core )?[r]epositor|requires? (?:source|repository) access", re.I)),
    ("literal live-key-shaped string", re.compile(
        r"(?:sk|rk)_live_[0-9A-Za-z]{8,}|xox[abposr]-[0-9A-Za-z-]{8,}")),
]


def digest(text):
    return hashlib.sha256(SALT + text.encode("utf-8")).hexdigest()[:24]


def entry(phrase):
    words = WORD.findall(phrase.lower())
    if not words:
        raise ValueError("a term needs at least one letter or digit")
    if len(words) == 1:
        return f"1 {len(words[0])} {digest(words[0])}"
    return f"{len(words)} 0 {digest(' '.join(words))}"


def load_blocklist(path):
    singles = {}  # length -> set of digests
    phrases = {}  # word count -> set of digests
    with open(path, encoding="utf-8") as f:
        for n, line in enumerate(f, 1):
            line = line.split("#", 1)[0].strip()
            if not line:
                continue
            parts = line.split()
            if len(parts) != 3 or not parts[0].isdigit() or not parts[1].isdigit():
                raise ValueError(f"{path}:{n}: expected 'WORDS LENGTH DIGEST'")
            words, length, d = int(parts[0]), int(parts[1]), parts[2]
            if words == 1:
                singles.setdefault(length, set()).add(d)
            else:
                phrases.setdefault(words, set()).add(d)
    if not singles and not phrases:
        raise ValueError(f"{path}: the blocklist is empty")
    return singles, phrases


class Checker:
    def __init__(self, singles, phrases):
        self.singles = singles
        self.phrases = phrases
        self.seen = {}
        self.hits = []

    def word_blocked(self, word):
        if word not in self.seen:
            found = False
            for length, digests in self.singles.items():
                for i in range(len(word) - length + 1):
                    if digest(word[i:i + length]) in digests:
                        found = True
                        break
                if found:
                    break
            self.seen[word] = found
        return self.seen[word]

    def lines(self, located, allowed=()):
        """Check (where, text) pairs, one per line of one file or text.

        Patterns are matched per line. Blocked terms are matched over the
        words of all the lines in order, so a phrase broken across a line
        break is still found; a hit is named at the line of its first word.
        """
        words = []  # (word, where)
        for where, text in located:
            words.extend((w, where) for w in WORD.findall(text.lower()))
            for m in HOME.finditer(text):
                if m.group(1) not in PLACEHOLDERS:
                    self.hits.append((where, "home directory"))
            for label, pattern in PATTERNS:
                if label not in allowed and pattern.search(text):
                    self.hits.append((where, label))
        blocked = []
        for i, (w, where) in enumerate(words):
            if self.word_blocked(w):
                blocked.append(where)
                continue
            for n, digests in self.phrases.items():
                if i + n <= len(words) and digest(" ".join(x for x, _ in words[i:i + n])) in digests:
                    blocked.append(where)
                    break
        for where in dict.fromkeys(blocked):
            self.hits.append((where, "blocked term"))

    def text(self, name, value):
        self.lines(
            ((f"{name}:{n}", t) for n, t in enumerate(value.splitlines(), 1)),
            allowed=("agent session link",),
        )


def tree_files(root):
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = sorted(d for d in dirnames if d != ".git")
        for name in sorted(filenames):
            path = os.path.join(dirpath, name)
            if not os.path.islink(path):
                yield path


LINK = re.compile(r"\]\(([^)]+)\)")


def broken_links(root, rel, text):
    base = os.path.dirname(os.path.join(root, rel))
    fence = False
    for n, line in enumerate(text.splitlines(), 1):
        if line.startswith("```"):
            fence = not fence
            continue
        if fence:
            continue
        targets = [m.group(1) for m in LINK.finditer(line)]
        if os.path.basename(rel) == "CLAUDE.md" and re.fullmatch(r"@[^ ]+", line):
            targets.append(line[1:])
        for t in targets:
            t = t.split(" ", 1)[0].split("#", 1)[0]
            if not t or ":" in t or t.startswith("<"):
                continue
            p = os.path.join(root, t.lstrip("/")) if t.startswith("/") else os.path.join(base, t)
            if not os.path.exists(p):
                yield n, t


def check_tree(checker, root):
    for path in tree_files(root):
        rel = os.path.relpath(path, root)
        size = os.path.getsize(path)
        if size > MAX_BYTES:
            checker.hits.append((rel, "file over 1 MiB"))
            continue
        with open(path, "rb") as f:
            data = f.read()
        if b"\0" in data[:8192]:
            continue
        text = data.decode("utf-8", errors="replace")
        checker.lines((f"{rel}:{n}", line) for n, line in enumerate(text.splitlines(), 1))
        if rel.endswith(".md"):
            for n, t in broken_links(root, rel, text):
                checker.hits.append((f"{rel}:{n}", f"broken relative link {t}"))
    licence = os.path.join(root, "LICENSE")
    try:
        with open(licence, encoding="utf-8") as f:
            text = f.read()
        if "Apache License" not in text or "Version 2.0" not in text:
            checker.hits.append(("LICENSE", "not the Apache-2.0 text"))
    except OSError:
        checker.hits.append(("LICENSE", "missing"))


def main(argv):
    args = argv[1:]
    if len(args) == 2 and args[0] == "--entry":
        print(entry(args[1]))
        return 0
    envs = []
    blocklist = BLOCKLIST
    while len(args) >= 2 and args[0] in ("--text-env", "--blocklist"):
        if args[0] == "--text-env":
            envs.append(args[1])
        else:
            blocklist = args[1]
        args = args[2:]
    if len(args) != 1 or not os.path.isdir(args[0]):
        print("usage: content-check.py [--blocklist FILE] [--text-env NAME]... TREE | --entry PHRASE", file=sys.stderr)
        return 2
    try:
        checker = Checker(*load_blocklist(blocklist))
    except (OSError, ValueError) as e:
        # A check that cannot load its list fails rather than passing.
        print(f"content-check: cannot read the blocklist: {e}", file=sys.stderr)
        return 2
    check_tree(checker, args[0])
    for name in envs:
        if name not in os.environ:
            print(f"content-check: environment variable {name} is not set", file=sys.stderr)
            return 2
        checker.text(name, os.environ[name])
    for where, label in checker.hits:
        print(f"{where}: {label}")
    if checker.hits:
        print(f"content-check: {len(checker.hits)} hit(s)", file=sys.stderr)
        return 1
    print("content-check: clean")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
