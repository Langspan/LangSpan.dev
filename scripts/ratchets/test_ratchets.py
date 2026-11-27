"""Unit tests of scripts/ratchets.py on small workspaces.

    python3 -I -B -m unittest discover -s scripts/ratchets -p 'test_*.py'
"""
import contextlib
import io
import os
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
import ratchets  # noqa: E402


def fn(name, body_lines, vis="pub "):
    body = "".join("    let _ = 0;\n" for _ in range(body_lines - 2))
    return f"{vis}fn {name}() {{\n{body}}}\n"


class Workspace:
    """A temporary workspace: a `langspan` crate in src/ and a `core` crate."""

    def __init__(self, test):
        self.dir = tempfile.TemporaryDirectory()
        test.addCleanup(self.dir.cleanup)
        self.root = self.dir.name
        self.write("Cargo.toml", '[package]\nname = "langspan"\n')
        self.write("crates/core/Cargo.toml", '[package]\nname = "core"\n')
        self.write("src/lib.rs", "mod a;\nmod b;\n")
        self.write("crates/core/src/lib.rs", "")

    def write(self, path, text):
        full = os.path.join(self.root, path)
        os.makedirs(os.path.dirname(full), exist_ok=True)
        with open(full, "w", encoding="utf-8") as f:
            f.write(text)

    def remove(self, path):
        os.remove(os.path.join(self.root, path))

    def baseline(self, path):
        full = ratchets._baseline_path(self.root, path)
        if not os.path.exists(full):
            return None
        with open(full, encoding="utf-8") as f:
            return f.read()

    def check(self):
        return ratchets.check(self.root)

    def record(self, *paths):
        for path in paths:
            ratchets.record(self.root, path)

    def tighten(self, path):
        ratchets.tighten(self.root, path)


class Lexing(unittest.TestCase):
    def setUp(self):
        with open(os.path.join(HERE, "fixtures", "lexing.rs"), encoding="utf-8") as f:
            self.source = ratchets.Source(f.read())

    def test_test_items_are_not_production_code(self):
        self.assertEqual(self.source.production_lines(), 38 - 11)
        self.assertEqual(self.source.test_modules, ["split_tests"])

    def test_functions_ignore_braces_in_strings_comments_and_chars(self):
        self.assertEqual(self.source.functions(), {"hidden": 1, "strings": 8, "borrowed": 3})

    def test_pub_counts_declared_items_only(self):
        # Counted, LIMIT, strings, borrowed, Last; not `pub use`, `pub(crate)`
        # or an item of a test module.
        self.assertEqual(self.source.pub_items(), 5)


