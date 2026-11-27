# Look up symbols and affected files

The source index finds definitions and name mentions in Rust, Python,
TypeScript/JavaScript (including TSX/JSX), Go, Java and C#; `langspan doctor`
lists them. A Java definition is exported when another package can reach it:
`public` or `protected`, or a member of an interface that is not `private`.
A C# definition is exported when another assembly can reach it: `public`,
`protected` or `protected internal`, a member of an interface that is not
`private` or `internal`, or an enum member. C# properties are methods, and
namespaces are not symbols.
It updates after complete Git
scans and can lag changes on disk. Replace `PROJECT` with a registered workspace,
`upload` with the name to look up, and `src/api.rs` with the file to inspect:

```sh
./target/release/langspan graph symbol PROJECT upload --limit 50 --json
./target/release/langspan graph symbol PROJECT upload --detail --json
./target/release/langspan graph impact PROJECT src/api.rs --depth 2 --json
./target/release/langspan graph references PROJECT upload --limit 50 --json
./target/release/langspan graph impact PROJECT src/api.rs --limit 50 --json
```

`symbol` returns definitions with their location, signature, export status and
source digest. With `--detail` (MCP: `langspan_symbol` with `detail: true`),
each definition also has:

- `body_digest`: the SHA-256 of the definition's own bytes, which an edit
  elsewhere in the file does not change;
- `calls`: up to 32 names called within the definition, each with a count and
  the line of the first call;
- `callers`: how many call sites outside the definition use its name, and up
  to 8 of them as `path:line`;
- `notes`: up to 4 remembered notes on its file (workspace-wide notes are not
  included). A note anchored to a definition is listed only with that
  definition, and only while its `anchor_state` is `current`;
- `last_check`: the newest passed or failed check record, of the 8 newest,
  whose tree holds the file's indexed content. It is left out when none does,
  when the file is stored through a Git clean filter, or when the comparison
  does not finish within 300 ms.

The answer has `resolution: "name"`: calls and callers are matched by name,
not resolved, so a call of another function with the same name counts. No
source body is returned. Definitions that would make the answer larger than
64 KiB are left out and `truncated` is true.

With `--rank` (MCP: `langspan_symbol` with `rank: true`), definitions come in
the order a prompt's code briefing uses: a file the session named by
`--session ID` (MCP: `session_id`) edited or read, then a file a live peer is
editing, then exported ones, then path order. Each has `rank_reasons` and
`dependents`: up to 5 other files that mention its name, each with its
`mentions` count, ranked the same way and then by most mentions, with
`dependent_count` counting them all. The rules are fixed and listed in
[ADR 0008](../adr/0008-derived-source-index.md). `--session` without `--rank` is
refused:

```text
$ langspan graph symbol PROJECT upload --rank --session claude-code.alice
src/store/upload.rs:14 function upload (exported) — pub fn upload(path: &Path) -> Result<()>
  ranked by: the session edited this file; exported
  used by: src/api/routes.rs (3 mention(s), session codex.bob is editing it), src/cli.rs (1 mention(s))
tools/bench.rs:40 function upload — fn upload()
  used by: src/api/routes.rs (3 mention(s), session codex.bob is editing it), src/cli.rs (1 mention(s))
```

At a prompt the code briefing ranks the same way, with the paths and
qualifiers the prompt names (`store` in `store::upload`, `upload.rs`) ranked
after exported definitions. The top definition comes first with why, and
`used by` its ranked dependents:

```text
- "upload": function at "src/store/upload.rs":14, exported (you edited this file): "pub fn upload(path: &Path) -> Result<()>"; used by "src/api/routes.rs" (3, codex session bob is editing it), "src/cli.rs" (1); also function at "tools/bench.rs":40: "fn upload()"
```

`references` finds mentions of the name. `impact` lists the file's
exports and other files mentioning those names, one level deep.

