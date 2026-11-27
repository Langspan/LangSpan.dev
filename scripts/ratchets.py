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
`langspan` crate, `edge MODULE`. In that crate each namespace of
scripts/ratchets/namespaces.tsv (a top-level module of src/), and each file with
a role, is a node; paths through `crate::`, `super::`, `self::`, child modules
and lib.rs's compatibility aliases name nodes, and an edge row may name a node
or an alias. An edge in a cycle, pointing outward, between namespaces of one
layer without an `allow` row, outside an `only` list or from a pipeline file to
an adapter beside it fails without an edge row; so does a top-level file that
isn't a namespace, a file deeper than src/<namespace>/<sub>/<file>.rs and an
alias nothing uses. Only production code is
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
NAMESPACES = "scripts/ratchets/namespaces.tsv"
ROLES = ("adapter", "feature")
ROLE_MARK = re.compile(r"^//\s*role:\s*(\S*)\s*$", re.M)
MAX_DEPTH = 3  # src/<namespace>/<sub>/<file>.rs
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

    With mark, a string literal keeps a closing `"` and a comment becomes `\\x01`s,
    so an index can see its operand and tell a comment from spacing.
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
        elif mark and tok in ("//", "/*"):
            blank = blank.replace(" ", "\x01")
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
# string literal and a `\x01` a comment (see strip's mark); rustfmt keeps a comment
# between an operand and its `[`, so the operand is matched across one.
_INDEX = re.compile(r"(\w+|[)\]?\"])(?:[\s\x01]*\x01[\s\x01]*)?\[")
# Keywords that start an array, slice type or slice pattern after a comment. A raw
# identifier such as `r#match` is an operand, so `indexing` checks for its `r#`.
_NOT_OPERAND = {
    "break", "const", "for", "if", "in", "let", "match", "mut", "return", "where", "while",
}


def _is_attribute_end(s, i):
    """Whether the `]` at i closes an attribute `#[...]` or `#![...]`."""
    depth = 0
    for j in range(i, -1, -1):
        depth += {"]": 1, "[": -1}.get(s[j], 0)
        if not depth:
            k = j - 1
            while k >= 0 and s[k].isspace():
                k -= 1
            k -= k >= 0 and s[k] == "!"
            return k >= 0 and s[k] == "#"
    return False