class FileSize(unittest.TestCase):
    def setUp(self):
        self.ws = Workspace(self)

    def test_a_new_file_under_the_limit_needs_no_baseline(self):
        self.ws.write("src/a.rs", "\n" * ratchets.MAX_FILE_LINES)
        self.assertEqual(self.ws.check(), ([], []))

    def test_a_new_file_over_the_limit_fails_with_the_excess_and_the_fix(self):
        self.ws.write("src/a.rs", "\n" * (ratchets.MAX_FILE_LINES + 7))
        failures, _ = self.ws.check()
        self.assertEqual(len(failures), 1)
        self.assertIn("src/a.rs: 1507 production lines, over the 1500-line limit by 7", failures[0])
        self.assertIn("split it", failures[0])
        self.assertIn("--record src/a.rs", failures[0])

    def test_inline_tests_do_not_count(self):
        tests = "#[cfg(test)]\nmod tests {\n" + "\n" * 2000 + "}\n"
        self.ws.write("src/a.rs", "fn a() {}\n" + tests)
        self.assertEqual(self.ws.check(), ([], []))

    def test_a_baselined_file_may_shrink_but_not_grow(self):
        self.ws.write("src/a.rs", "\n" * 2000)
        self.ws.record("src/a.rs")
        self.assertEqual(self.ws.baseline("src/a.rs"), "lines\t2000\n")
        self.ws.write("src/a.rs", "\n" * 2003)
        failures, _ = self.ws.check()
        self.assertIn("2003 production lines, over baseline 2000 by 3", failures[0])
        self.ws.write("src/a.rs", "\n" * 1900)
        failures, improvements = self.ws.check()
        self.assertEqual(failures, [])
        self.assertIn("--tighten src/a.rs", improvements[0])
        self.ws.tighten("src/a.rs")
        self.assertEqual(self.ws.baseline("src/a.rs"), "lines\t1900\n")
        self.assertEqual(self.ws.check(), ([], []))

    def test_tightening_under_the_limit_drops_the_baseline(self):
        self.ws.write("src/a.rs", "\n" * 2000)
        self.ws.record("src/a.rs")
        self.ws.write("src/a.rs", "\n" * 10)
        self.ws.tighten("src/a.rs")
        self.assertIsNone(self.ws.baseline("src/a.rs"))

    def test_tightening_never_raises(self):
        self.ws.write("src/a.rs", "\n" * 2000)
        self.ws.record("src/a.rs")
        self.ws.write("src/a.rs", "\n" * 2100)
        self.ws.tighten("src/a.rs")
        self.assertEqual(self.ws.baseline("src/a.rs"), "lines\t2000\n")

    def test_the_baseline_of_a_deleted_file_is_removed_by_tightening(self):
        self.ws.write("src/a.rs", "\n" * 2000)
        self.ws.record("src/a.rs")
        self.ws.remove("src/a.rs")
        failures, improvements = self.ws.check()
        self.assertEqual(failures, [])
        self.assertIn("no longer measured", improvements[0])
        self.ws.tighten("src/a.rs")
        self.assertIsNone(self.ws.baseline("src/a.rs"))


class FunctionLength(unittest.TestCase):
    def setUp(self):
        self.ws = Workspace(self)

    def test_a_new_long_function_fails(self):
        self.ws.write("src/a.rs", fn("long", 151, vis=""))
        failures, _ = self.ws.check()
        self.assertEqual(len(failures), 1)
        self.assertIn("fn long is 151 lines, over the 150-line limit by 1", failures[0])

    def test_a_long_test_function_is_not_measured(self):
        self.ws.write("src/a.rs", "#[cfg(test)]\nmod tests {\n" + fn("golden", 400) + "}\n")
        self.assertEqual(self.ws.check(), ([], []))

    def test_a_baselined_function_may_shrink_but_not_grow(self):
        self.ws.write("src/a.rs", fn("long", 200, vis=""))
        self.ws.record("src/a.rs")
        self.assertEqual(self.ws.baseline("src/a.rs"), "fn\tlong\t200\n")
        self.ws.write("src/a.rs", fn("long", 205, vis=""))
        self.assertIn("over baseline 200 by 5", self.ws.check()[0][0])
        self.ws.write("src/a.rs", fn("long", 180, vis=""))
        failures, improvements = self.ws.check()
        self.assertEqual(failures, [])
        self.assertIn("fn long is 180 lines, baseline 200", improvements[0])
        self.ws.tighten("src/a.rs")
        self.assertEqual(self.ws.baseline("src/a.rs"), "fn\tlong\t180\n")
        self.ws.write("src/a.rs", "")
        self.ws.tighten("src/a.rs")
        self.assertIsNone(self.ws.baseline("src/a.rs"))