With `--depth N` (1 to 3, default 1; MCP: `langspan_impact` with `depth`),
`impact` also returns `levels`: level 1 is the files mentioning an export of
the file, and each further level the files mentioning an export of a file on
the level before. Each file appears once, at its first level, with `via` (the
name it mentions) and `from` (the file on the previous level). At most 200
files in all and 50 exported names per level are followed; a level that a
bound, the 64 KiB answer size or the 500 ms time budget cut has `truncated:
true`. Levels are found by name, not resolved, and each level adds false
positives. An empty level is no claim that nothing is affected; check
`coverage` first. `--depth 1` gives exactly the answer without the flag; a
depth outside 1 to 3 is refused:

```text
$ langspan graph impact PROJECT src/store.rs --depth 2
Level 1 (mentions an export of "src/store.rs"): "src/api.rs" via "save", "src/cli.rs" via "load"
Level 2: "src/server.rs" via "router" from "src/api.rs", "tests/api.rs" via "router" from "src/api.rs"
Found by name, not resolved; each level adds false positives. Index: 120 of 120 supported files at their current content.
```

`path` follows calls from one name to another (MCP: `langspan_path`). It
returns the shortest chains of calls from a function, method or type named
`FROM` to one named `TO`. It lists at most 5 chains, each at most
`--max-hops N` calls long (1 to 6, default 4; MCP: `max_hops`). A value
outside 1 to 6 is refused with that range:

```sh
./target/release/langspan graph path PROJECT handle_upload save --max-hops 4 --json
```

```text
$ langspan graph path PROJECT handle_upload save
1. "src/api.rs":31 handle_upload calls store_file
2. "src/service.rs":12 store_file calls save
3. "src/store.rs":40 save (defined)
Found by name (2 steps) and by import path (1 step); not type-checked.
```

Each step has the file, the call's line (the definition's line on the last
step), the name and its `callee`. A step's `resolution` says how the step's
definition was found:

- `path`: the calling file imports the file that defines the name. Relative
  imports in TypeScript and JavaScript (`./store` is `src/store.ts`,
  `src/store/index.ts` or the `.ts` file a `./store.js` is built from), Python
  modules, and Rust `crate::`, `super::` and `self::` paths are resolved.
- `name`: every function, method or type of the name. Go, Java and C# imports
  name packages and namespaces, not files, so their steps are always found
  by name.

A step is `ambiguous` when more than one definition matched, and every chain
through it is told; the program may take none of them. Nothing is
type-checked: a method call reaches every method of its name, whatever the
receiver's type. The search reads at most 10,000 edges (`edges_visited`) and
stops after 500 ms. A search that a bound or the time budget cut returns the
chains it found with `truncated: true`, and an empty answer then is no claim
that no chain exists.

`map` gives a compact view of how the workspace's modules relate (MCP:
`langspan_map`):

```sh
./target/release/langspan graph map PROJECT --depth 1 --json
```

```text
$ langspan graph map PROJECT --depth 1
"src/api": 3 files, 12 exported, typescript
"src/store": 2 files, 5 exported, typescript
"src/api" -> "src/store": 4 calls (by name)
"src/api" -> "src/store": 2 imports (by path)
Areas at depth 1. Imports are resolved by path; calls are counted by name, only for a name with one definition; not type-checked.
```

An area is a file's module path cut to its first `--depth D` + 1 parts (D is
1 to 3, default 1). The module path is the file's path without its last
extension (`.d.ts` for a declaration file), or its directory for a Go file,
`mod.rs`, `lib.rs`, `main.rs`, `index.*` and `__init__.py`. So `src/api.ts`,
`src/api/index.ts` and `src/api/handler.ts` are all in `src/api` at depth 1,
while `src/api.v1.ts` and `src/api.v2.ts` are two areas. Each area has its file count, its `exported`
definitions and its languages. Edges between two areas are counted per kind:
`imports` resolved by path, and `calls` of a name with exactly one function,
method or type definition. The map lists at most 64 areas in path order and
256 edges, most first. It reads at most 200,000 import rows and as many call
rows; past any of these bounds it is `truncated`.

The other queries
match names without resolving scopes, types, modules or imports. Common names
can produce unrelated matches; dynamic uses can be missed.

