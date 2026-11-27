"""Give each CI test shard an even share of the measured test time (#486).

nextest's own partitions (hash, count, slice) split by name or position and
ignore how long a test takes, so one shard could get several of the slowest
tests. This script assigns them from the times in scripts/ci-test-times.tsv
and prints a nextest filterset for one shard.

  record --platform P JUNIT...  Replace platform P's rows of the times file
                                with the times in full runs' JUnit reports
                                (one per shard); a test in several runs gets
                                its mean. Record from runs with the
                                benchmark dry runs on.
  assign [--platform P] --shard K/N LIST_JSON
                                Print shard K's expected test count, then its
                                filterset, for `cargo nextest list
                                --message-format json` output LIST_JSON.
                                The platform defaults to this machine's.
  count LIST_JSON               Print how many tests LIST_JSON selects.
  balance [--platform P] --shards N
                                Print each shard's expected seconds, from the
                                times file alone.

A unit is one test the times file names, or the rest of a binary's tests.
Units go longest first to the shard that would then finish first; ties go
to the lower shard, so every shard computes the same assignment. Each listed
test is in exactly one unit, so the shards together run every test once.
A test or binary the file doesn't name still runs: it goes with the rest of
its binary, or as a binary of its own weighed at the platform's mean time
of the tests not named singly.

Options: --times FILE (default scripts/ci-test-times.tsv); --skip-dry-runs,
when LANGSPAN_BENCH_DRY_RUNS=skip makes the benchmark dry runs return at
once; --threads N, the tests one shard runs at once (default: this
machine's CPUs).
"""

import argparse
import json
import os
import re
import sys
import xml.etree.ElementTree as ET

TIMES = os.path.join(os.path.dirname(os.path.abspath(__file__)), "ci-test-times.tsv")
PLATFORMS = ("linux", "macos")
# A test that took this long on its platform gets a row of its own.
SINGLE_SECONDS = 2.0
REST = "*"
DRY_RUN_BINARY = "langspan::benchmark_harness"
DRY_RUN_PREFIX = "dry_run_"
MAX_TIMES_BYTES = 1 << 20
MAX_JUNIT_BYTES = 32 << 20
MAX_LIST_BYTES = 64 << 20
# Every name goes into a filterset unquoted, so it may not hold a character
# the filterset grammar reads (a parenthesis, comma, slash-delimited regex).
NAME = re.compile(r"^[A-Za-z0-9_:./-]+$")
HEADER = """\
# Measured test time per CI platform, read by scripts/ci-shards.py to
# balance the full lane's shards. Columns: platform, nextest binary id,
# test name (* for the binary's other tests), seconds, tests. Rewrite a
# platform's rows from full runs' JUnit reports, dry runs on (a test in
# several runs gets its mean):
#   python3 -I -B scripts/ci-shards.py record --platform linux run-*/junit-linux-*/junit.xml
"""


class Refused(Exception):
    pass


def read_bounded(path, limit):
    try:
        size = os.path.getsize(path)
    except OSError as e:
        raise Refused(f"can't read {path}: {e.strerror}")
    if size > limit:
        raise Refused(f"{path} is {size} bytes, over the {limit}-byte bound")
    with open(path, "rb") as f:
        return f.read(limit + 1)


def checked(name, what):
    if not NAME.match(name):
        raise Refused(f"the {what} {name!r} has a character a nextest filterset can't hold unquoted")
    return name


def read_times(path, platform=None):
    """{platform: {binary: {test or REST: (seconds, tests)}}}, or one
    platform's part, which must not be empty."""
    times = {p: {} for p in PLATFORMS}
    if platform is None and not os.path.exists(path):
        return times
    text = read_bounded(path, MAX_TIMES_BYTES).decode("utf-8")
    for number, line in enumerate(text.splitlines(), 1):
        if not line or line.startswith("#"):
            continue
        fields = line.split("\t")
        try:
            on, binary, test, seconds, tests = fields
            row = (float(seconds), int(tests))
        except ValueError:
            raise Refused(f"{path}:{number}: expected platform, binary, test, seconds and tests separated by tabs")
        if on not in times:
            raise Refused(f"{path}:{number}: unknown platform {on!r}; expected one of {', '.join(PLATFORMS)}")
        times[on].setdefault(binary, {})[test] = row
    if platform is None:
        return times
    if not times[platform]:
        raise Refused(f"{path} has no {platform} times; record them from a full run's JUnit reports (see its header)")
    return times[platform]


def record(args):
    times = read_times(args.times)
    # Each test's times over every report, so several runs' reports give
    # each test its mean.
    seen = {}
    for path in args.junit:
        root = ET.fromstring(read_bounded(path, MAX_JUNIT_BYTES))
        for suite in root.iter("testsuite"):
            binary = checked(suite.get("name", ""), "binary id")
            for case in suite.iter("testcase"):
                test = checked(case.get("name", ""), "test name")
                seen.setdefault(binary, {}).setdefault(test, []).append(float(case.get("time", "0")))
    if not seen:
        raise Refused("the JUnit reports hold no tests")
    binaries = {}
    for binary, tests in seen.items():
        rows = binaries[binary] = {}
        for test, runs in tests.items():
            seconds = sum(runs) / len(runs)
            # To the millisecond JUnit gives, so a mean isn't split by float error.
            if round(seconds, 3) >= SINGLE_SECONDS:
                rows[test] = (seconds, 1)
            else:
                total, count = rows.get(REST, (0.0, 0))
                rows[REST] = (total + seconds, count + 1)
    times[args.platform] = binaries
    lines = [HEADER]
    for platform in PLATFORMS:
        for binary in sorted(times[platform]):
            for test, (seconds, tests) in sorted(times[platform][binary].items()):
                lines.append(f"{platform}\t{binary}\t{test}\t{seconds:.1f}\t{tests}\n")
    with open(args.times, "w", encoding="utf-8") as f:
        f.writelines(lines)


