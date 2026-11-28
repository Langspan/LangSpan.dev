"""Unit tests of scripts/evidence-report.py on recorded `gh` output.

    python3 -I -B -m unittest discover -s scripts/evidence-report -p 'test_*.py'
"""
import contextlib
import importlib.util
import io
import json
import os
import stat
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.join(HERE, "fixtures")
SPEC = importlib.util.spec_from_file_location(
    "evidence_report", os.path.join(os.path.dirname(HERE), "evidence-report.py")
)
report = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(report)


def fixture(name):
    with open(os.path.join(FIXTURES, name), encoding="utf-8") as file:
        return json.load(file)


def recorded():
    issues = [i["number"] for i in fixture("gh-issue-list.json")]
    return report.normalize(fixture("gh-pr-list.json"), issues, "o/r")


def counts(number, people=("alice", "bob")):
    record = recorded()
    pr = next(p for p in record["pulls"] if p["number"] == number)
    return report.touches(pr, set(people), record["needs_owner_issues"])


def run(argv):
    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        status = report.main(["evidence-report.py", *argv])
    return status, out.getvalue(), err.getvalue()


class Touches(unittest.TestCase):
    def test_a_merge_and_a_person_comment_count_but_agent_and_bot_comments_do_not(self):
        self.assertEqual(
            counts(101),
            {"merge": 1, "approval": 0, "review": 0, "comment": 1, "commit": 0, "needs-owner": 0},
        )

    def test_an_approval_a_manual_commit_and_a_closed_needs_owner_issue_count(self):
        # The agent-marked review and the merge of main are not touches.
        self.assertEqual(
            counts(102),
            {"merge": 1, "approval": 1, "review": 0, "comment": 0, "commit": 1, "needs-owner": 1},
        )

    def test_a_bot_merge_and_an_agent_commit_do_not_count_but_a_label_and_a_review_do(self):
        self.assertEqual(
            counts(103),
            {"merge": 0, "approval": 0, "review": 1, "comment": 0, "commit": 0, "needs-owner": 1},
        )

    def test_only_the_given_people_are_counted(self):
        self.assertEqual(sum(counts(102, people=("bob",)).values()), 2)
        self.assertEqual(sum(counts(102, people=("carol",)).values()), 1)  # the label

    def test_only_a_closed_issue_in_the_same_repository_is_matched_against_needs_owner(self):
        def closing(owner, name):
            pr = {"number": 1, "mergedAt": "2026-11-23T07:35:49Z", "closingIssuesReferences": [
                {"number": 90, "repository": {"name": name, "owner": {"login": owner}}}
            ]}
            record = report.normalize([pr], [90], "o/r")
            return record["pulls"][0]["closes"], report.touches(record["pulls"][0], {"alice"}, [90])["needs-owner"]

        self.assertEqual(closing("other", "r"), ([], 0))
        self.assertEqual(closing("o", "other"), ([], 0))
        self.assertEqual(closing("O", "R"), ([90], 1))  # repository names ignore case too

    def test_logins_match_whatever_their_capitalization(self):
        self.assertEqual(counts(101, people=("Alice",)), counts(101, people=("alice",)))
        self.assertEqual(sum(counts(101, people=("ALICE",)).values()), 2)
        record = recorded()
        for pr in record["pulls"]:  # a record GitHub wrote with other capitals
            pr["merged_by"] = pr["merged_by"].upper()
            for item in pr["reviews"] + pr["comments"]:
                item["author"] = item["author"].upper()
            for commit in pr["commits"]:
                commit["authors"] = [a.upper() for a in commit["authors"]]
        self.assertEqual(
            report.render(record, ["Bob", "alice"], "s"), report.render(recorded(), ["bob", "alice"], "s")
        )

    def test_the_recorded_form_keeps_no_text_and_is_in_merge_order(self):
        record = recorded()
        text = json.dumps(record)
        for body in ("@codex review", "Rename it first", "Generated with", "edge case", "example.com"):
            self.assertNotIn(body, text)
        self.assertEqual([p["number"] for p in record["pulls"]], [101, 102, 103])


class Render(unittest.TestCase):
    def test_the_report_matches_its_golden_file(self):
        text = report.render(recorded(), ["bob", "alice"], "recorded file r.json, 3 merged pull requests")
        with open(os.path.join(FIXTURES, "report.md"), encoding="utf-8") as file:
            self.assertEqual(text, file.read())

    def test_iso_weeks_cross_the_year_in_utc(self):
        self.assertEqual(report.week("2026-12-31T23:59:59Z"), "2026-W53")
        self.assertEqual(report.week("2027-01-04T00:00:00Z"), "2027-W01")

    def test_no_merged_change_has_no_ratio(self):
        text = report.render({"version": report.VERSION, "needs_owner_issues": [], "pulls": []}, ["alice"], "s")
        self.assertIn("| all | 0 | 0 | - |", text)


