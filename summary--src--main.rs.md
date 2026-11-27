`hash: wc:2154`

- Binary entrypoint dispatches `hook` specially (no async runtime, hot path).
- Other commands build Tokio runtime, call async `run`, and print structured/plain errors via `error_line`.
- Error output is plain text on terminal unless `--json`, otherwise a single JSON error line to stderr.

