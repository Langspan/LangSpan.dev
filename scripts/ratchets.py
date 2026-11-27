#!/usr/bin/env python3
"""Architecture ratchets: file size, function length, module dependencies,
public surface and unchecked indexing of the Rust workspace. See
docs/development-process.md#ratchets.

    python3 -I -B scripts/ratchets.py --check [ROOT]
    python3 -I -B scripts/ratchets.py --tighten FILE... [--root ROOT]
    python3 -I -B scripts/ratchets.py --record FILE... [--root ROOT]

Each measured file may have a baseline, scripts/ratchets/baseline/<file>.tsv,
with tab-separated rows: `lines N`, `pub N`, `index N`, `slice N` (the last
three summed per crate), `fn NAME N` and, in a module's root file of the
`langspan` crate, `edge MODULE`. Only production code is
measured: `#[cfg(test)]` items and files outside `src/` are skipped. The
scanner is lexical, not a Rust parser; it is exact for rustfmt-formatted code.
"""
import os
import re
import sys

MAX_FILE_LINES = 1500
MAX_FN_LINES = 150
BASELINE_DIR = os.path.join("scripts", "ratchets", "baseline")
LAYERS = ("utility", "infrastructure", "application", "interface")
LAYER_MARK = re.compile(r"^//\s*layer:\s*(\S*)\s*$", re.M)
FIX = (
    "split it, or justify the growth in the pull request and run "
    "`python3 -I -B scripts/ratchets.py --record {path}`"
)
TIGHTEN = "run `python3 -I -B scripts/ratchets.py --tighten {path}`"
# Counts each crate may not grow past the sum of its files' rows: {row: (what, fix)}.
COUNTS = {
    "pub": ("pub items", "Make the new items pub(crate) or private"),
    "index": ("index expressions", "Use `.get(i)` and handle the missing element"),
    "slice": ("slice expressions", "Use `.get(a..b)` and handle the missing range"),
}


class RatchetError(Exception):
    """A baseline or source the checker can't read; exit status 2."""


# --- Lexing -----------------------------------------------------------------

_TOKEN = re.compile(r"//|/\*|b?r#*\"|b?\"|b?'")
_RAW = re.compile(r"b?r(#*)\"")
_CHAR = re.compile(r"b?'(?:\\.[^']*|[^\\'])'")


def _blank(text):
    return "".join(c if c == "\n" else " " for c in text)


def strip(src, mark=False):
    """Blank comments, strings and character literals, keeping every offset.

    With mark, a string literal keeps a closing `"`, so an index can see its operand.
    """
    out, i, n = [], 0, len(src)
    while i < n:
        m = _TOKEN.search(src, i)
        if not m:
            out.append(src[i:])
            break
        out.append(src[i : m.start()])
        i, tok = m.start(), m.group()
        if tok == "//":
            j = src.find("\n", i)
            j = n if j < 0 else j
        elif tok == "/*":
            depth, j = 1, i + 2
            while j < n and depth:
                if src.startswith("/*", j):
                    depth, j = depth + 1, j + 2
                elif src.startswith("*/", j):
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
        elif tok.endswith("'"):
            c = _CHAR.match(src, i)
            if not c:  # a lifetime
                out.append(src[i : m.end()])
                i = m.end()
                continue
            j = c.end()
        elif "r" in tok:
            close = '"' + _RAW.match(src, i).group(1)
            j = src.find(close, m.end())
            j = n if j < 0 else j + len(close)
        else:
            j = m.end()
            while j < n and src[j] != '"':
                j += 2 if src[j] == "\\" else 1
            j = min(j + 1, n)
        blank = _blank(src[i:j])
        if mark and tok.endswith('"') and blank.endswith(" "):
            blank = blank[:-1] + '"'
        out.append(blank)
        i = j
    return "".join(out)


