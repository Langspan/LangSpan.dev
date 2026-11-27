`hash: wc:5957`

- Main command router for `langspan` subcommands.
- Handles help/version, parses options, routes install/uninstall/setup/demo/guidance/doctor/hosts/check/land/statusline/count/notify/commit/adopt/dashboard/daemon/mcp.
- Enforces option validity and command-specific argument rules.
- Delegates execution to domain drivers in `cli/*`, local commands, and request pipeline.

