# AgentNoise

Chat with local coding agents through White Noise.

`agentnoise` is a native desktop helper for using a phone running White Noise as the control surface for local Codex and Claude sessions. It is intentionally Rust-first and keeps Node/npm/bun out of the trusted bridge path.

AgentNoise exists because the available agent-chat bridges were too heavy,
too slow-moving, or too awkward for the simple workflow this project needs: a
native desktop helper, a phone chat UI, strong first-pairing, and local coding
agents launched through a constrained policy boundary.

The less polite design brief: I had to build it because everything else sucks
and Jeff moves too slow.

## What It Does

AgentNoise listens to one or more White Noise chats and accepts a small command set from allowlisted senders. It launches local coding agents through `bondage`, stores job state and logs locally, and posts results back into the same White Noise chat that sent the command. Each White Noise group id gets its own workspace state, so the same phone user can keep multiple independent AgentNoise sessions open in separate chat windows.

The first supported target is macOS.

## Requirements

- Rust toolchain for building from source
- `bondage` with `codex` and `claude` profiles
- Codex CLI and Claude Code CLI
- `wn` and `wnd` from `marmot-protocol/whitenoise-rs`, either packaged beside `agentnoise` or installed with `agentnoise whitenoise install`
- OS keychain access if using automatic White Noise login repair
- a dedicated White Noise group for agent control

## Quick Start

Install from Homebrew:

```sh
brew install nvk/tap/agentnoise
agentnoise up
```

Or build from source:

```sh
cargo build --release
target/release/agentnoise up
```

`up` writes the config, creates or reuses the desktop keypair in the OS
keychain, starts White Noise, logs in, publishes the desktop profile/key package,
enables keychain login repair, prints the phone pairing QR when pairing is still
needed, discovers visible control chats when White Noise can see them, then
listens.
The QR contains the desktop `nprofile`/`npub` plus relay hints. It never exposes
the desktop `nsec`.

If you already have the phone identity `npub`, AgentNoise can create the White
Noise control chat too:

```sh
target/release/agentnoise up --phone npub...
```

Otherwise scan the QR, create a White Noise chat/group with the desktop
identity, then rerun:

```sh
target/release/agentnoise up
```

To open another independent session, create another White Noise chat with the
same AgentNoise desktop identity. The running listener discovers visible chats
periodically; each chat has separate `/use`, `/cd`, and prompt context.

If `wn`/`wnd` are not already packaged beside `agentnoise`, install them under AgentNoise's managed data directory:

```sh
target/release/agentnoise whitenoise install
```

Install as a user LaunchAgent:

```sh
target/release/agentnoise service install --target launchd --force --load
```

If first pairing is still required, macOS shows a pairing window with the
desktop identity QR, current PIN, and live countdown. The same PIN is also
printed to the terminal or service log.

## Chat Commands

- `/status`
- `/new [name]`
- `/rename [name]`
- `/list`
- `/resume <number|name|id>`
- `/close`
- `/repos`
- `/use <repo>`
- `/pwd`
- `/ls [path]`
- `/cd <path>`
- `/codex <prompt>`
- `/codex <repo> <prompt>`
- `/codex-resume <session> <prompt>`
- `/claude <prompt>`
- `/claude <repo> <prompt>`
- `/claude-resume <session> <prompt>`
- `/wiki <prompt>`
- `/codex-wiki <prompt>`
- `/claude-wiki <prompt>`
- `/jobs`
- `/tail <job>`
- `/cancel <job>`
- `/help`

Each White Noise chat is one AgentNoise session. `/new bugfix-ui` creates a new
parallel White Noise chat with the paired phone identity and clones the current
workspace into it. `/rename main` names the current chat, `/list` shows known
sessions, `/resume 2` resumes a session from that list, and `/close` marks the
current session closed locally. `/sessions` remains accepted as a readable alias
for `/list`.

Repos are aliases from the config, not arbitrary paths. `/use` selects a repo
for the session, `/cd ..` moves within that selected repo, and plain `/codex` or
`/claude` uses the selected workspace. `/wiki` follows the local Codex
`codex-wiki` convention by prefixing `@wiki`; `/claude-wiki` sends a `wiki ...`
prompt for Claude installations with the LLM Wiki instructions/plugin available.

## First Pairing

The AgentNoise `npub` is public on relays, so first-run command authorization is
separate from discovery. When `allowed_senders` is empty, `agentnoise up` enters
pairing mode:

1. Desktop shows a QR for the AgentNoise `nprofile`.
2. On macOS, AgentNoise opens a pairing window with the QR, the desktop `npub`,
   a live countdown, and the current 6-digit PIN.
3. The phone sends the PIN as the first message, either `123456` or `/pair 123456`.
4. AgentNoise stores that sender in `allowed_senders`.
5. All other messages are ignored until this succeeds.

The PIN also prints to stdout/stderr logs for headless and non-macOS setups. It
rotates on `whitenoise.pairing_pin_seconds`, which defaults to 30 seconds.

## Security Defaults

- Use a dedicated White Noise bot identity for the desktop helper.
- Put only trusted devices/users in AgentNoise control chats.
- Keep first pairing local: the sender must prove they can see the desktop PIN.
- Keep repos as configured aliases.
- Keep agent execution behind `bondage`.
- Store the bot `nsec` in the OS keychain, not in `config.toml`.

## More

- [White Noise setup](docs/whitenoise.md)
- [Local bring-up](docs/local-bringup.md)
- [Supervisor services](docs/services.md)
- [Launchd service](docs/launchd.md)
- [Homebrew packaging](docs/homebrew.md)