def _not_operand(s, m):
    """Whether the word or `]` matched by _INDEX starts what follows, not indexes it."""
    word, at = m.group(1), m.start()
    if word == "]":
        return _is_attribute_end(s, at)
    if s[at - 1 : at] == "'":  # a lifetime or a label; strip blanks character literals
        return True
    return word in _NOT_OPERAND and not (at >= 2 and s.startswith("r#", at - 2))


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
            if _not_operand(s, m):
                continue
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
        self.paths = []  # every .rs file of the root crate's src/, tests included
        self.roles = {}  # "namespace::file" -> adapter or feature
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
                        text = fh.read()
                    source = Source(text)
                    if crate == self.root_crate:
                        self.paths.append(path)
                        self._role_mark(path, text)
                    self.sources[path] = source
                    self.crate_of[path] = crate
                    parent = path[:-3] if f not in ("lib.rs", "main.rs", "mod.rs") else os.path.dirname(path)
                    for name in source.test_modules:
                        tests.update({f"{parent}/{name}.rs", f"{parent}/{name}/mod.rs"})
        for path in tests:
            self.sources.pop(path, None)
            self.crate_of.pop(path, None)
        self.ns = load_namespaces(root)
        self.roles.update(self.ns.roles if self.ns else {})
        self.modules = {_module_path(p)[0] for p in self.sources if self._in_src(p) and _module_path(p)}
        lib = self.sources.get("src/lib.rs")
        self.aliases = {}  # name -> the module path it stands for, from lib.rs's `use` lines
        for m in _ALIAS.finditer(lib.text if lib else ""):
            segs = m.group(1).split("::")
            if segs[0] in self.modules:
                self.aliases[m.group(2) or segs[-1]] = segs
        self.used = set()  # aliases a path resolved through
        self._edges, self._layers = None, {}

    def _in_src(self, path):
        return self.crate_of.get(path) == self.root_crate and path.startswith("src/")

    def _role_mark(self, path, text):
        m = ROLE_MARK.search(text)
        if not m:
            return
        parts = _module_path(path)
        if len(parts) != 2 or m.group(1) not in ROLES:
            raise RatchetError(
                f"{path}: a `// role:` marker is `adapter` or `feature`, in "
                f"src/<namespace>/<file>.rs; got {m.group(1)!r}"
            )
        self.roles["::".join(parts)] = m.group(1)

    def _node(self, segs):
        """The node a module path belongs to: a role file, else its namespace."""
        if not segs or segs[0] not in self.modules:
            return None
        return "::".join(segs[:2]) if "::".join(segs[:2]) in self.roles else segs[0]

    def resolve(self, segs, use=True):
        """The node of a crate path, through lib.rs's compatibility aliases."""
        if segs and segs[0] in self.aliases:
            self.used.update([segs[0]] if use else [])
            segs = self.aliases[segs[0]] + list(segs[1:])
        return self._node(segs)

    # Nodes of the root crate: namespaces (top-level files of src/ with their
    # submodules) and the files with a role.
    def module_of(self, path):
        return self._node(_module_path(path)) if self._in_src(path) else None

    def module_root(self, module):
        base = "src/" + module.replace("::", "/")
        for path in (base + ".rs", base + "/mod.rs"):
            if path in self.sources:
                return path
        return None

    def edges(self):
        """{node: set of nodes it names}, from crate, super, self and child-module
        paths (`crate::` and `langspan::` only in main)."""
        if self._edges is not None:
            return self._edges
        edges = {n: set() for n in self.modules | set(self.roles)}
        for path, source in self.sources.items():
            node = self.module_of(path)
            if not node:
                continue
            if node == "main":
                paths = named_paths(source.text, None, (), ("crate", "langspan"))
            else:
                paths = named_paths(source.text, _module_path(path), _CHILD.findall(source.text))
            for segs in paths:
                target = self.resolve(segs)
                if target and target != node:
                    edges[node].add(target)
        self._edges = edges
        return edges

    def layer(self, module):
        """A namespace's layer: its namespaces.tsv row, else its `// layer:` marker."""
        if self.ns and self.ns.layer.get(module):
            return self.ns.layer[module]
        if module in self._layers:
            return self._layers[module]
        path = self.module_root(module)
        with open(os.path.join(self.root, path), encoding="utf-8") as f:
            m = LAYER_MARK.search(f.read())
        if m and m.group(1) not in LAYERS:
            raise RatchetError(
                f"{path}: unknown layer {m.group(1)!r} in its `// layer:` marker; "
                f"use one of {', '.join(LAYERS)}"
            )
        self._layers[module] = m.group(1) if m else None
        return self._layers[module]

    def _reason(self, a, b, component):
        """Why the edge a -> b is debt, or None."""
        na, nb = a.split("::")[0], b.split("::")[0]
        la, lb = self.layer(na), self.layer(nb)
        if component[a] == component[b]:
            return "cycle"
        if la and lb and LAYERS.index(la) < LAYERS.index(lb):
            return f"outward ({la} -> {lb})"
        if na == nb and self.roles.get(b) == "adapter" and self.roles.get(a) != "adapter":
            return f"adapter (a pipeline may not name {b})"
        if not self.ns or na == nb:
            return None
        if nb in self.ns.only and na not in self.ns.only[nb]:
            return f"only-list ({nb} is imported only from {', '.join(sorted(self.ns.only[nb]))})"
        if la and la == lb and (na, nb) not in self.ns.allow:
            return f"same-layer ({la}, no `allow {na} {nb}` row)"
        return None

    def debt(self):
        """{node: {target: reason}} for edges that break namespaces.tsv or the layers."""
        edges = self.edges()
        component = _components(edges)
        for module in sorted(self.modules):  # a bad marker fails even without an edge
            self.layer(module)
        out = {}
        for a, targets in edges.items():
            for b in targets:
                reason = self._reason(a, b, component)
                if reason:
                    out.setdefault(a, {})[b] = reason
        return out

    def layout(self):
        """Failures of the layout: depth, top-level files and unused aliases."""
        failures, tops = [], {}
        for path in self.paths:
            parts = path.split("/")
            if len(parts) > MAX_DEPTH + 1:
                failures.append(f"{path}: deeper than src/<namespace>/<sub>/<file>.rs; "
                                f"move it up into {'/'.join(parts[:MAX_DEPTH])}/")
            name = parts[1][:-3] if len(parts) == 2 else parts[1]
            if name not in ("lib", "main"):
                tops.setdefault(name, self.module_root(name) or path)
        if self.ns:
            for name in sorted(set(tops) - set(self.ns.layer)):
                failures.append(
                    f"{tops[name]}: {name} is not a namespace in {NAMESPACES}; move it into a "
                    "namespace (git mv, with a re-export in lib.rs's compatibility block) or "
                    f"add a `namespace {name} LAYER` row"
                )
            for name in sorted(set(self.ns.layer) - set(tops)):
                failures.append(f"{NAMESPACES}: namespace {name} has no src/{name}.rs or "
                                f"src/{name}/mod.rs; delete its rows")
            for role in sorted(r for r in self.ns.roles if not self.module_root(r)):
                failures.append(f"{NAMESPACES}: role {role} has no src/{role.replace('::', '/')}.rs; "
                                "delete its row")
        self.edges()
        for here, _, files in os.walk(os.path.join(self.root, "tests")):
            for f in (f for f in files if f.endswith(".rs")):
                with open(os.path.join(here, f), encoding="utf-8") as fh:
                    for segs in named_paths(strip(fh.read()), None, (), ("langspan",)):
                        self.resolve(segs)
        for name in sorted(set(self.aliases) - self.used):
            failures.append(f"src/lib.rs: compatibility alias {name} "
                            f"({'::'.join(self.aliases[name])}) has no users; delete it")
        return failures