def listed(path):
    """{binary: [test, ...]}, the tests nextest would run, from its list JSON."""
    try:
        suites = json.loads(read_bounded(path, MAX_LIST_BYTES))["rust-suites"]
        tests = {}
        for binary, suite in suites.items():
            names = [t for t, case in suite["testcases"].items() if case["filter-match"]["status"] == "matches"]
            if names:
                tests[checked(binary, "binary id")] = sorted(checked(t, "test name") for t in names)
        return tests
    except (ValueError, KeyError, TypeError, AttributeError) as e:
        raise Refused(f"{path} is not `cargo nextest list --message-format json` output: {e!r}")


def weigh(rows, test, skip_dry_runs, binary):
    if skip_dry_runs and binary == DRY_RUN_BINARY and test.startswith(DRY_RUN_PREFIX):
        return 0.0
    return rows[test][0]


def units_of(times, tests, skip_dry_runs):
    """[(seconds, longest single test, filterset, tests)] covering every listed test once."""
    rests = [row for rows in times.values() for name, row in rows.items() if name == REST]
    mean = sum(s for s, _ in rests) / max(1, sum(n for _, n in rests))
    units = []
    for binary, names in tests.items():
        rows = times.get(binary, {})
        single = [t for t in names if t != REST and t in rows]
        for test in single:
            seconds = weigh(rows, test, skip_dry_runs, binary)
            units.append((seconds, seconds, f"(binary_id(={binary}) & test(={test}))", 1))
        rest = len(names) - len(single)
        if not rest:
            continue
        total, recorded = rows.get(REST, (mean * rest, rest))
        seconds = total * rest / max(1, recorded)
        if single:
            others = " | ".join(f"test(={t})" for t in single)
            expr = f"(binary_id(={binary}) & not ({others}))"
        else:
            expr = f"binary_id(={binary})"
        units.append((seconds, seconds / rest, expr, rest))
    return units


def assign(units, shards, threads):
    """Longest first, each to the shard that would then finish first, or of
    those the one with the least work (an empty shard before a full one)."""
    loads = [{"work": 0.0, "longest": 0.0, "units": []} for _ in range(shards)]

    def finish(load, unit):
        return max((load["work"] + unit[0]) / threads, load["longest"], unit[1])

    for unit in sorted(units, key=lambda u: (-u[0], u[2])):
        best = min(range(shards), key=lambda k: (finish(loads[k], unit), loads[k]["work"], k))
        load = loads[best]
        load["work"] += unit[0]
        load["longest"] = max(load["longest"], unit[1])
        load["units"].append(unit)
    for load in loads:
        load["seconds"] = max(load["work"] / threads, load["longest"])
    return loads


def shard_spec(text):
    match = re.fullmatch(r"([1-9][0-9]*)/([1-9][0-9]*)", text)
    if not match or int(match.group(1)) > int(match.group(2)):
        raise Refused(f"--shard is {text!r}; expected K/N with 1 <= K <= N")
    return int(match.group(1)), int(match.group(2))


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--times", default=TIMES)
    sub = parser.add_subparsers(dest="command", required=True)
    rec = sub.add_parser("record")
    rec.add_argument("--platform", choices=PLATFORMS, required=True)
    rec.add_argument("junit", nargs="+")
    here = {"linux": "linux", "darwin": "macos"}.get(sys.platform)
    for name in ("assign", "balance"):
        p = sub.add_parser(name)
        p.add_argument("--platform", choices=PLATFORMS, default=here, required=here is None)
        p.add_argument("--skip-dry-runs", action="store_true")
        p.add_argument("--threads", type=int, default=os.cpu_count() or 1)
    sub.choices["assign"].add_argument("--shard", required=True)
    sub.choices["assign"].add_argument("list")
    sub.choices["balance"].add_argument("--shards", type=int, required=True)
    sub.add_parser("count").add_argument("list")
    args = parser.parse_args()
    try:
        if args.command == "record":
            record(args)
        elif args.command == "count":
            print(sum(len(names) for names in listed(args.list).values()))
        elif args.command == "assign":
            shard, shards = shard_spec(args.shard)
            tests = listed(args.list)
            times = read_times(args.times, args.platform)
            mine = assign(units_of(times, tests, args.skip_dry_runs), shards, max(1, args.threads))[shard - 1]
            print(sum(u[3] for u in mine["units"]))
            print(" | ".join(sorted(u[2] for u in mine["units"])) or "none()")
        else:
            times = read_times(args.times, args.platform)
            units = []
            for binary, rows in times.items():
                for test, (_, n) in rows.items():
                    weight = weigh(rows, test, args.skip_dry_runs, binary)
                    units.append((weight, weight / n, f"{binary} {test}", n))
            for k, load in enumerate(assign(units, max(1, args.shards), max(1, args.threads)), 1):
                print(f"{k}\t{load['seconds']:.1f}")
    except Refused as e:
        print(f"ci-shards.py: {e}", file=sys.stderr)
        sys.exit(2)


if __name__ == "__main__":
    main()