class PublicSurface(unittest.TestCase):
    def setUp(self):
        self.ws = Workspace(self)
        self.ws.write("src/a.rs", "pub fn one() {}\npub struct Two;\n")
        self.ws.write("src/b.rs", "")
        self.ws.record("src/a.rs", "src/b.rs")

    def test_a_new_pub_item_fails_for_its_crate(self):
        self.ws.write("src/b.rs", "pub fn three() {}\n")
        failures, _ = self.ws.check()
        self.assertEqual(len(failures), 1)
        self.assertIn("crate langspan: 3 pub items, baseline 2, 1 more", failures[0])
        self.assertIn("src/b.rs 1 (baseline 0)", failures[0])

    def test_re_exports_and_crate_visibility_are_not_counted(self):
        self.ws.write("src/b.rs", "pub use crate::a::one;\npub(crate) fn three() {}\n")
        self.assertEqual(self.ws.check(), ([], []))

    def test_other_crates_have_their_own_allowance(self):
        self.ws.write("crates/core/src/lib.rs", "pub fn core() {}\n")
        failures, _ = self.ws.check()
        self.assertIn("crate core: 1 pub items, baseline 0", failures[0])

    def test_moving_a_pub_item_between_files_passes_and_keeps_the_allowance(self):
        self.ws.write("src/a.rs", "pub fn one() {}\n")
        self.ws.write("src/b.rs", "pub struct Two;\n")
        self.assertEqual(self.ws.check(), ([], []))
        self.ws.tighten("src/a.rs")
        self.assertEqual(self.ws.check(), ([], []))

    def test_removing_a_pub_item_can_be_tightened(self):
        self.ws.write("src/a.rs", "pub fn one() {}\n")
        failures, improvements = self.ws.check()
        self.assertEqual(failures, [])
        self.assertIn("crate langspan: 1 pub items, baseline 2", improvements[0])
        self.ws.tighten("src/a.rs")
        self.assertEqual(self.ws.baseline("src/a.rs"), "pub\t1\n")