def _module_path(path):
    """The module path of a file under src/: src/a/b.rs and src/a/b/mod.rs are a::b."""
    parts = path[len("src/") : -len(".rs")].split("/")
    if parts[-1] == "mod":
        parts.pop()
    return [] if parts == ["lib"] else parts


_ALIAS = re.compile(r"^[ \t]*(?:pub(?:\([^)]*\))?\s+)?use\s+(\w+(?:::\w+)+)(?:\s+as\s+(\w+))?\s*;", re.M)
_CHILD = re.compile(r"^[ \t]*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;", re.M)
_PATH_HEAD = re.compile(r"(?<![\w:])\$?(\w+)\s*::\s*")
_WORD = re.compile(r"(\w+)\s*")
_RENAME = re.compile(r"as\s+\w+\s*")
_SPACE = re.compile(r"\s*")
_SEPARATOR = re.compile(r"[\s,]*")


def _use_tree(text, i):
    """([segments...] for each path of the use tree at i, the offset after it)."""
    i = _SPACE.match(text, i).end()
    if text.startswith("{", i):
        out, i = [], i + 1
        while i < len(text) and not text.startswith("}", i):
            sub, j = _use_tree(text, i)
            out += sub
            i = _SEPARATOR.match(text, max(j, i + 1)).end()
        return out, i + 1
    m = _WORD.match(text, i)
    if not m:
        return [[]], i
    if text.startswith("::", m.end()):
        subs, i = _use_tree(text, m.end() + 2)
        return [[m.group(1)] + s for s in subs], i
    rename = _RENAME.match(text, m.end())  # `b as c` names b; c is a new local name
    return [[m.group(1)]], rename.end() if rename else m.end()


