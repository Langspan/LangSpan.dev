"""The one list of RustSec advisories LangSpan ignores.

deny.toml's [advisories] ignore owns it. cargo-deny reads it there, and
scripts/check.sh hands cargo-audit the same ids as flags derived here, so
the two tools can't ignore different advisories. Each ignore is one line,

    { id = "RUSTSEC-YYYY-NNNN", reason = "Why. Review by YYYY-MM-DD." },

and the check fails when a reason or its review date is missing, the date
has passed, an id is listed twice, or .cargo/audit.toml keeps a list of its
own. The nightly run therefore fails on the day an ignore is due for review.

Usage: advisory-ignores.py [--audit-flags] [ROOT]
  Checks ROOT/deny.toml (ROOT defaults to the current directory). With
  --audit-flags it then prints cargo-audit's flags, `--ignore ID` per line.
"""

import datetime
import os
import re
import sys

ENTRY = re.compile(
    r'\{\s*id\s*=\s*"(RUSTSEC-\d{4}-\d{4})"\s*,\s*reason\s*=\s*"((?:[^"\\]|\\.)*)"\s*\},?'
)
REVIEW = re.compile(r"Review by (\d{4}-\d{2}-\d{2})")


def ignores(text):
    """The (line number, line) pairs of deny.toml's advisory ignore array."""
    section = None
    inside = False
    for number, line in enumerate(text.splitlines(), 1):
        stripped = line.strip()
        if inside:
            if stripped.startswith("]"):
                return
            if stripped and not stripped.startswith("#"):
                yield number, stripped
        elif stripped.startswith("["):
            section = stripped
        elif section == "[advisories]" and re.match(r"ignore\s*=\s*\[", stripped):
            if stripped.endswith("]"):
                if stripped.replace(" ", "") != "ignore=[]":
                    yield number, stripped
                return
            inside = True


def check(root, today):
    """Returns (ids, errors) for the ignores in ROOT/deny.toml."""
    path = os.path.join(root, "deny.toml")
    try:
        with open(path, encoding="utf-8") as f:
            text = f.read()
    except OSError as e:
        return [], ["can't read %s: %s" % (path, e)]
    ids, errors = [], []
    for number, line in ignores(text):
        entry = ENTRY.fullmatch(line)
        if not entry:
            errors.append(
                "deny.toml:%d: can't read this ignore; write it as "
                '{ id = "RUSTSEC-...", reason = "Why. Review by YYYY-MM-DD." }' % number
            )
            continue
        id, reason = entry.groups()
        review = REVIEW.search(reason)
        if id in ids:
            errors.append("deny.toml:%d: %s is listed twice; keep one entry" % (number, id))
        elif not reason.strip():
            errors.append("deny.toml:%d: %s has no reason; say why it is ignored" % (number, id))
        elif not review:
            errors.append(
                "deny.toml:%d: %s has no review date; end its reason with "
                '"Review by YYYY-MM-DD."' % (number, id)
            )
        elif review.group(1) < today:
            errors.append(
                "deny.toml:%d: %s was due for review on %s; check for a fix, then "
                "upgrade and drop the ignore, or set a new date with the reason "
                "it still stands" % (number, id, review.group(1))
            )
        ids.append(id)
    audit = os.path.join(root, ".cargo", "audit.toml")
    if os.path.exists(audit):
        with open(audit, encoding="utf-8") as f:
            if re.search(r"^\s*ignore\s*=", f.read(), re.M):
                errors.append(
                    ".cargo/audit.toml lists advisories to ignore; remove that list "
                    "and add each ignore to deny.toml, which cargo-audit reads through "
                    "scripts/check.sh"
                )
    return ids, errors


def main(argv):
    flags = "--audit-flags" in argv
    rest = [a for a in argv if a != "--audit-flags"]
    if len(rest) > 1 or any(a.startswith("-") for a in rest):
        print("usage: advisory-ignores.py [--audit-flags] [ROOT]", file=sys.stderr)
        return 2
    root = rest[0] if rest else "."
    ids, errors = check(root, datetime.date.today().isoformat())
    for error in errors:
        print("advisory-ignores.py: " + error, file=sys.stderr)
    if errors:
        return 1
    if flags:
        for id in ids:
            print("--ignore " + id)
    else:
        print(
            "%d advisory ignores in deny.toml, each with a reason and a review date "
            "still ahead; cargo-audit gets the same list" % len(ids)
        )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
