"""The shared-checkout arm (issue #543): every session in one checkout.

The sessions work in WORK/trunk, on `main`, cloned from a bare integration
remote WORK/origin.git. Each commits only its own files and pushes to
`origin main`; on a rejection it fetches and merges, then pushes again. The
combined result is what `origin main` holds when every session has ended.

Push attempts and rejections are counted from a session's transcript: each
Bash call that runs `git push`, and whether its result shows Git's
rejection. The stand-in makes its commits and pushes as Bash calls in the
same stream-json shape, so one count serves stand-in and real sessions.
"""

import io
import json
import re
import shlex
import subprocess
import time
import uuid

GIT_SECONDS = 60
# How many times the stand-in pushes before it gives up, as an agent would.
MAX_PUSH_ATTEMPTS = 3
# Sessions in one checkout contend for Git's index and ref locks.
LOCK_RETRIES = 20
LOCK_WAIT_SECONDS = 0.1
OUTPUT_BYTES = 4000
# Words that can come before the program a simple command runs.
PREFIXES = {"!", "{", "}", "if", "then", "else", "elif", "do", "while", "until", "time", "command", "exec",
            "nohup", "env"}
ASSIGNMENT = re.compile(r"[A-Za-z_][A-Za-z0-9_]*=")
# Git's global options that take the next word as their value.
GIT_OPTION_ARGS = {"-C", "-c", "--git-dir", "--work-tree", "--namespace", "--config-env"}
SHELLS = {"sh", "bash", "zsh"}
SHELL_COMMAND = re.compile(r"-[a-z]*c[a-z]*")
# Git's own words for a push the remote refused.
REJECTED = re.compile(r"\[(?:remote )?rejected\]|failed to push")
# A lock a peer holds, or a merge a peer's pull has not concluded yet.
LOCKED = re.compile(r"index\.lock|cannot lock ref|Unable to create|during a merge|not concluded your merge")
IDENTITY = (("user.name", "Benchmark"), ("user.email", "benchmark@example.invalid"), ("commit.gpgsign", "false"))


def git(cwd, *args):
    done = subprocess.run(["git", *args], cwd=cwd, capture_output=True, text=True, timeout=GIT_SECONDS)
    if done.returncode != 0:
        raise RuntimeError("git %s failed in %s: %s" % (" ".join(args), cwd, done.stderr.strip()))
    return done


def identify(checkout):
    """The commit identity the sessions use in CHECKOUT."""
    for key, value in IDENTITY:
        git(checkout, "config", key, value)


def lands_by_pushing(isolation, family):
    """Whether sessions land their work by pushing to `origin main`: in the
    shared checkout, and in every arm for the push-contention family
    (issue #611). Otherwise each commits on its own branch, merged later."""
    return isolation == "shared" or family == "push_contention"


def prepare(work, repo, names=("trunk",)):
    """The bare remote WORK/origin.git holding REPO's `main`, whose reflog
    times each update of it, and a clone of it on `main` under WORK for each
    of NAMES: the one checkout every session works in, or one per session.
    Returns the clones."""
    origin = work / "origin.git"
    git(work, "clone", "-q", "--bare", "--no-hardlinks", str(repo), str(origin))
    git(origin, "config", "core.logAllRefUpdates", "always")
    for name in names:
        git(work, "clone", "-q", str(origin), str(work / name))
        identify(work / name)
    return [work / name for name in names]


def landing_times(origin):
    """When each push updated `main` in the bare remote ORIGIN, in whole
    seconds since the epoch, from its reflog; none when it has no reflog."""
    try:
        lines = (origin / "logs" / "refs" / "heads" / "main").read_text(encoding="utf-8").splitlines()
    except FileNotFoundError:
        return []
    found = [re.search(r"> (\d+) [+-]\d{4}\t", line) for line in lines]
    return [int(match.group(1)) for match in found if match]


def blocks(event):
    message = event.get("message") if isinstance(event, dict) else None
    content = message.get("content") if isinstance(message, dict) else None
    return [block for block in content if isinstance(block, dict)] if isinstance(content, list) else []


def result_text(content):
    if isinstance(content, list):
        return "\n".join(str(block.get("text", "")) for block in content if isinstance(block, dict))
    return content if isinstance(content, str) else ""


class ShellSource(io.StringIO):
    """A command's text, read one character at a time by LEXER, with the
    shell's comments removed. shlex's own comment handling differs from the
    shell's twice: it also consumes the newline that ends the command, and
    it starts a comment at `#` inside a word. Here a `#` starts a comment
    only between words (lexer state ' ', or 'c' just after an operator),
    and the comment reads as the newline that ends it."""

    lexer = None

    def read(self, size=-1):
        char = super().read(size)
        if char == "#" and self.lexer.state in (" ", "c"):
            return "\n" if super().readline().endswith("\n") else ""
        return char