def _block_end(s, i):
    """Offset of the `{` or `;` ending the item header at i, outside brackets."""
    depth = 0
    for j in range(i, len(s)):
        c = s[j]
        if c in "([":
            depth += 1
        elif c in ")]":
            depth -= 1
        elif depth == 0 and c in "{;":
            return j
    return None


def _matching_brace(s, i):
    depth = 0
    for j in range(i, len(s)):
        if s[j] == "{":
            depth += 1
        elif s[j] == "}":
            depth -= 1
            if depth == 0:
                return j
    return len(s) - 1


_TEST_ATTR = re.compile(r"^[ \t]*#\[cfg\((?:test|all\(\s*test\b[^\]]*)\)\]", re.M)
_MORE_ATTRS = re.compile(r"(?:\s|#\[[^\]]*\])*")
_ITEM = re.compile(
    r"(?:pub(?:\([^)]*\))?\s+)?(?:(?:const|async|unsafe|extern)\s+)*"
    r"(mod\s+(\w+)\s*;|mod|fn|impl|struct|enum|trait|const|static|type|use|\w+!)"
)
_FN = re.compile(
    r"^[ \t]*(?:pub(?:\([^)]*\))?\s+)?(?:(?:const|async|unsafe|extern)\s+)*fn\s+(\w+)",
    re.M,
)
_PUB = re.compile(
    r"^\s*pub\s+(?:(?:const|async|unsafe|extern)\s+)*"
    r"(?:fn|struct|enum|trait|type|const|static|mod|union)\b"
)
# A `[` right after an operand: rustfmt puts a space or `<`, `&`, `(` before an
# array type, literal or slice pattern, and `!` before a macro's. A `"` ends a
# string literal (see strip's mark).
_INDEX = re.compile(r"(?<=[\w)\]?\"])\[")


class Source:
    """One Rust file reduced to its production code."""

    def __init__(self, text):
        self.stripped = strip(text)
        self.lines = self.stripped.split("\n")
        if text.endswith("\n"):
            self.lines.pop()
        self.test_modules = []  # `#[cfg(test)] mod name;`
        self.excluded = set()
        self._indexing = None
        starts = [0]
        for line in self.lines:
            starts.append(starts[-1] + len(line) + 1)
        self._starts = starts
        s = self.stripped
        for attr in _TEST_ATTR.finditer(s):
            at = _MORE_ATTRS.match(s, attr.end()).end()
            item = _ITEM.match(s, at)
            if not item:
                continue
            if item.group(2):
                self.test_modules.append(item.group(2))
                end = item.end()
            else:
                end = _block_end(s, at)
                if end is None:
                    continue
                if s[end] == "{":
                    end = _matching_brace(s, end)
            self.excluded.update(range(self._line(attr.start()), self._line(end) + 1))
        self.text = self._production(self.lines)
        self._marked = self._production(strip(text, mark=True).split("\n"))

    def _production(self, lines):
        return "\n".join("" if i in self.excluded else line for i, line in enumerate(lines))

    def _line(self, offset):
        """0-based line of a byte offset."""
        return self.stripped.count("\n", 0, offset)

    def production_lines(self):
        return len(self.lines) - len(self.excluded)

    def functions(self):
        """{name: longest production length in lines} for every fn with a body."""
        s, out = self.stripped, {}
        for m in _FN.finditer(s):
            start = self._line(m.start())
            if start in self.excluded:
                continue
            body = _block_end(s, m.end())
            if body is None or s[body] != "{":
                continue
            length = self._line(_matching_brace(s, body)) - start + 1
            out[m.group(1)] = max(out.get(m.group(1), 0), length)
        return out

    def pub_items(self):
        return sum(1 for line in self.text.split("\n") if _PUB.match(line))

    def indexing(self):
        """(index, slice): `x[i]` and `x[a..b]` expressions, which can panic."""
        if self._indexing is not None:
            return self._indexing
        counts, s = [0, 0], self._marked
        for m in _INDEX.finditer(s):
            depth, j = 0, m.end()
            while j < len(s) and (depth or s[j] != "]"):
                depth += {"(": 1, "[": 1, "{": 1, ")": -1, "]": -1, "}": -1}.get(s[j], 0)
                if not depth and s.startswith("..", j):
                    counts[1] += 1
                    break
                j += 1
            else:
                counts[0] += 1
        self._indexing = tuple(counts)
        return self._indexing

    def count(self, metric):
        """The production number a crate-wide ratchet sums: pub, index or slice."""
        if metric == "pub":
            return self.pub_items()
        return self.indexing()[("index", "slice").index(metric)]


