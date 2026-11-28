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


def measure(trial, changed=None):
    """The measure of TRIAL, with the fixture's files as the base, as the
    driver gives it. Each session changed the files it committed, unless
    CHANGED says otherwise."""
    def base(path):
        file = HERE / "fixture" / path
        return file.read_text(encoding="utf-8") if file.is_file() else None
    transcripts = [(events, (root,)) for events, root in zip(trial["transcripts"], trial["roots"])]
    return stale_callers.stale_callers(trial["plan"], transcripts, base,
                                       lambda index, path: trial["committed"][index - 1].get(path),
                                       changed or (lambda index: list(trial["committed"][index - 1])))


def shared(trial):
    """TRIAL as the shared arm runs it: every session in one checkout,
    /bench/work/trunk, which holds every session's committed files."""
    trial = json.loads(json.dumps(trial).replace("/bench/work/s1/", "/bench/work/trunk/")
                       .replace("/bench/work/s2/", "/bench/work/trunk/"))
    trial["roots"] = ["/bench/work/trunk"] * len(trial["roots"])
    tree = {path: text for files in trial["committed"] for path, text in files.items()}
    trial["committed"] = [tree] * len(trial["committed"])
    return trial


def write_call(name, tool, args, ns, path="/bench/work/trunk/shelf/orders.py"):
    """The events of one admitted Write or Edit call and its clock line."""
    return [{"type": "assistant", "message": {"content": [
                {"type": "tool_use", "id": name, "name": tool, "input": dict(args, file_path=path)}]}},
            {"type": "user", "message": {"content": [
                {"type": "tool_result", "tool_use_id": name, "is_error": False, "content": "done"}]}},
            {"type": stale_callers.CLOCK_TYPE, "tool_use_ids": [name], "monotonic_ns": ns}]


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

    def test_a_caller_stale_at_the_change_and_fixed_by_the_next_write_is_counted_and_timed(self):
        trial = recorded("langspan-follow")
        trial["transcripts"][1][3]["monotonic_ns"] = CHANGE["langspan-follow"] - 1000
        found = measure(trial)
        self.assertEqual((found["status"], found["writes_admitted"], found["stale_files"], found["unfixed"],
                          found["fixed_after_s"]),
                         ("reported", 0, 1, 0, (2152111981911375 - CHANGE["langspan-follow"]) / 1e9))
        self.assertEqual([item["kind"] for item in found["items"]], ["stale_at_change", "fixed"])

    def test_a_caller_file_written_another_way_is_unknown(self):
        # A caller Bash added or removed: no Write or Edit call names it.
        for path, text in (("shelf/features/extra.py", "from shelf.dates import days_left\nx = days_left(1)\n"),
                           ("shelf/features/extra.py", "x = 1\n"), ("shelf/report.py", None)):
            trial = recorded("langspan-follow")
            trial["committed"][1][path] = text
            found = measure(trial)
            self.assertEqual(found["status"], "unknown", path)
            self.assertIn(path, found["reason"])
        # A file that cannot hold a caller does not make the trial unknown.
        trial = recorded("langspan-follow")
        trial["committed"][1]["notes/s2.md"] = "done\n"
        self.assertEqual(measure(trial)["status"], "reported")
        # Nor does a list of changed files the driver could not read.
        found = measure(recorded("langspan-follow"), changed=lambda index: None)
        self.assertEqual(found["status"], "unknown")
        self.assertIn("changed files", found["reason"])

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

    def test_a_shared_checkout_is_replayed_from_every_session_s_writes(self):
        # Every session's checkout holds its peers' files too: a peer's
        # recorded write is not a write made another way, and the measure is
        # the one the same writes give in separate checkouts.
        for name in ("langspan-follow", "worktrees-ignore"):
            self.assertEqual(measure(shared(recorded(name))), measure(recorded(name)), name)
        # A session's Edit of a file a peer wrote applies to the peer's text,
        # in clock order; the session's own replay alone cannot make it.
        trial = shared(recorded("langspan-follow"))
        orders = trial["committed"][0]["shelf/orders.py"]
        trial["transcripts"][1][4:4] = write_call("edit-peer", "Edit", {
            "old_string": "datetime.date.today())", "new_string": "datetime.date.today()) "}, 2152110000000000)
        trial["committed"][0]["shelf/orders.py"] = orders.replace("datetime.date.today())",
                                                                  "datetime.date.today()) ")
        found = measure(trial)
        self.assertEqual(found["status"], "reported", found.get("reason"))
        # The same Edit before the peer's write replaces text that is not there yet.
        trial["transcripts"][1][-6]["monotonic_ns"] = CHANGE["langspan-follow"] - 1
        found = measure(trial)
        self.assertEqual(found["status"], "unknown")
        self.assertIn("shelf/orders.py", found["reason"])
        # A write made another way in the shared checkout is still unknown, and
        # the reason names the sessions that share it.
        trial = shared(recorded("langspan-follow"))
        trial["committed"][0]["shelf/features/gone.py"] += "\nlate = days_left\n"
        found = measure(trial)
        self.assertEqual(found["status"], "unknown")
        self.assertIn("sessions 1 and 2", found["reason"])
        self.assertIn("shelf/features/gone.py", found["reason"])

    def test_a_shared_checkout_s_caller_is_fixed_by_whichever_session_writes_it(self):
        # In one checkout a file is one file: session 1's write that leaves
        # session 2's stale caller in the new form fixes it.
        trial = shared(recorded("langspan-follow"))
        fix = trial["transcripts"][1][4:7]
        del trial["transcripts"][1][4:7]
        trial["transcripts"][0][-2:-2] = fix
        found = measure(trial)
        fixed = (2152111981911375 - CHANGE["langspan-follow"]) / 1e9
        self.assertEqual((found["status"], found["writes_admitted"], found["stale_files"], found["unfixed"],
                          found["fixed_after_s"]), ("reported", 1, 1, 0, fixed), found)
        self.assertEqual(found["items"][-1], {"session": 1, "path": "shelf/features/gone.py", "after_s": fixed,
                                              "kind": "fixed"})

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
