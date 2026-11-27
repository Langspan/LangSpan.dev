"""The per-file coverage table and floor of the nightly coverage job.

Reads the two line exports scripts/coverage.sh writes into DIR:
production.lcov, over the integration-test binaries and the langspan
binary; and all.lcov, over every test binary. Production lines are the
lines production.lcov measures under src/ and crates/*/src/, so in-file
test modules count as neither covered nor uncovered. A production line is
covered when either export counts it.

With --prepare, run in the checkout that built the binaries, it rewrites
both exports instead. Source paths under ROOT become relative to it, so an
artifact from another checkout reads the same. production.lcov loses the
lines of #[cfg(test)] and #[cfg(all(test, ...))] items, which an
integration test compiles when it includes a source with #[path]. With
comments and string and char literals set aside, an item runs from its
attribute to its first ";" or "," outside brackets, or to the "}" that
closes its first top-level "{". The report counts only relative paths, and
it names exports that were never prepared.

The floor: production lines of the whole workspace 90% covered or more, and
no file more than 2 points under its row in ROOT/scripts/coverage-baseline.tsv.
Every row is checked. A row whose file no longer exists fails, so a split or
move carries its rows to the new files; so does a row whose file measures no
production lines. A file with 100 production lines or more and no row fails.
Under 100 lines, one line is more than one point, so a 2-point margin is a
line or two of noise; those files may still have a row.

Prints the summary and table as Markdown on stdout; each failure goes to
stderr too, and the exit status is 1. With --write-baseline it rewrites the
baseline with one row per measured file of 100 production lines or more
instead of checking it.

Usage: coverage-report.py [--prepare | --write-baseline] ROOT DIR
"""

import os
import re
import sys

FLOOR = 90
NEEDS_ROW = 100
MARGIN = 2.0
BASELINE = os.path.join("scripts", "coverage-baseline.tsv")
PRODUCTION = re.compile(r"(src|crates/[^/]+/src)/")
TEST_ONLY = re.compile(r"#\[cfg\((all\()?test\b")
EXPORTS = ("production.lcov", "all.lcov")


def lines(path):
    """({relative path: {line: count}} for the production files in a prepared
    lcov export, the number of sources it names by absolute path)."""
    files, current, outside = {}, None, 0
    with open(path, encoding="utf-8") as f:
        for line in f:
            if line.startswith("SF:"):
                source = line[3:].strip()
                if os.path.isabs(source):
                    outside += 1
                    current = None
                    continue
                relative = os.path.normpath(source)
                current = files.setdefault(relative, {}) if PRODUCTION.match(relative) else None
            elif line.startswith("DA:") and current is not None:
                number, count = line[3:].split(",")[:2]
                number = int(number)
                current[number] = max(current.get(number, 0), int(count))
    return files, outside


def ident(c):
    return c.isalnum() or c == "_"


def code_only(text):
    """text with its comments and string and char literals blanked, lines kept."""
    out, i, n = list(text), 0, len(text)

    def blank(start, end):
        for k in range(start, min(end, n)):
            if out[k] != "\n":
                out[k] = " "

    while i < n:
        c = text[i]
        if text.startswith("//", i):
            end = text.find("\n", i)
            end = n if end < 0 else end
        elif text.startswith("/*", i):
            end, depth = i + 2, 1
            while end < n and depth:
                if text.startswith("/*", end):
                    depth, end = depth + 1, end + 2
                elif text.startswith("*/", end):
                    depth, end = depth - 1, end + 2
                else:
                    end += 1
        elif c == "r" and (i == 0 or not ident(text[i - 1]) or (text[i - 1] == "b" and (i < 2 or not ident(text[i - 2])))):
            hashes = len(text[i + 1:]) - len(text[i + 1:].lstrip("#"))
            if not text.startswith('"', i + 1 + hashes):
                i += 1
                continue
            close = text.find('"' + "#" * hashes, i + 2 + hashes)
            end = n if close < 0 else close + 1 + hashes
        elif c == '"':
            end = i + 1
            while end < n and text[end] != '"':
                end += 2 if text[end] == "\\" else 1
            end += 1
        elif c == "'" and i + 1 < n and text[i + 1] == "\\":
            close = text.find("'", i + 3)
            end = n if close < 0 else close + 1
        elif c == "'" and i + 2 < n and text[i + 2] == "'":
            end = i + 3
        else:
            i += 1
            continue
        blank(i, end)
        i = end
    return "".join(out)


def test_only(text):
    """The 1-based numbers of the lines of #[cfg(test)] items in a source."""
    code = code_only(text)
    found, after = set(), 0
    for match in TEST_ONLY.finditer(code):
        if match.start() < after:
            continue
        stack, end = [], len(code)
        for q in range(match.start() + 1, len(code)):
            c = code[q]
            if c in "([{":
                stack.append(c)
            elif c in ")]}":
                if not stack:
                    end = q - 1  # The enclosing block closes first.
                    break
                opened = stack.pop()
                if c == "}" and opened == "{" and not stack:
                    end = q
                    break
            elif c in ";," and not stack:
                end = q
                break
        first = code.count("\n", 0, match.start()) + 1
        found.update(range(first, code.count("\n", 0, end) + 2))
        after = end
    return found


