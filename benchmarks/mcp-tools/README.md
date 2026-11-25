# MCP tool list size

Roadmap item I11 asks how much context the LangSpan MCP tool list costs before
any of it is hidden. This records that measurement.

## How it is measured

```sh
cargo test --locked --test mcp_hooked measure_the_served_tool_list -- --nocapture
```

The test starts the real `langspan mcp`, performs the MCP handshake over stdio
with the official Rust SDK client, asks for `tools/list` and prints one JSON
document: the bytes of each tool as compact JSON (name, description, input
schema and annotations, which is what a host receives), their sum for every
tool and for the list `--tools hooked` serves while hooks report, and the
bytes of the server instructions in both modes.

The tool list a server sends does not depend on the host: Claude Code and
Codex receive the same bytes. What each host then puts in its model's context
(its own wrapping, whether it defers tool schemas until they are needed) is
the host's choice and was not measured. Tokens were not measured either: they
depend on each host's model tokenizer, which is not available offline.

## Result

One run, 2026-11-25, LangSpan 0.1.0 (this branch, debug build), macOS. The figures are byte counts of deterministic output, so one run is
the whole sample.

| | Tools | Tool list bytes | Instructions bytes | Total |
| --- | ---: | ---: | ---: | ---: |
| `--tools all` (default) | 28 | 31,840 | 494 | 32,334 |
| `--tools hooked`, hooks reporting | 24 | 28,950 | 781 | 29,731 |
| Difference | 4 | 2,890 | +287 | 2,603 (8.1%) |

The four tools left out while hooks report:

| Tool | Bytes |
| --- | ---: |
| `langspan_admit` | 899 |
| `langspan_complete` | 590 |
| `langspan_session_register` | 663 |
| `langspan_workspace_register` | 738 |

The longest tools are `langspan_land` (3,215 bytes) and
`langspan_integrate_check` (2,505 bytes); the full per-tool figures are in the
test's output.

## Reading it

The whole list is about 32 KB on every request that carries it. Hiding the
four bookkeeping tools saves about 2.6 KB of that, 8.1%, once the longer
instructions naming them are counted. Whether that is worth having is the
owner's call: the option is off unless an install asks for it
(`install --mcp-tools hooked`), and LangSpan never turns it on by itself. The larger effect the issue names,
a model calling bookkeeping tools by hand, is not measured here.

Recorded hook events from Claude Code 2.1.331 and Codex 0.171.0
(`fixtures/`) drive the integration tests in `tests/mcp_hooked.rs`; a real
Claude Code session showing the shorter list in `/mcp` has not been run.