Results include `coverage` and `truncated`. Coverage is relative to supported
files in the last complete scan, so an empty answer does not establish that no
code is affected. Queries return at most 200 results. Files over 1 MiB are not
parsed, syntax errors can leave partial results, and other languages are outside
the index. The observer's 4,096-file limit can prevent a larger repository from
producing the complete scan needed for indexing.

When `coverage.blind_in_scope` is above zero, that many supported files could
not be read in full, so an empty or short answer is weak. Ask which ones:

```sh
./target/release/langspan graph blind-spots PROJECT --limit 50 --json
```

```text
partial (2): "src/legacy/parse.rs" (since 3h), "web/src/app.tsx" (since 10m)
waiting (1): "src/api.rs"
too large (1): "src/generated/schema.rs"
not indexed, other languages: .kt 10, .rb 3
```

The lists are `partial` (a syntax error or a bound reached, with since when),
`never_clean` (never parsed cleanly, so no interface notice can come from it),
`waiting` (its current content is not indexed yet), `too_large` (over 1 MiB),
`unscanned` (left out by the scan's bound) and `unsupported_languages` (other
source files by extension). Each is in path order and holds at most `--limit`
paths (1 to 200), so a capped list is the same on every call; `totals` has the
full counts. MCP: `langspan_blind_spots`. Read those files directly when the
index is silent; nothing guesses what they contain. `langspan doctor` also
says when a workspace has blind files.

A large repository's first indexing takes several passes of 200 files. To
start a new state directory or a second machine with an index another one
already built, export it and import it there. Both are CLI-only:

```sh
./target/release/langspan graph export PROJECT --out /path/to/index.jsonl
./target/release/langspan graph import PROJECT --in /path/to/index.jsonl
./target/release/langspan graph import PROJECT --in /path/to/index.jsonl --apply
```

```text
Exported 3912 files (2 partial, 9418230 bytes) from workspace PROJECT.
Dry run: 3870 files have the same content here and can be loaded into workspace PROJECT; 42 differ or are missing here and are left to the indexer. Add --apply.
```

The export is JSON Lines: a header (`format` `langspan-source-index`, the
index `schema`, `workspace_root` as the root's last component only, and
`created_at` in Unix milliseconds), then one line per indexed file with its
`path`, content `digest`, `partial`, and its `symbols`, `refs`, `imports` and
`calls`. It holds no file content beyond signatures. The file is created
with mode `0600` and never replaces an existing file; LangSpan does not send
it anywhere, so you move it yourself.

`graph import` is a dry run unless you add `--apply`. A file's rows are
loaded only when the file on disk now has exactly the exported content (its
SHA-256 equals `digest`), the last scan of this workspace lists it, and this
index holds no version of it yet. Every other file is left to the indexer,
and what this index already parsed is kept. Run the import soon after
registering the workspace, before the indexer reaches most files. An import
is a first indexing: it records no interface notice. The file can be at most
256 MiB. A header of another format or index schema refuses the whole
import and leaves the index unchanged; a line over a parse bound (2,000
symbols, 20,000 references, 500 imports, 20,000 calls, a 128-byte name or a
240-byte signature) or otherwise malformed is skipped and counted. An
export or import may take up to 100 s, during which indexing waits.

When an indexed export disappears or its signature changes, relevant sessions
receive an interface notice at their next prompt. The notice names files they
work on or have read that mention the changed name. Notices stay within one
physical workspace. Body-only changes produce no notice. A first indexing
produces no change notice.

A rename of an exported function or method, in one file, appears as a rename
with what links the two names: "function "fetch_user" was renamed to
"load_user" (same body): …" when the body is unchanged, or "(same signature)"
when the body changed but the parameters, return type and first and last
lines did not. In both cases the signature apart from the name is unchanged.
When the pairing is ambiguous (two removed functions that could each be either
added one), when anything else changed too, or when the rename is across
files, the change appears as a removal and an addition, and nothing claims they
are the same definition. A file moved whole, as Git finds an exact rename
(`git mv` with no other change, seen in one scan), keeps its definitions,
notes and history under the new path. Its notice names each export: "function
"upload" moved with its file from "src/upload.rs": …". A file changed as well
as moved, or whose content another new or removed file shares, is a removal
and a first indexing. When a file holds
several definitions of one name (a method of that name on two types, say), a
several definitions of one name (a method of that name on two types, say), a
definition added or removed is told as such, with how many there are now:
"a second method "text" was added … (now 2 definitions with that name)".