class Indexing(unittest.TestCase):
    def setUp(self):
        self.ws = Workspace(self)
        self.ws.write("src/a.rs", "fn f(v: &[u8]) -> u8 {\n    v[0] + v[1..][0]\n}\n")
        self.ws.write("src/b.rs", "")
        self.ws.record("src/a.rs", "src/b.rs")

    def test_the_fixture_counts_index_and_slice_expressions_only(self):
        with open(os.path.join(HERE, "fixtures", "indexing.rs"), encoding="utf-8") as f:
            self.assertEqual(ratchets.Source(f.read()).indexing(), (6, 3))

    def test_indexing_a_string_literal_counts(self):
        # Each kind of string literal Rust can index, then literals that aren't indexed.
        src = (
            "fn f(i: usize) -> u8 {\n"
            '    let a = b"abc"[i];\n'
            '    let b = &"abc"[..i];\n'
            '    let c = r"abc"[..i].len();\n'
            '    let d = br#"a"b"#[i];\n'
            '    let e = &"a\\"[b"[i..];\n'
            '    let f = ["x"];\n'
            "    let g = 'x';\n"
            "    a + d\n"
            "}\n"
        )
        self.assertEqual(ratchets.Source(src).indexing(), (2, 3))

    def test_marking_literals_for_indexing_leaves_other_measures_alone(self):
        src = 'pub extern "C" fn f() -> u8 {\n    b"x"[0]\n}\n'
        source = ratchets.Source(src)
        self.assertEqual(source.functions(), {"f": 3})
        self.assertEqual(source.pub_items(), 1)
        self.assertEqual(source.indexing(), (1, 0))

    def test_indexing_across_a_comment_counts(self):
        # Each operand kind, then block, nested, doc and line comments before `[`.
        src = (
            "fn f(v: &[u8], i: usize) -> u8 {\n"
            "    let a = v /* element */ [i];\n"
            "    let b = f(v, i) /* a /* b */ */ [..i];\n"
            "    let c = v[0] /** doc */ [1..];\n"
            '    let d = b"ab" /* c */ [i];\n'
            "    let e = v.get(i)? /* c */ /* d */ [0];\n"
            "    let g = v // line\n"
            "        [i];\n"
            "    a + e + g\n"
            "}\n"
        )
        self.assertEqual(ratchets.Source(src).indexing(), (5, 2))

    def test_a_comment_before_an_array_or_slice_pattern_is_not_indexing(self):
        # A keyword or punctuation before the comment starts a literal, type or pattern.
        src = (
            "fn f(v: &mut /* c */ [u8]) -> /* c */ [u8; 2] {\n"
            "    let /* c */ [a, b] = [1, 2];\n"
            "    let x = /* c */ [a, b];\n"
            "    for y in /* c */ [a, b] {}\n"
            "    match /* c */ [a, b] { _ => {} }\n"
            "    if let /* c */ [_] = v {}\n"
            "    return /* c */ [a, b];\n"
            "}\n"
        )
        self.assertEqual(ratchets.Source(src).indexing(), (0, 0))

    def test_marking_a_comment_keeps_its_lines(self):
        marked = ratchets.strip("a /* x\ny */ b // z\n", mark=True)
        self.assertEqual(marked, "a " + "\x01" * 4 + "\n" + "\x01" * 4 + " b " + "\x01" * 4 + "\n")

    def test_marking_an_unterminated_string_keeps_its_lines(self):
        self.assertEqual(ratchets.strip('x = "a\n', mark=True), "x =   \n")

    def test_recording_writes_both_counts(self):
        self.assertEqual(self.ws.baseline("src/a.rs"), "index\t2\nslice\t1\n")
        self.assertEqual(self.ws.check(), ([], []))

    def test_a_new_index_fails_for_its_crate(self):
        self.ws.write("src/b.rs", "fn g(v: &[u8]) -> u8 {\n    v[2]\n}\n")
        failures, _ = self.ws.check()
        self.assertEqual(len(failures), 1)
        self.assertIn("crate langspan: 3 index expressions, baseline 2, 1 more", failures[0])
        self.assertIn("src/b.rs 1 (baseline 0)", failures[0])
        self.assertIn("Use `.get(i)`", failures[0])

    def test_a_new_slice_fails_for_its_crate(self):
        self.ws.write("crates/core/src/lib.rs", "fn g(s: &str) -> &str {\n    &s[1..]\n}\n")
        failures, _ = self.ws.check()
        self.assertEqual(len(failures), 1)
        self.assertIn("crate core: 1 slice expressions, baseline 0, 1 more", failures[0])

    def test_indexing_in_tests_is_not_counted(self):
        self.ws.write("src/b.rs", "#[cfg(test)]\nmod tests {\n    fn t(v: &[u8]) -> u8 {\n        v[0]\n    }\n}\n")
        self.assertEqual(self.ws.check(), ([], []))

    def test_moving_indexing_between_files_passes_and_removing_it_tightens(self):
        self.ws.write("src/a.rs", "")
        self.ws.write("src/b.rs", "fn f(v: &[u8]) -> u8 {\n    v[0] + v[1..][0]\n}\n")
        self.assertEqual(self.ws.check(), ([], []))
        self.ws.write("src/b.rs", "fn f(v: &[u8]) -> u8 {\n    v[0]\n}\n")
        failures, improvements = self.ws.check()
        self.assertEqual(failures, [])
        self.assertIn("crate langspan: 1 index expressions, baseline 2", improvements[0])
        self.assertIn("crate langspan: 0 slice expressions, baseline 1", improvements[1])
        self.ws.tighten("src/a.rs")
        self.assertEqual(self.ws.baseline("src/a.rs"), "index\t1\n")
        self.assertEqual(self.ws.check(), ([], []))