class Command(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)

    def path(self, name):
        return os.path.join(self.dir.name, name)

    def stub_gh(self):
        """A `gh` first on PATH that logs its arguments and answers from the fixtures."""
        stub = self.path("gh")
        with open(stub, "w", encoding="utf-8") as file:
            file.write(
                f"#!{sys.executable}\n"
                "import sys\n"
                f"open({self.path('calls')!r}, 'a').write(' '.join(sys.argv[1:]) + '\\n')\n"
                f"name = {{'pr': 'gh-pr-list.json', 'issue': 'gh-issue-list.json'}}[sys.argv[1]]\n"
                f"sys.stdout.write(open({FIXTURES!r} + '/' + name).read())\n"
            )
        os.chmod(stub, stat.S_IRWXU)
        old = os.environ.get("PATH", "")
        os.environ["PATH"] = self.dir.name + os.pathsep + old
        self.addCleanup(os.environ.__setitem__, "PATH", old)

    def test_a_saved_read_reproduces_the_same_report_without_github(self):
        self.stub_gh()
        status, live, _ = run(["--repo", "o/r", "--person", "alice", "--person", "bob", "--since", "2026-11-01", "--save", self.path("r.json")])
        self.assertEqual(status, 0)
        with open(self.path("calls"), encoding="utf-8") as file:
            calls = file.read().splitlines()
        self.assertEqual(
            calls,
            [
                f"pr list --repo o/r --state merged --limit 500 --search merged:>=2026-11-01 --json {report.PR_FIELDS}",
                "issue list --repo o/r --state all --label needs-owner --limit 1000 --json number",
            ],
        )
        self.assertIn("Source: GitHub o/r, 3 merged pull requests since 2026-11-01.", live)
        status, replay, _ = run(["--from", self.path("r.json"), "--person", "alice", "--person", "bob"])
        self.assertEqual(status, 0)
        self.assertEqual(replay.split("\n", 3)[3], live.split("\n", 3)[3])

    def test_a_read_that_fills_the_limit_says_it_is_the_newest_only(self):
        self.stub_gh()
        status, text, _ = run(["--repo", "o/r", "--person", "alice", "--limit", "3"])
        self.assertEqual(status, 0)
        self.assertIn("(the newest 3 only)", text)

    def test_an_unreadable_or_old_record_says_what_failed(self):
        with open(self.path("old.json"), "w", encoding="utf-8") as file:
            # Version 1 kept other repositories' closed issues in "closes".
            json.dump({"version": 1, "needs_owner_issues": [], "pulls": []}, file)
        for path, says in ((self.path("missing.json"), "cannot read"), (self.path("old.json"), "not a version 2 record")):
            status, out, err = run(["--from", path, "--person", "alice"])
            self.assertEqual((status, out), (2, ""))
            self.assertIn(says, err)

    def test_a_record_of_the_wrong_shape_says_so_instead_of_failing(self):
        for pulls in ("x", [{"number": 1}], [{"number": 1, "merged_at": "May", "merged_by": "", "labels": [], "closes": [], "reviews": [], "comments": [], "commits": []}]):
            with open(self.path("bad.json"), "w", encoding="utf-8") as file:
                json.dump({"version": report.VERSION, "needs_owner_issues": [], "pulls": pulls}, file)
            status, out, err = run(["--from", self.path("bad.json"), "--person", "alice"])
            self.assertEqual((status, out), (2, ""), err)
            self.assertIn("has an unexpected shape", err)

    def test_no_gh_on_path_says_how_to_get_it(self):
        old = os.environ.get("PATH", "")
        os.environ["PATH"] = self.dir.name
        self.addCleanup(os.environ.__setitem__, "PATH", old)
        status, _, err = run(["--repo", "o/r", "--person", "alice"])
        self.assertEqual(status, 2)
        self.assertIn("gh is not on PATH", err)

    def test_a_failing_gh_says_so(self):
        with open(self.path("gh"), "w", encoding="utf-8") as file:
            file.write(f"#!{sys.executable}\nimport sys\nsys.stderr.write('HTTP 404\\n')\nsys.exit(1)\n")
        os.chmod(self.path("gh"), stat.S_IRWXU)
        old = os.environ.get("PATH", "")
        os.environ["PATH"] = self.dir.name + os.pathsep + old
        self.addCleanup(os.environ.__setitem__, "PATH", old)
        status, _, err = run(["--repo", "o/missing", "--person", "alice"])
        self.assertEqual(status, 2)
        self.assertIn("gh pr list failed: HTTP 404", err)

    def test_bad_arguments_are_refused(self):
        for argv in (["--repo", "o/r", "--person", "a", "--limit", "0"], ["--repo", "o/r", "--person", "a", "--since", "May"], ["--repo", "o/r"]):
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as raised:
                report.main(["evidence-report.py", *argv])
            self.assertEqual(raised.exception.code, 2)


if __name__ == "__main__":
    unittest.main()