def named_paths(text, module_path, children, prefixes=("crate",)):
    """The first two segments of each crate-absolute module path text names.

    A path starts at one of prefixes or, with a module_path, at `super`, `self`
    or one of the file's child modules. A `self` in a group names its parent.
    """
    consumed = 0
    for m in _PATH_HEAD.finditer(text):
        head = m.group(1)
        if m.start() < consumed:
            continue
        if head in prefixes:
            out = []
        elif module_path is not None and (head in ("super", "self") or head in children):
            out = list(module_path) + ([head] if head in children else [])
            out = out[:-1] if head == "super" else out
        else:
            continue
        trees, consumed = _use_tree(text, m.end())
        for segs in trees:
            path = list(out)
            for seg in segs:
                if seg == "super":
                    path = path[:-1]
                elif seg != "self":
                    path.append(seg)
            if path:
                yield tuple(path[:2])


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


class Namespaces:
    def __init__(self):
        self.layer, self.allow, self.only, self.roles = {}, set(), {}, {}


def parse_namespaces(text, where):
    """namespaces.tsv rows: `namespace NAME LAYER` (`-` for none yet), `allow FROM TO`
    (a same-layer edge), `only TARGET IMPORTER...` and `role NAMESPACE::FILE ROLE`."""
    ns, named = Namespaces(), []
    for number, line in enumerate(text.split("\n"), 1):
        row, at = line.split(), f"{where}:{number}"
        if not row or row[0].startswith("#"):
            continue
        if row[0] == "namespace" and len(row) == 3:
            if row[2] not in (*LAYERS, "-"):
                raise RatchetError(f"{at}: unknown layer {row[2]!r}; use one of {', '.join(LAYERS)}, or -")
            ns.layer[row[1]] = None if row[2] == "-" else row[2]
        elif row[0] == "allow" and len(row) == 3:
            ns.allow.add((row[1], row[2]))
            named += [(at, row[1]), (at, row[2])]
        elif row[0] == "only" and len(row) >= 3:
            ns.only[row[1]] = set(row[2:])
            named += [(at, n) for n in row[1:]]
        elif row[0] == "role" and len(row) == 3 and row[1].count("::") == 1:
            if row[2] not in ROLES:
                raise RatchetError(f"{at}: unknown role {row[2]!r}; use one of {', '.join(ROLES)}")
            ns.roles[row[1]] = row[2]
            named.append((at, row[1].split("::")[0]))
        else:
            raise RatchetError(
                f"{at}: expected `namespace NAME LAYER`, `allow FROM TO`, `only TARGET "
                f"IMPORTER...` or `role NAMESPACE::FILE ROLE`; got {line!r}"
            )
    for at, name in named:
        if name not in ns.layer and name != "main":
            raise RatchetError(f"{at}: {name} is not a namespace; add a `namespace {name} LAYER` row")
    return ns


def load_namespaces(root):
    """The parsed namespaces.tsv, or None in a workspace without one."""
    path = os.path.join(root, NAMESPACES)
    if not os.path.exists(path):
        return None
    with open(path, encoding="utf-8") as f:
        return parse_namespaces(f.read(), NAMESPACES)


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
        if not path:
            continue
        rows = set().union(*(baselines[p].edges for p in ws.sources if ws.module_of(p) == module and p in baselines))
        # A row names a node, or a module that an alias in lib.rs now stands for.
        allowed = {r: ws.resolve(r.split("::"), use=False) for r in rows}
        for target, reason in sorted(debt.get(module, {}).items()):
            if target not in allowed.values():
                failures.append(
                    f"{path}: module {module} now depends on {target}, a new {reason} edge. "
                    f"Move the shared item to a lower module, or justify the edge in the pull "
                    f"request and run `python3 -I -B scripts/ratchets.py --record {path}`"
                )
        for row in sorted(r for r, node in allowed.items() if node not in debt.get(module, {})):
            improvements.append(f"{path}: edge {module} -> {row} is no longer debt; "
                                + TIGHTEN.format(path=path))
    failures += ws.layout()
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
    debt = ws.debt().get(module, {}) if module else {}
    b.edges = {r for r in b.edges if ws.resolve(r.split("::"), use=False) in debt}
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
