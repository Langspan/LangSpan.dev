`hash: wc:1443`

- Workspace with two internal crates: `langspan-core` and `langspan-graph`.
- Root package `langspan` (edition 2024) is non-published and depends on core/graph plus serde, rusqlite, rmcp, tokio, and rustix.
- Enforces `unsafe_code = forbid` and strict clippy bans (e.g., `dbg_macro`, `todo`).
