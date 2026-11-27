`hash: wc:5517`

- Architecture overview states Rust implementation split across pure `langspan-core`, SQLite event/receipt store, Unix-socket daemon, CLI, and stdio MCP adapter.
- Source index is in `crates/langspan-graph`.
- Notes protocol/schema compatibility: wire protocol version 10 with backward shapes, SQLite schema marker 4, rollback compatibility policy.
- Emphasizes caller authority split between coordination commands and owner grants.
- Includes milestone decision history via ADRs and evidence-oriented acceptance framing.

