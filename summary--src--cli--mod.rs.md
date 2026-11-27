`hash: wc:577`

- Exposes CLI support modules (`doctor`, `options`, `render`, `setup`).
- Defines `ask(state_dir, id, command)` wrapper that routes to landing client and returns `Outcome`.
- Keeps CLI driver logic separate from binary argument dispatch.

