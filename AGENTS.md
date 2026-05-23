# agentnoise

This is a Rust CLI/daemon project.

- Keep the trusted bridge path native: Rust plus the local launcher policy.
- Do not add Node, npm, bun, Electron, or Tauri to the daemon path.
- Agent execution must go through `bondage` profiles.
- Chat commands must map to structured argv arrays, never shell-concatenated strings.
- Treat the Marmot v2 protocol (implemented by the `darkmatter` Rust workspace) as the transport/control channel; do not make this look like an official Marmot client.
- The embedded engine lives in `src/darkmatter_app.rs`; the chat client in `src/dm.rs`; the agent-text-stream lifecycle in `src/dm_streams.rs`. The legacy Marmot v1 / White Noise CLI integration was removed in v0.2.0. See [docs/darkmatter.md](docs/darkmatter.md).
