user_version = 4

-- index baselines_by_path on baselines
CREATE INDEX baselines_by_path ON baselines(workspace_id, path);

-- index calls_by_callee on calls
CREATE INDEX calls_by_callee ON calls(workspace_id, callee, path, line);

-- index calls_by_path on calls
CREATE INDEX calls_by_path ON calls(workspace_id, path, line);

-- index imports_by_path on imports
CREATE INDEX imports_by_path ON imports(workspace_id, path);

-- index inflight_by_path on inflight
CREATE INDEX inflight_by_path ON inflight(workspace_id, path);

-- index refs_by_name on refs
CREATE INDEX refs_by_name ON refs(workspace_id, name, path);

-- index refs_by_path on refs
CREATE INDEX refs_by_path ON refs(workspace_id, path);

-- index symbols_by_name on symbols
CREATE INDEX symbols_by_name ON symbols(workspace_id, name);

-- index symbols_by_path on symbols
CREATE INDEX symbols_by_path ON symbols(workspace_id, path);

-- table baselines on baselines
CREATE TABLE baselines (
                    workspace_id TEXT NOT NULL, path TEXT NOT NULL, name TEXT NOT NULL,
                    kind TEXT NOT NULL, signature TEXT NOT NULL, block_digest TEXT, line INTEGER, end_line INTEGER);

-- table calls on calls
CREATE TABLE calls (
                    workspace_id TEXT NOT NULL, path TEXT NOT NULL, caller TEXT,
                    callee TEXT NOT NULL, line INTEGER NOT NULL, args INTEGER);

-- table files on files
CREATE TABLE files (
                    workspace_id TEXT NOT NULL, path TEXT NOT NULL,
                    scan_digest TEXT NOT NULL, digest TEXT, partial INTEGER NOT NULL DEFAULT 0,
                    baseline_digest TEXT, partial_since INTEGER, oversize_digest TEXT,
                    PRIMARY KEY (workspace_id, path)) WITHOUT ROWID;

-- table imports on imports
CREATE TABLE imports (
                    workspace_id TEXT NOT NULL, path TEXT NOT NULL, target TEXT NOT NULL,
                    line INTEGER NOT NULL);

-- table inflight on inflight
CREATE TABLE inflight (
                workspace_id TEXT NOT NULL, path TEXT NOT NULL, name TEXT NOT NULL,
                kind TEXT NOT NULL, signature TEXT NOT NULL, renamed_from TEXT);

-- table inflight_files on inflight_files
CREATE TABLE inflight_files (
                workspace_id TEXT NOT NULL, path TEXT NOT NULL, digest TEXT NOT NULL,
                head_object TEXT, PRIMARY KEY (workspace_id, path)) WITHOUT ROWID;

-- table passes on passes
CREATE TABLE passes (
                workspace_id TEXT NOT NULL PRIMARY KEY, count INTEGER NOT NULL) WITHOUT ROWID;

-- table refs on refs
CREATE TABLE refs (
                    workspace_id TEXT NOT NULL, path TEXT NOT NULL, name TEXT NOT NULL,
                    line INTEGER NOT NULL);

-- table retries on retries
CREATE TABLE retries (
                workspace_id TEXT NOT NULL, path TEXT NOT NULL, stage TEXT NOT NULL,
                version TEXT NOT NULL, failures INTEGER NOT NULL, next_pass INTEGER NOT NULL,
                PRIMARY KEY (workspace_id, path, stage)) WITHOUT ROWID;

-- table roots on roots
CREATE TABLE roots (
                workspace_id TEXT NOT NULL PRIMARY KEY, root TEXT NOT NULL,
                revision INTEGER NOT NULL DEFAULT 0) WITHOUT ROWID;

-- table scan_summary on scan_summary
CREATE TABLE scan_summary (
                    workspace_id TEXT NOT NULL PRIMARY KEY, too_large INTEGER NOT NULL,
                    unscanned INTEGER NOT NULL, languages TEXT NOT NULL) WITHOUT ROWID;

-- table symbols on symbols
CREATE TABLE symbols (
                    workspace_id TEXT NOT NULL, path TEXT NOT NULL, name TEXT NOT NULL,
                    kind TEXT NOT NULL, line INTEGER NOT NULL, end_line INTEGER NOT NULL,
                    signature TEXT NOT NULL, exported INTEGER NOT NULL,
                    body_digest TEXT NOT NULL);

-- table unread on unread
CREATE TABLE unread (
                    workspace_id TEXT NOT NULL, path TEXT NOT NULL, reason TEXT NOT NULL,
                    PRIMARY KEY (workspace_id, path)) WITHOUT ROWID;
