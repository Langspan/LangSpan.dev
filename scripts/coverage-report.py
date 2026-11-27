"""The per-file coverage table and floor of the nightly coverage job.

Reads the two line exports scripts/coverage.sh writes into DIR:
production.lcov, over the integration-test binaries and the langspan
binary, whose libraries have no #[cfg(test)] code; and all.lcov, over every
test binary. Production lines are the lines production.lcov measures under
src/ and crates/*/src/, so in-file test modules count as neither covered nor
uncovered. A production line is covered when either export counts it.

The floor: production lines of the whole workspace 90% covered or more, and
none of the ten files with the most production lines more than 2 points
under its row in ROOT/scripts/coverage-baseline.tsv. A file among them
with no row is named, not failed.

Prints the summary and table as Markdown on stdout; each failure goes to
stderr too, and the exit status is 1. With --write-baseline it rewrites the
baseline with one row per measured file instead of checking it.

Usage: coverage-report.py [--write-baseline] ROOT DIR
"""

import os
import re
import sys

FLOOR = 90
LARGEST = 10
MARGIN = 2.0
BASELINE = os.path.join("scripts", "coverage-baseline.tsv")
PRODUCTION = re.compile(r"(src|crates/[^/]+/src)/")


def lines(path, root):
    """{relative path: {line: count}} for the production files in an lcov export."""
    files, current = {}, None
    with open(path, encoding="utf-8") as f:
        for line in f:
            if line.startswith("SF:"):
                source = os.path.realpath(line[3:].strip())
                relative = os.path.relpath(source, root)
                current = files.setdefault(relative, {}) if PRODUCTION.match(relative) else None
            elif line.startswith("DA:") and current is not None:
                number, count = line[3:].split(",")[:2]
                number = int(number)
                current[number] = max(current.get(number, 0), int(count))
    return files


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
    write = "--write-baseline" in argv
    rest = [a for a in argv if a != "--write-baseline"]
    if len(rest) != 2:
        print("usage: coverage-report.py [--write-baseline] ROOT DIR", file=sys.stderr)
        return 2
    root, out = os.path.realpath(rest[0]), rest[1]
    production = lines(os.path.join(out, "production.lcov"), root)
    every = lines(os.path.join(out, "all.lcov"), root)
    measured = {}
    for file, counts in production.items():
        unit = every.get(file, {})
        covered = sum(1 for n, c in counts.items() if c > 0 or unit.get(n, 0) > 0)
        measured[file] = (len(counts), covered)
    if not measured:
        print("coverage-report.py: no production lines in %s; did the tests run?" % out, file=sys.stderr)
        return 1
    baseline_path = os.path.join(root, BASELINE)
    current = {file: tenths(c, t) for file, (t, c) in measured.items()}
    if write:
        write_baseline(baseline_path, current)
    baseline = read_baseline(baseline_path)

    total = sum(t for t, _ in measured.values())
    covered = sum(c for _, c in measured.values())
    failures, notes = [], []
    if covered * 100 < FLOOR * total:
        failures.append(
            "the total, %s%% of production lines, is under the %d%% floor"
            % (shown(tenths(covered, total)), FLOOR)
        )
    largest = sorted(measured, key=lambda f: (-measured[f][0], f))[:LARGEST]
    for file in largest:
        if file not in baseline:
            notes.append("`%s` has no baseline row; add one (docs/development-process.md#coverage)" % file)
        elif baseline[file] - current[file] > MARGIN * 10:
            failures.append(
                "`%s` covers %s%% of its production lines, more than %d points under "
                "its baseline of %s%%" % (file, shown(current[file]), MARGIN, shown(baseline[file]))
            )

    print("## Coverage\n")
    print(
        "Production lines covered: %s%% (%d of %d); the floor is %d%%."
        % (shown(tenths(covered, total)), covered, total, FLOOR)
    )
    print(
        "The %d largest files may fall at most %d points under their baseline row.\n"
        % (LARGEST, MARGIN)
    )
    for failure in failures:
        print("- **Below the floor:** " + failure)
        print("coverage-report.py: " + failure, file=sys.stderr)
    for note in notes:
        print("- " + note)
    if failures or notes:
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