# --- The workspace ----------------------------------------------------------


def _crate_name(manifest):
    with open(manifest, encoding="utf-8") as f:
        m = re.search(r'^\[package\][^\[]*?^name\s*=\s*"([^"]+)"', f.read(), re.M | re.S)
    if not m:
        raise RatchetError(f"{manifest}: no [package] name; the checker needs one per crate")
    return m.group(1)


class Workspace:
    """Measured files of ROOT: src/ (the `langspan` crate) and crates/*/src/."""

    def __init__(self, root):
        self.root = root
        self.crate_of = {}  # path -> crate name
        self.sources = {}
        crates = os.path.join(root, "crates")
        dirs = [""] + sorted(
            "crates/" + d
            for d in (os.listdir(crates) if os.path.isdir(crates) else ())
            if os.path.isfile(os.path.join(crates, d, "Cargo.toml"))
        )
        self.root_crate = _crate_name(os.path.join(root, "Cargo.toml"))
        tests = set()
        for d in dirs:
            crate = _crate_name(os.path.join(root, d, "Cargo.toml"))
            src = os.path.join(d, "src")
            for here, subdirs, files in os.walk(os.path.join(root, src)):
                subdirs.sort()
                for f in sorted(files):
                    if not f.endswith(".rs"):
                        continue
                    path = os.path.relpath(os.path.join(here, f), root).replace(os.sep, "/")
                    with open(os.path.join(root, path), encoding="utf-8") as fh:
                        source = Source(fh.read())
                    self.sources[path] = source
                    self.crate_of[path] = crate
                    parent = path[:-3] if f not in ("lib.rs", "main.rs", "mod.rs") else os.path.dirname(path)
                    for name in source.test_modules:
                        tests.update({f"{parent}/{name}.rs", f"{parent}/{name}/mod.rs"})
        for path in tests:
            self.sources.pop(path, None)
            self.crate_of.pop(path, None)
        self._edges = None

    # Modules of the root crate: a top-level file of src/ with its submodules.
    def module_of(self, path):
        if self.crate_of.get(path) != self.root_crate or not path.startswith("src/"):
            return None
        top = path.split("/")[1]
        name = top[:-3] if top.endswith(".rs") else top
        return None if name == "lib" else name

    def module_root(self, module):
        for path in (f"src/{module}.rs", f"src/{module}/mod.rs"):
            if path in self.sources:
                return path
        return None

    def edges(self):
        """{module: set of modules it names}, from `crate::` (and `langspan::` in main)."""
        if self._edges is not None:
            return self._edges
        modules = {m for m in map(self.module_of, self.sources) if m}
        edges = {m: set() for m in modules}
        for path, source in self.sources.items():
            module = self.module_of(path)
            if not module:
                continue
            prefixes = "crate|langspan" if module == "main" else "crate"
            for name in _named_modules(source.text, prefixes):
                if name in modules and name != module:
                    edges[module].add(name)
        self._edges = edges
        return edges

    def layer(self, module):
        path = self.module_root(module)
        with open(os.path.join(self.root, path), encoding="utf-8") as f:
            m = LAYER_MARK.search(f.read())
        if not m:
            return None
        if m.group(1) not in LAYERS:
            raise RatchetError(
                f"{path}: unknown layer {m.group(1)!r} in its `// layer:` marker; "
                f"use one of {', '.join(LAYERS)}"
            )
        return m.group(1)

    def debt(self):
        """{module: {target: reason}} for edges in a cycle or pointing outward."""
        edges = self.edges()
        component = _components(edges)
        layers = {m: self.layer(m) for m in edges}
        out = {}
        for a, targets in edges.items():
            for b in targets:
                if component[a] == component[b]:
                    out.setdefault(a, {})[b] = "cycle"
                elif layers[a] and layers[b] and LAYERS.index(layers[a]) < LAYERS.index(layers[b]):
                    out.setdefault(a, {})[b] = f"outward ({layers[a]} -> {layers[b]})"
        return out


