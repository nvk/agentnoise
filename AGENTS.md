# AgentNoise

This is a Rust CLI/daemon project.

- Keep the trusted bridge path native: Rust plus the local launcher policy.
- Do not add Node, npm, bun, Electron, or Tauri to the daemon path.
- Agent execution must go through `bondage` profiles.
- Chat commands must map to structured argv arrays, never shell-concatenated strings.
- Treat White Noise as the transport/control channel; do not make this look like an official White Noise client.