def prepare(root, out):
    """Rewrite the exports in OUT: paths relative to ROOT, test-only lines out."""
    for name in EXPORTS:
        path = os.path.join(out, name)
        with open(path, encoding="utf-8") as f:
            records = f.readlines()
        kept, skip, found, hit = [], set(), 0, 0
        for line in records:
            if line.startswith("DA:"):
                number, count = line[3:].split(",")[:2]
                if int(number) in skip:
                    continue
                found, hit = found + 1, hit + (int(count) > 0)
            elif line.startswith("LF:"):
                line = "LF:%d\n" % found
            elif line.startswith("LH:"):
                line = "LH:%d\n" % hit
            elif line.startswith("SF:"):
                found = hit = 0
                source = line[3:].strip()
                if os.path.isabs(source):
                    relative = os.path.relpath(os.path.realpath(source), root)
                    if relative != ".." and not relative.startswith("../"):
                        source = relative
                line, skip = "SF:%s\n" % source, set()
                file = os.path.join(root, source)
                if name == "production.lcov" and not os.path.isabs(source) and os.path.isfile(file):
                    with open(file, encoding="utf-8", errors="replace") as f:
                        skip = test_only(f.read())
            kept.append(line)
        with open(path + ".tmp", "w", encoding="utf-8") as f:
            f.writelines(kept)
        os.replace(path + ".tmp", path)


def tenths(covered, total):
    """The percentage in tenths, rounded: 95.04% is 950."""
    return round(1000 * covered / total) if total else 1000


def shown(value):
    return "%d.%d" % divmod(value, 10)


def read_baseline(path):
    rows = {}
    with open(path, encoding="utf-8") as f:
        for number, line in enumerate(f, 1):
            line = line.rstrip("\n")
            if not line or line.startswith("#"):
                continue
            match = re.fullmatch(r"([^\t]+)\t(\d+)\.(\d)", line)
            if not match:
                raise SystemExit(
                    "coverage-report.py: %s:%d: write each row as PATH, a tab, and a "
                    "percentage with one decimal, such as 91.4" % (path, number)
                )
            rows[match.group(1)] = int(match.group(2)) * 10 + int(match.group(3))
    return rows


def write_baseline(path, rows):
    with open(path, encoding="utf-8") as f:
        header = [line for line in f if line.startswith("#")]
    with open(path, "w", encoding="utf-8") as f:
        f.writelines(header)
        for file in sorted(rows):
            f.write("%s\t%s\n" % (file, shown(rows[file])))


def main(argv):
    write, ready = "--write-baseline" in argv, "--prepare" in argv
    rest = [a for a in argv if a not in ("--prepare", "--write-baseline")]
    if len(rest) != 2 or (write and ready):
        print("usage: coverage-report.py [--prepare | --write-baseline] ROOT DIR", file=sys.stderr)
        return 2
    root, out = os.path.realpath(rest[0]), rest[1]
    if ready:
        prepare(root, out)
        return 0
    production, outside = lines(os.path.join(out, "production.lcov"))
    every, _ = lines(os.path.join(out, "all.lcov"))
    measured = {}
    for file, counts in production.items():
        if not counts:
            continue  # Every line test-only: measures no production lines.
        unit = every.get(file, {})
        covered = sum(1 for n, c in counts.items() if c > 0 or unit.get(n, 0) > 0)
        measured[file] = (len(counts), covered)
    if not measured:
        hint = "did the tests run?"
        if outside:
            hint = (
                "it names %d source%s outside the checkout by absolute path; run "
                "coverage-report.py --prepare ROOT DIR in the checkout that built them"
                % (outside, "" if outside == 1 else "s")
            )
        print("coverage-report.py: no production lines in %s; %s" % (out, hint), file=sys.stderr)
        return 1
    baseline_path = os.path.join(root, BASELINE)
    current = {file: tenths(c, t) for file, (t, c) in measured.items()}
    if write:
        write_baseline(baseline_path, {f: v for f, v in current.items() if measured[f][0] >= NEEDS_ROW})
    baseline = read_baseline(baseline_path)

    total = sum(t for t, _ in measured.values())
    covered = sum(c for _, c in measured.values())
    failures = []
    if covered * 100 < FLOOR * total:
        failures.append(
            "the total, %s%% of production lines, is under the %d%% floor"
            % (shown(tenths(covered, total)), FLOOR)
        )
    how = " (docs/development-process.md#coverage)"
    for file in sorted(baseline):
        if file not in measured and not os.path.isfile(os.path.join(root, file)):
            failures.append(
                "`%s` has a baseline row but no longer exists; move its row to the "
                "files its lines went to%s" % (file, how)
            )
        elif file not in measured:
            failures.append("`%s` has a baseline row but no production lines measured; remove the row%s" % (file, how))
        elif baseline[file] - current[file] > MARGIN * 10:
            failures.append(
                "`%s` covers %s%% of its production lines, more than %d points under "
                "its baseline of %s%%" % (file, shown(current[file]), MARGIN, shown(baseline[file]))
            )
    for file in sorted(measured):
        if file not in baseline and measured[file][0] >= NEEDS_ROW:
            failures.append(
                "`%s` has %d production lines and no baseline row; add one%s"
                % (file, measured[file][0], how)
            )

    print("## Coverage\n")
    print(
        "Production lines covered: %s%% (%d of %d); the floor is %d%%."
        % (shown(tenths(covered, total)), covered, total, FLOOR)
    )
    print(
        "No file may fall more than %d points under its baseline row; a file of "
        "%d production lines or more needs one.\n" % (MARGIN, NEEDS_ROW)
    )
    for failure in failures:
        print("- **Fails:** " + failure)
        print("coverage-report.py: " + failure, file=sys.stderr)
    if failures:
        print()
    print("| File | Lines | Covered | % | Baseline |")
    print("| --- | ---: | ---: | ---: | ---: |")
    for file in sorted(measured):
        t, c = measured[file]
        base = shown(baseline[file]) if file in baseline else "none"
        print("| `%s` | %d | %d | %s | %s |" % (file, t, c, shown(current[file]), base))
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