def _named_modules(text, prefixes):
    for m in re.finditer(r"\b(?:%s)::(\w+)" % prefixes, text):
        yield m.group(1)
    for m in re.finditer(r"\b(?:%s)::\{" % prefixes, text):
        i, depth, current = m.end(), 1, ""
        while i < len(text) and depth:
            c = text[i]
            depth += {"{": 1, "}": -1}.get(c, 0)
            if depth == 1 and c == ",":
                yield from re.findall(r"^\s*(\w+)", current)
                current = ""
            elif depth >= 1:
                current += c
            i += 1
        yield from re.findall(r"^\s*(\w+)", current)


def _components(edges):
    """Strongly connected components (iterative Tarjan): {module: component id}."""
    index, low, stack, on, comp = {}, {}, [], set(), {}
    counter = 0
    for root in sorted(edges):
        if root in index:
            continue
        work = [(root, iter(sorted(edges[root])))]
        index[root] = low[root] = counter
        counter += 1
        stack.append(root)
        on.add(root)
        while work:
            v, it = work[-1]
            w = next(it, None)
            if w is not None:
                if w not in index:
                    index[w] = low[w] = counter
                    counter += 1
                    stack.append(w)
                    on.add(w)
                    work.append((w, iter(sorted(edges[w]))))
                elif w in on:
                    low[v] = min(low[v], index[w])
                continue
            work.pop()
            if work:
                low[work[-1][0]] = min(low[work[-1][0]], low[v])
            if low[v] == index[v]:
                while True:
                    w = stack.pop()
                    on.discard(w)
                    comp[w] = v
                    if w == v:
                        break
    return comp


# --- Baselines --------------------------------------------------------------


class Baseline:
    def __init__(self, lines=None, pub=0, fns=None, edges=None, index=0, slice=0):
        self.lines, self.pub, self.index, self.slice = lines, pub, index, slice
        self.fns = dict(fns or {})
        self.edges = set(edges or ())

    def empty(self):
        counts = any(getattr(self, m) for m in COUNTS)
        return self.lines is None and not counts and not self.fns and not self.edges

    def dump(self):
        rows = []
        if self.lines is not None:
            rows.append(f"lines\t{self.lines}")
        rows += [f"{m}\t{getattr(self, m)}" for m in COUNTS if getattr(self, m)]
        rows += [f"fn\t{n}\t{v}" for n, v in sorted(self.fns.items())]
        rows += [f"edge\t{m}" for m in sorted(self.edges)]
        return "".join(r + "\n" for r in rows)


def _baseline_path(root, path):
    return os.path.join(root, BASELINE_DIR, path + ".tsv")


def parse_baseline(text, where):
    b = Baseline()
    for number, line in enumerate(text.split("\n"), 1):
        if not line.strip() or line.startswith("#"):
            continue
        row = line.split("\t")
        try:
            if row[0] in ("lines", *COUNTS) and len(row) == 2:
                setattr(b, row[0], int(row[1]))
            elif row[0] == "fn" and len(row) == 3:
                b.fns[row[1]] = int(row[2])
            elif row[0] == "edge" and len(row) == 2:
                b.edges.add(row[1])
            else:
                raise ValueError
        except ValueError:
            raise RatchetError(
                f"{where}:{number}: expected `lines N`, `pub N`, `index N`, `slice N`, "
                f"`fn NAME N` or `edge MODULE`, tab-separated; got {line!r}"
            ) from None
    return b