class ModuleDependencies(unittest.TestCase):
    def setUp(self):
        self.ws = Workspace(self)
        self.ws.write("src/a.rs", "use crate::b::Thing;\n")
        self.ws.write("src/b.rs", "pub struct Thing;\n")
        self.ws.record("src/b.rs")

    def test_a_new_cycle_fails_on_every_edge_in_it(self):
        self.ws.write("src/b.rs", "pub struct Thing;\nfn f() { crate::a::g() }\n")
        failures, _ = self.ws.check()
        self.assertEqual(len(failures), 2)
        self.assertIn("module a now depends on b, a new cycle edge", failures[0])
        self.assertIn("module b now depends on a, a new cycle edge", failures[1])
        self.assertIn("--record src/b.rs", failures[1])

    def test_a_baselined_cycle_passes_and_tightens_once_broken(self):
        self.ws.write("src/b.rs", "pub struct Thing;\nuse crate::{c, a::{g}};\n")
        self.ws.write("src/c.rs", "")
        self.ws.record("src/a.rs", "src/b.rs")
        self.assertEqual(self.ws.baseline("src/b.rs"), "pub\t1\nedge\ta\n")
        self.assertEqual(self.ws.check(), ([], []))
        self.ws.write("src/b.rs", "pub struct Thing;\n")
        failures, improvements = self.ws.check()
        self.assertEqual(failures, [])
        self.assertEqual(len(improvements), 2)
        self.ws.tighten("src/b.rs")
        self.assertEqual(self.ws.baseline("src/b.rs"), "pub\t1\n")

    def test_submodule_files_belong_to_their_module(self):
        self.ws.write("src/b/inner.rs", "fn f() { crate::a::g() }\n")
        failures, _ = self.ws.check()
        self.assertIn("src/b.rs: module b now depends on a", failures[1])

    def test_main_names_library_modules_through_the_crate_name(self):
        self.ws.write("src/main.rs", "use langspan::a;\n")
        self.ws.write("src/a.rs", "use crate::main::x;\n")
        failures, _ = self.ws.check()
        self.assertIn("module a now depends on main", failures[0])

    def test_an_outward_edge_between_marked_layers_fails(self):
        self.ws.write("src/a.rs", "// layer: utility\nuse crate::b::Thing;\n")
        self.ws.write("src/b.rs", "// layer: interface\npub struct Thing;\n")
        failures, _ = self.ws.check()
        self.assertEqual(len(failures), 1)
        self.assertIn("a new outward (utility -> interface) edge", failures[0])

    def test_an_inward_edge_or_an_unmarked_module_passes(self):
        self.ws.write("src/a.rs", "// layer: interface\nuse crate::b::Thing;\n")
        self.ws.write("src/b.rs", "// layer: utility\npub struct Thing;\n")
        self.assertEqual(self.ws.check(), ([], []))
        self.ws.write("src/b.rs", "pub struct Thing;\n")
        self.assertEqual(self.ws.check(), ([], []))

    def test_an_unknown_layer_is_an_error(self):
        self.ws.write("src/a.rs", "// layer: domain\n")
        with self.assertRaisesRegex(ratchets.RatchetError, "src/a.rs: unknown layer 'domain'"):
            self.ws.check()


class Interface(unittest.TestCase):
    def setUp(self):
        self.ws = Workspace(self)

    def run_main(self, *args):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = ratchets.main(["ratchets.py", *args])
        return code, out.getvalue(), err.getvalue()

    def test_exit_status_says_whether_the_check_passed(self):
        self.assertEqual(self.run_main("--check", self.ws.root)[0], 0)
        self.ws.write("src/a.rs", fn("long", 151))
        code, out, _ = self.run_main("--check", self.ws.root)
        self.assertEqual(code, 1)
        self.assertIn("FAILED: src/a.rs: fn long", out)
        self.assertIn("ratchets: 2 failed", out)

    def test_a_malformed_baseline_names_the_file_and_line(self):
        self.ws.write("scripts/ratchets/baseline/src/lib.rs.tsv", "lines\t10\nlines 12\n")
        code, _, err = self.run_main("--check", self.ws.root)
        self.assertEqual(code, 2)
        self.assertIn("src/lib.rs.tsv:2: expected", err)

    def test_usage_errors(self):
        self.assertEqual(self.run_main()[0], 2)
        self.assertEqual(self.run_main("--tighten")[0], 2)
        code, _, err = self.run_main("--record", "src/nope.rs", "--root", self.ws.root)
        self.assertEqual(code, 2)
        self.assertIn("src/nope.rs: not a measured source file", err)


if __name__ == "__main__":
    unittest.main()