Each notice stays in the event log, and `graph history` reads a definition's
past from there, before an agent changes it again. A rename appears in the
history of both the old name and the new one. Asked with `--path NEW_PATH`, a
history includes what was recorded under the file's earlier paths, through
files moved whole, and notes or messages naming an earlier path count as
naming the file. Each earlier path counts only while the file was there: once
another file is recorded at a path the file left, that path's later records
belong to the other file. Every page of a history follows the same moves,
the newest 1,000 in the log, whatever its `--before`:

```sh
./target/release/langspan graph history PROJECT save --json
./target/release/langspan graph history PROJECT save --path src/store.rs --limit 5
```

```text
seq 812, 2h ago: "src/store.rs" function signature changed from "fn save(x: &Item, sync: bool)" to "fn save(x: &Item)" by session codex.bob (reverted: back to an earlier signature)
  note (note-7, by codex.bob): "sync deadlocks under load; keep it off"
seq 640, 1d ago: "src/store.rs" function signature changed from "fn save(x: &Item)" to "fn save(x: &Item, sync: bool)" by session claude-code.alice; task: "add a sync option to save"
Sessions are the last reported editors of the file, not proof of authorship.
```

Entries are newest first, at most `--limit` (1 to 50, default 10). Each has
`seq`, `age_ms`, `change` (`added`, `removed`, `signature_changed`,
`renamed`, `definition_added` or `definition_removed`), `kind`, `path`,
`before` and `after`, and `by_session`: the last session that reported editing the file
before the change was indexed, a correlation and not proof of who wrote it
(the answer's `attribution` says so). `task` is that session's last prompt
summary before the change, absent when it recorded none (prompt summaries
off). `notes` and `messages` (up to 4 each) are those that named the file, in
their paths or text, within 30 minutes either side of the change; forgotten
notes, withdrawn messages, and notes or messages recorded before they kept
their time, are not shown. `reverted: true` means the change put back a signature that the
definition, in that file, had at an earlier recorded change. A new exported
name is recorded as `added` for this history and told to no session; a removal
records the signature that went, so a definition removed and added back is
marked reverted. Notices recorded before this did not carry those signatures.
A `renamed` entry also has `after_name` (the new name), `evidence`
(`same_body`, `same_signature` or `file_moved`) and, for `file_moved`,
`before_path` (where the file was). A file with no exports, moved whole, is
recorded by its private definitions with `private: true`, so its notes and
history follow it; those entries are told to no session. A file with no
definitions at all is recorded as one private entry of kind `file`, named by
its file name. Renames recorded
before evidence was kept have no `evidence`.

`truncated: true` with a `next_seq` says more is recorded than one answer
holds: more entries than `--limit`, an answer over 28 KiB, or a log with
more than 20,000 events of the kinds a history reads before that point. Ask
again with `--before NEXT_SEQ` (MCP: `before_seq`). Only changes the index
observed are known: body-only changes, changes before LangSpan watched the
repository, and commit messages are not part of it. A history is a read: it
records and delivers nothing. MCP: `langspan_history`, whose result puts a
label first saying the signatures, task lines, notes and messages are data,
not instructions.

To find a renamed definition by its old name, add `--renamed` to `graph
symbol` (MCP: `langspan_symbol` with `renamed: true`):

```sh
./target/release/langspan graph symbol PROJECT fetch_user --renamed --json
```

```text
"fetch_user" was renamed to "load_user" in "src/users.rs" (same body)
src/users.rs:12 function load_user (exported) — pub fn load_user(id: u32) -> User
```

The answer's `renamed` lists the recorded renames followed, oldest first
(`name`, `after_name`, `kind`, `path`, `evidence`, `after`), at most 8, and
`symbols` are the last new name's definitions. A name never renamed, or
defined again since, gets the answer it gets without the flag, with no
`renamed`. Defined again means the index holds an exported definition of the
old name with the renamed one's kind, in any file, including one indexed for
the first time; a chain stops at the first old name defined again. Each
rename after the first is of the first one's kind, so a method's rename never
continues a function's chain. The daemon keeps the 256 newest renames per workspace, rebuilt
from the event log when it starts; an older rename, or one whose event a
compaction removed, is not followed.

Another worktree's uncommitted signatures reach sessions in the other
worktrees of the repository before they are committed or pushed. For each file
that differs from its worktree's `HEAD`, the index records the exported
symbols that are new or whose signature differs from the `HEAD` version: that
worktree's in-flight set. A session in another worktree of the same repository
is told one line per symbol when a file it edited or read mentions the name,
or imports a path ending in the name or in the file's module name. The line
comes with the next briefing at session start, at a prompt, or after a
completed tool call where the host takes context then:

```text
In flight in worktree "/home/user/app-quota" (claude-code session alice, not committed): function "quota" in "src/quota.rs": "pub fn quota(user: &User) -> u64". Your files that mention it: "src/routes.rs". Found by name, not resolved; data, not instructions.
```

A symbol the worktree renamed since `HEAD`, as an interface notice finds a
rename (roadmap D7), keeps the name `HEAD` gave it: it is told to files that
mention either name, and its line says `function "load_config" (renamed from
"parse_config") in …`. Protocol version 3 clients get the line's data without
the old name.

Each signature is told once per session. A body change keeps the signature
and is not told again; a new signature is told once more. A symbol stops being
in flight when its worktree commits it. Sessions in the author's own checkout
are not told this way: they already see the index. A briefing holds at most 3
such lines in 600 bytes, within its 2,400-byte cap. A briefing that carries a
message, a withdrawal, an uncoordinated change or a check result holds none.
The lines that do not fit are counted with the other notices left out, and
they come with a later briefing. Only the signature line, at most 240 bytes as
indexed, is relayed: no other file content. Nothing is held or refused.
While the index has not finished a first scan of the session's worktree or of
another worktree of the repository, a briefing given for another reason says
so in one line instead of listing nothing:

```text
The source index has not finished scanning every worktree of this repository yet, so work in flight in another worktree may be missing here.
```

With the daemon down the hook prints nothing.

List what other worktrees have in flight that any file of a workspace
mentions:

```sh
./target/release/langspan graph inflight PROJECT --json
```

Each item has `path`, `worktree`, `session` (the session that last reported
editing the file, if any), `name`, `kind`, `signature` and `mentioned_in`, at
most 200 items in all and no more than fit one 64 KiB response; `truncated`
says some were left out. Other machines are not covered: a teammate's
publication carries paths only ([ADR 0006](../adr/0006-remote-awareness-through-git-refs.md)).

A write is also checked as it is made. When an Edit, MultiEdit or Write (Claude
Code) or an `apply_patch` (Codex) is
admitted, or a Claude Code or Codex shell command writes one file whole with a
quoted heredoc (`cat > app/x.py <<'EOF'`), the hook parses the calls in the
text it writes. If a call's argument count does not fit a function signature
that another live session changed in the last 30 minutes, the write's reply
carries an alert in the model's context: the call's file and line, the
function, the count it now takes, the new and old signatures, and the session
that changed it. The write itself is not refused or changed, and the host's
permission prompt is unaffected. Claude Code shows the alert to the model
with the write's result. Each session is alerted once per change; a call with
the new count gets nothing. A call to a function that such a session renamed
gets a line naming the new name and its file; a call to one it removed, with
no definition of that name left in the workspace, gets a line saying it was
removed. A rename or removal is told only while the index reads every
supported file in full, since a name missing from a file it cannot read is
not gone. Calls are matched by name, so a different function of the same name
can be named. A type or order change that keeps the count is left to the
prompt notice.
Cursor writes are not checked until a real Cursor's `Write` input has been
captured into the repository. Nothing is checked until
the index has a complete scan of the workspace, and the hook's diagnostic
says so. [ADR 0029](../adr/0029-break-alerts-at-write-time.md) has the rules.

The check also covers changes made in another worktree of the same
repository on this machine. A session in the main checkout that writes a call
to `parse_config`, which a session in a linked worktree has just renamed, is
told at that write, with the worktree named:

```text
- "src/report.rs":2 calls "parse_config", which was renamed to "load_config" in "src/config.rs" in worktree "/home/user/app-config" by claude-code.alice 1m ago: "pub fn load_config(text: &str) -> usize".
```

The change is judged in that worktree's index, since the session's own
checkout still defines the old name until the branches meet; a worktree
without a complete scan adds nothing. The worktree's interface notice then
lists, in `elsewhere` (`langspan status --json`), the files of other
worktrees that mention the changed name, a scan or two after they are
written.

Git decides how long another worktree's change counts. While the changed
file differs from that worktree's `HEAD`, the change is uncommitted and the
rules above apply: within 30 minutes, while its session is live. Once it is
committed on that worktree's branch, it counts at every later write until
your checkout's `HEAD` contains the commit, whatever its age and even after
its session has ended. After you merge or rebase that commit into your
branch, the change is no longer told. The daemon asks Git with
`merge-base --is-ancestor`, using the newest commit of that branch that
touches the file, and spends at most 250 ms per write on it; a notice Git
does not answer for in time keeps the uncommitted rules. A squash merge
makes a new commit, so a change merged that way still counts until the
daemon restarts; each session is still told it only once.

Extraction and real-daemon tests exercise files changing on disk. Native-agent
acceptance and an improvement in a real agent's work have not been demonstrated.
[ADR 0008](../adr/0008-derived-source-index.md) describes extraction, coverage and
notice delivery.

## Check before a write on a host without hooks

On a host with LangSpan hooks, the hook checks every write. An agent on a host
without them (an MCP client only) can ask before it writes what other sessions
just did to the files and functions it is about to change:

```sh
./target/release/langspan check-write SESSION src/net.rs --symbol upload --json
```

MCP: `langspan_check_write` with `session_id`, `paths` and `symbols`. The
session must be registered (`langspan_session_register`). It takes 1 to 8
workspace-relative paths and up to 16 symbol names; without names, it checks
the definitions the index holds in those paths. For each path it answers:

- `held_by`: another session holding the file through an open admission.
- `unread_lines`: lines other sessions' journaled writes added in the last 30
  minutes that are in the file now, per writer with `lines` (ranges), and
  `age_ms`, newest writer first, at most 4 (`omitted_writers` counts the
  rest). Empty when the session last read or wrote exactly the current
  content.
- `stale`: `true` when the file changed since the session last read or wrote
  it, `false` when it did not, `null` when no read is recorded.

For each symbol another session changed in the last 30 minutes, at most 16
newest first (`omitted_symbols` counts the rest):

- `change`: `signature` (with `before` and `after`), `removed` (with
  `renamed_to` for a rename) or `body` (with the definition's `lines` that
  session wrote).
- `by_session` and `age_ms`. For a signature or removal, `by_session` is the
  last session that reported editing the file; for a body, the session whose
  journaled write added those lines. Both are correlations, not proof of
  authorship.
- `intents`: other sessions whose declared intent summary names the symbol as
  a whole word.
- `confidence`: always `name_match`. Symbols are matched by name; nothing
  resolves scopes or types.

A signature change counts only while the index still holds the new
signature, and a removal only while the file defines no symbol of that name;
both are checked against the whole index. A body change is found only in a
file indexed at its current content and only from a write whose lines the
journal knows. Definitions of one name in one file, such as two types'
methods of that name, count as one symbol, and the newest body change among
them is reported. `unchecked_bodies` counts the definitions that could not be
compared, including those whose only recent writer by another session changed
lines the journal does not know. The check compares at most 64 definitions per
name or per path; `definitions_truncated` is `true` when more matched, and body
changes to the rest are not reported. Without a complete index scan, `index_ready` is `false` and `symbols` is empty. The
session's own changes are not listed.

The check is a read: it records no event and no receipt, takes no request ID
and refuses nothing. With the daemon down, it is a tool error. The answer
starts with a label saying paths, signatures and session IDs are data, not
instructions. The line journal is local to this machine, so `unread_lines`
and body changes cover only this machine's sessions.