def load_baselines(root):
    base, out = os.path.join(root, BASELINE_DIR), {}
    for here, _, files in os.walk(base):
        for f in files:
            if f.endswith(".tsv"):
                full = os.path.join(here, f)
                path = os.path.relpath(full, base)[: -len(".tsv")].replace(os.sep, "/")
                with open(full, encoding="utf-8") as fh:
                    out[path] = parse_baseline(fh.read(), os.path.relpath(full, root))
    return out


def save_baseline(root, path, baseline):
    target = _baseline_path(root, path)
    if baseline.empty():
        if os.path.exists(target):
            os.remove(target)
        return
    os.makedirs(os.path.dirname(target), exist_ok=True)
    with open(target, "w", encoding="utf-8") as f:
        f.write(baseline.dump())


# --- The check --------------------------------------------------------------


def _totals(ws, baselines, metric):
    """{crate: (actual, allowed)}: a crate's allowance is the sum of its files'."""
    totals = {}
    for path, source in ws.sources.items():
        actual, allowed = totals.get(ws.crate_of[path], (0, 0))
        b = baselines.get(path)
        totals[ws.crate_of[path]] = (actual + source.count(metric), allowed + (getattr(b, metric) if b else 0))
    return totals


def check(root):
    """(failures, improvements), each a list of messages."""
    ws, baselines = Workspace(root), load_baselines(root)
    failures, improvements = [], []
    for path, source in ws.sources.items():
        b = baselines.get(path) or Baseline()
        fix, tighten = FIX.format(path=path), TIGHTEN.format(path=path)
        lines = source.production_lines()
        limit = b.lines if b.lines is not None else MAX_FILE_LINES
        if lines > limit:
            what = f"baseline {b.lines}" if b.lines is not None else f"the {MAX_FILE_LINES}-line limit"
            failures.append(f"{path}: {lines} production lines, over {what} by {lines - limit}; {fix}")
        elif b.lines is not None and lines < b.lines:
            improvements.append(f"{path}: {lines} production lines, baseline {b.lines}; {tighten}")
        fns = source.functions()
        for name, length in sorted(fns.items()):
            limit = max(b.fns.get(name, 0), MAX_FN_LINES)
            if length > limit:
                what = f"baseline {b.fns[name]}" if name in b.fns else f"the {MAX_FN_LINES}-line limit"
                failures.append(f"{path}: fn {name} is {length} lines, over {what} by {length - limit}; {fix}")
        for name, base in sorted(b.fns.items()):
            if fns.get(name, 0) < base:
                improvements.append(f"{path}: fn {name} is {fns.get(name, 0)} lines, baseline {base}; {tighten}")
    for metric, (what, how) in COUNTS.items():
        base = {p: getattr(baselines[p], metric) if p in baselines else 0 for p in ws.sources}
        for crate, (actual, allowed) in sorted(_totals(ws, baselines, metric).items()):
            grown = [
                f"{p} {s.count(metric)} (baseline {base[p]})"
                for p, s in sorted(ws.sources.items())
                if ws.crate_of[p] == crate and s.count(metric) > base[p]
            ]
            if actual > allowed:
                failures.append(
                    f"crate {crate}: {actual} {what}, baseline {allowed}, {actual - allowed} more; "
                    f"grown: {', '.join(grown)}. {how}, or justify "
                    "them in the pull request and run `python3 -I -B scripts/ratchets.py --record FILE` "
                    "for each grown file"
                )
            elif actual < allowed:
                improvements.append(
                    f"crate {crate}: {actual} {what}, baseline {allowed}; run "
                    f"`python3 -I -B scripts/ratchets.py --tighten FILE` for each file that lost {what}"
                )
    debt = ws.debt()
    for module in sorted(ws.edges()):
        path = ws.module_root(module)
        allowed = set().union(*(baselines[p].edges for p in ws.sources if ws.module_of(p) == module and p in baselines))
        for target, reason in sorted(debt.get(module, {}).items()):
            if target not in allowed:
                failures.append(
                    f"{path}: module {module} now depends on {target}, a new {reason} edge. "
                    f"Move the shared item to a lower module, or justify the edge in the pull "
                    f"request and run `python3 -I -B scripts/ratchets.py --record {path}`"
                )
        for target in sorted(allowed - set(debt.get(module, {}))):
            improvements.append(f"{path}: edge {module} -> {target} is no longer a cycle or outward; "
                                + TIGHTEN.format(path=path))
    for path in sorted(set(baselines) - set(ws.sources)):
        improvements.append(f"{path}: no longer measured; {TIGHTEN.format(path=path)} to delete its baseline")
    return failures, improvements