def simple_commands(command):
    """COMMAND's simple commands, each as its list of words: the shell's
    quoting and comments applied, split at `;`, `&&`, `||`, `|`, `&`,
    parentheses, redirections and newlines. A comment ends at its newline.
    A command the shell could not parse (an unclosed quote) runs nothing,
    so it has none."""
    source = ShellSource(command)
    lexer = source.lexer = shlex.shlex(source, posix=True, punctuation_chars="();<>|&\n")
    lexer.whitespace, lexer.whitespace_split, lexer.commenters = " \t\r", True, ""
    commands, words = [], []
    try:
        for token in lexer:
            if token and set(token) <= set(lexer.punctuation_chars):
                commands, words = commands + [words], []
            else:
                words.append(token)
    except ValueError:
        return []
    return [words for words in commands + [words] if words]


def runs_git_push(command):
    """Whether the shell COMMAND runs `git push` as one of its simple
    commands, also through `git -C DIR`, an environment prefix or `sh -c`.
    Text that only mentions it, such as a commit message, is not a push."""
    for words in simple_commands(command):
        while words and (words[0] in PREFIXES or ASSIGNMENT.match(words[0])
                         or (words[0].startswith("-") and len(words) > 1)):
            words = words[1:]
        program = words[0].rsplit("/", 1)[-1] if words else ""
        if program in SHELLS and len(words) > 2 and SHELL_COMMAND.fullmatch(words[1]):
            if runs_git_push(words[2]):
                return True
            continue
        if program != "git":
            continue
        rest = words[1:]
        while rest and rest[0].startswith("-"):
            rest = rest[2:] if rest[0] in GIT_OPTION_ARGS else rest[1:]
        if rest[:1] == ["push"]:
            return True
    return False


def push_counts(events):
    """Push attempts and rejections in a transcript's EVENTS: every Bash
    call whose command runs `git push`, a compound command's too, and each
    of those whose result shows Git refusing the push. A retry is an attempt
    made after a rejected one; a push landed when its result is neither a
    rejection nor an error."""
    pushes, counts, rejected = set(), {"attempts": 0, "rejections": 0, "retries": 0, "landed": 0}, False
    for event in events:
        for block in blocks(event):
            tool_input = block.get("input") if isinstance(block.get("input"), dict) else {}
            command = tool_input.get("command")
            if block.get("type") == "tool_use" and block.get("name") == "Bash" and isinstance(command, str) \
                    and runs_git_push(command):
                counts["attempts"] += 1
                counts["retries"] += rejected
                pushes.add(block.get("id"))
            elif block.get("type") == "tool_result" and block.get("tool_use_id") in pushes:
                pushes.discard(block.get("tool_use_id"))
                rejected = REJECTED.search(result_text(block.get("content"))) is not None
                counts["rejections"] += rejected
                counts["landed"] += not rejected and block.get("is_error") is not True
    return counts


def bash(command, cwd):
    """Run COMMAND (no shell) as the stand-in's Bash call, printing the call
    and its result as Claude Code's stream-json does. Returns whether it
    succeeded and its output."""
    call = "bash-" + uuid.uuid4().hex
    print(json.dumps({"type": "assistant", "message": {"content": [
        {"type": "tool_use", "id": call, "name": "Bash", "input": {"command": command}}]}}), flush=True)
    try:
        done = subprocess.run(shlex.split(command), cwd=cwd, capture_output=True, text=True, timeout=GIT_SECONDS)
        ok, output = done.returncode == 0, (done.stdout + done.stderr)[-OUTPUT_BYTES:]
    except subprocess.TimeoutExpired:
        ok, output = False, "timed out after %d s" % GIT_SECONDS
    except OSError as error:
        ok, output = False, "cannot run %s: %s" % (command, error)
    print(json.dumps({"type": "user", "message": {"content": [
        {"type": "tool_result", "tool_use_id": call, "is_error": not ok, "content": output}]}}), flush=True)
    return ok, output


def unlocked(commands, cwd):
    """Run COMMANDS in order as Bash calls, all again while one fails on
    a lock a peer in the same checkout holds. Returns the last result."""
    for _ in range(LOCK_RETRIES):
        for command in commands:
            ok, output = bash(command, cwd)
            if not ok:
                break
        if ok or not LOCKED.search(output):
            break
        time.sleep(LOCK_WAIT_SECONDS)
    return ok, output


def commit_and_push(paths, index, cwd=None, attempts=MAX_PUSH_ATTEMPTS):
    """The stand-in's landing by pushing: commit only PATHS, then push to
    `origin main`, fetching and merging after a rejection, at most ATTEMPTS
    times. Returns whether a push landed."""
    named = " ".join(shlex.quote(str(path)) for path in paths)
    if not unlocked(["git add -- " + named,
                     "git commit -q -m %s -- %s" % (shlex.quote("Session %d" % index), named)], cwd)[0]:
        return False
    for attempt in range(1, attempts + 1):
        if bash("git push origin main", cwd)[0]:
            return True
        if attempt < attempts:
            unlocked(["git pull --no-rebase --no-edit origin main"], cwd)
    return False
