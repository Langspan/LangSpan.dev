`hash: wc:1324`

- Exposes adapter-layer modules for CLI, daemon, hooks, MCP, hosts, git, integration, storage, and reporting.
- Re-exports a shared error type: `Box<dyn std::error::Error + Send + Sync>`.
- Keeps compatibility aliases and module boundary between pure core and side-effect adapters.