def tighten(root, path):
    """Lower PATH's baseline to its current numbers, never raising one."""
    ws, baselines = Workspace(root), load_baselines(root)
    b = baselines.get(path)
    if path not in ws.sources:
        save_baseline(root, path, Baseline())
        return
    if b is None:
        return
    source = ws.sources[path]
    lines = source.production_lines()
    if b.lines is not None and lines < b.lines:
        b.lines = lines if lines > MAX_FILE_LINES else None
    fns = source.functions()
    for name, base in list(b.fns.items()):
        b.fns[name] = min(base, fns.get(name, 0))
        if b.fns[name] <= MAX_FN_LINES:
            del b.fns[name]
    for metric in COUNTS:
        actual, allowed = _totals(ws, baselines, metric)[ws.crate_of[path]]
        mine = getattr(b, metric)
        setattr(b, metric, mine - max(0, min(mine - source.count(metric), allowed - actual)))
    module = ws.module_of(path)
    b.edges &= set(ws.debt().get(module, {})) if module else set()
    save_baseline(root, path, b)


def record(root, path):
    """Set PATH's baseline to its current numbers, raising any that grew."""
    ws = Workspace(root)
    if path not in ws.sources:
        raise RatchetError(f"{path}: not a measured source file; give a path under src/ or crates/*/src/")
    source = ws.sources[path]
    lines = source.production_lines()
    module = ws.module_of(path)
    save_baseline(root, path, Baseline(
        lines=lines if lines > MAX_FILE_LINES else None,
        pub=source.pub_items(),
        index=source.count("index"),
        slice=source.count("slice"),
        fns={n: v for n, v in source.functions().items() if v > MAX_FN_LINES},
        edges=set(ws.debt().get(module, {})) if module and ws.module_root(module) == path else (),
    ))


USAGE = __doc__.split("\n\n")[1]


def main(argv):
    if len(argv) < 2 or argv[1] not in ("--check", "--tighten", "--record"):
        print(f"usage:\n{USAGE}", file=sys.stderr)
        return 2
    args, root = argv[2:], "."
    if "--root" in args:
        at = args.index("--root")
        if at + 1 >= len(args):
            print("--root needs a directory", file=sys.stderr)
            return 2
        root = args[at + 1]
        del args[at : at + 2]
    try:
        if argv[1] == "--check":
            if len(args) > 1:
                print(f"usage:\n{USAGE}", file=sys.stderr)
                return 2
            failures, improvements = check(args[0] if args else root)
            for line in improvements:
                print(f"improved: {line}")
            for line in failures:
                print(f"FAILED: {line}")
            print(f"ratchets: {len(failures)} failed, {len(improvements)} can be tightened")
            return 1 if failures else 0
        if not args:
            print(f"{argv[1]} needs at least one FILE", file=sys.stderr)
            return 2
        for path in args:
            (tighten if argv[1] == "--tighten" else record)(root, path.replace(os.sep, "/"))
        return 0
    except (RatchetError, OSError, UnicodeDecodeError) as e:
        print(f"ratchets: {e}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
