# agentnoise

This is a Rust CLI/daemon project.

- Keep the trusted bridge path native: Rust plus the local launcher policy.
- Do not add Node, npm, bun, Electron, or Tauri to the daemon path.
- Agent execution must go through the configured launcher. Keep the `bondage`
  profile path working and explicit for hardened/local installs, and keep the
  direct launcher simple for public first-run setup.
- Chat commands must map to structured argv arrays, never shell-concatenated strings.
- Treat White Noise as the transport/control channel; do not make this look like an official White Noise client.
