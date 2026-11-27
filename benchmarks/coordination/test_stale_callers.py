"""The stale-caller measure on recorded `discovered_change` runs. In-process:
no run, process, Git repository or model; the recordings are in
testdata/stale-callers."""

import copy
import json
from pathlib import Path
import unittest

import stale_callers

HERE = Path(__file__).resolve().parent
# The change's first write, in langspan-follow.json and worktrees-ignore.json.
CHANGE = {"langspan-follow": 2152108261023458, "worktrees-ignore": 2152102353923250}


def recorded(name):
    return json.loads((HERE / "testdata" / "stale-callers" / (name + ".json")).read_text(encoding="utf-8"))


def measure(trial):
    """The measure of TRIAL, with the fixture's files as the base, as the driver gives it."""
    def base(path):
        file = HERE / "fixture" / path
        return file.read_text(encoding="utf-8") if file.is_file() else None
    transcripts = [(events, (root,)) for events, root in zip(trial["transcripts"], trial["roots"])]
    return stale_callers.stale_callers(trial["plan"], transcripts, base,
                                       lambda index, path: trial["committed"][index - 1].get(path))


def result_of(trial, session, tool_use_id):
    """The tool result block of TOOL_USE_ID in SESSION's transcript."""
    return next(block for event in trial["transcripts"][session - 1] for block in stale_callers.blocks(event)
                if block.get("tool_use_id") == tool_use_id)


class StaleCallers(unittest.TestCase):
    def test_a_stale_write_fixed_after_an_alert_is_timed_from_the_change(self):
        found = measure(recorded("langspan-follow"))
        fixed = (2152111981911375 - CHANGE["langspan-follow"]) / 1e9
        self.assertEqual(found, {
            "status": "reported", "writes_admitted": 1, "stale_files": 1, "unfixed": 0, "fixed_after_s": fixed,
            "items": [
                {"session": 2, "path": "shelf/features/gone.py", "after_s": (2152109438081708 - CHANGE["langspan-follow"]) / 1e9,
                 "kind": "stale_write"},
                {"session": 2, "path": "shelf/features/gone.py", "after_s": fixed, "kind": "fixed"}]})

    def test_a_stale_caller_never_fixed_has_no_time(self):
        found = measure(recorded("worktrees-ignore"))
        self.assertEqual((found["status"], found["writes_admitted"], found["stale_files"], found["unfixed"],
                          found["fixed_after_s"]), ("reported", 1, 1, 1, None))

    def test_a_refused_write_is_not_admitted(self):
        trial = recorded("langspan-follow")
        result_of(trial, 2, "edit-5c8d6a5967804ac8b4ad6a953033c883")["is_error"] = True
        found = measure(trial)
        self.assertEqual((found["status"], found["writes_admitted"], found["stale_files"], found["fixed_after_s"],
                          found["items"]), ("reported", 0, 0, None, []))

    def test_a_caller_written_before_the_change_is_stale_but_not_a_stale_write(self):
        trial = recorded("worktrees-ignore")
        clock = trial["transcripts"][1][3]
        clock["monotonic_ns"] = CHANGE["worktrees-ignore"] - 1000
        found = measure(trial)
        self.assertEqual((found["writes_admitted"], found["stale_files"], found["unfixed"]), (0, 1, 1))
        self.assertEqual(found["items"], [{"session": 2, "path": "shelf/features/gone.py", "after_s": 0.0,
                                           "kind": "stale_at_change"}])

    def test_a_file_that_differs_from_its_replayed_writes_is_unknown(self):
        # A write the transcript does not show, such as one made with Bash.
        trial = recorded("langspan-follow")
        trial["committed"][1]["shelf/features/gone.py"] += "\nlate = days_left\n"
        found = measure(trial)
        self.assertEqual(found["status"], "unknown")
        self.assertIn("shelf/features/gone.py", found["reason"])
        # So is a changed helper the transcript never shows written.
        trial = recorded("worktrees-ignore")
        del trial["transcripts"][0][1:4]
        self.assertEqual(measure(trial)["status"], "unknown")

    def test_an_edit_whose_text_is_missing_is_unknown(self):
        trial = recorded("langspan-follow")
        trial["transcripts"][0][1]["message"]["content"][0]["input"]["old_string"] = "def nothing():"
        self.assertEqual(measure(trial)["status"], "unknown")

    def test_a_write_without_a_clock_line_or_a_transcript_is_unknown(self):
        trial = recorded("langspan-follow")
        del trial["transcripts"][1][3]
        self.assertEqual(measure(trial)["status"], "unknown")
        trial = recorded("langspan-follow")
        trial["transcripts"][1] = None
        self.assertEqual(measure(trial)["status"], "unknown")

    def test_a_write_outside_the_checkout_is_not_a_caller(self):
        trial = recorded("worktrees-ignore")
        call = trial["transcripts"][1][1]["message"]["content"][0]["input"]
        call["file_path"] = "/bench/work/s2/../s1/shelf/features/gone.py"
        # The session's own module was then never written, as committed says.
        trial["committed"][1] = {}
        found = measure(trial)
        self.assertEqual((found["status"], found["writes_admitted"], found["stale_files"]), ("reported", 0, 0))

    def test_no_change_or_another_family_is_not_applicable(self):
        trial = recorded("worktrees-ignore")
        for line in (2, 5, 8):
            trial["transcripts"][0][line]["message"]["content"][0]["is_error"] = True
        trial["committed"][0]["shelf/dates.py"] = (HERE / "fixture" / "shelf" / "dates.py").read_text(encoding="utf-8")
        self.assertEqual(measure(trial)["status"], "not_applicable")
        trial = recorded("worktrees-ignore")
        trial["plan"]["family"] = "rename_vs_new_caller"
        self.assertEqual(measure(trial)["status"], "not_applicable")

    def test_a_caller_that_does_not_parse_names_nothing(self):
        trial = recorded("worktrees-ignore")
        for text in ("x = 1\0\n", "x = " + "-" * 100000 + "1\n"):
            trial["transcripts"][1][1]["message"]["content"][0]["input"]["content"] = text
            trial["committed"][1]["shelf/features/gone.py"] = text
            found = measure(trial)
            self.assertEqual((found["status"], found["stale_files"]), ("reported", 0))

    def test_the_input_is_not_changed(self):
        trial = recorded("langspan-follow")
        before = copy.deepcopy(trial)
        measure(trial)
        self.assertEqual(trial, before)

    def test_the_clock_line_names_each_tool_result(self):
        event = recorded("langspan-follow")["transcripts"][1][2]
        self.assertEqual(stale_callers.clock_line(event, 5), {
            "type": stale_callers.CLOCK_TYPE, "tool_use_ids": ["edit-5c8d6a5967804ac8b4ad6a953033c883"],
            "monotonic_ns": 5})
        self.assertIsNone(stale_callers.clock_line(recorded("langspan-follow")["transcripts"][1][1], 5))
        self.assertIsNone(stale_callers.clock_line({"type": "user", "message": "text"}, 5))


if __name__ == "__main__":
    unittest.main()
