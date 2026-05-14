# agentnoise

```text
░█▀█░█▀▀░█▀▀░█▀█░▀█▀░█▀█░█▀█░▀█▀░█▀▀░█▀▀
░█▀█░█░█░█▀▀░█░█░░█░░█░█░█░█░░█░░▀▀█░█▀▀
░▀░▀░▀▀▀░▀▀▀░▀░▀░░▀░░▀░▀░▀▀▀░▀▀▀░▀▀▀░▀▀▀
```

Chat with local coding agents through White Noise.

`agentnoise` is a native desktop helper for using a phone running White Noise as the control surface for local Codex and Claude sessions. It is intentionally Rust-first and keeps Node/npm/bun out of the trusted bridge path.

agentnoise exists because the available agent-chat bridges were too heavy,
too slow-moving, or too awkward for the simple workflow this project needs: a
native desktop helper, a phone chat UI, strong first-pairing, and local coding
agents launched through a [local policy boundary](https://agentbondage.org/).

The less polite design brief: I had to build it because everything else sucks
and Jeff moves too slow.

such alpha, much wow.

## What It Does

agentnoise listens to one or more White Noise chats and accepts a small command set from allowlisted senders. It launches local coding agents through [`bondage`](https://agentbondage.org/), stores job state and logs locally, and posts results back into the same White Noise chat that sent the command. Each White Noise group id gets its own workspace state, so the same phone user can keep multiple independent agentnoise sessions open in separate chat windows.

The first supported target is macOS.

## Requirements

- Rust toolchain for building from source
- `bondage` with `codex` and `claude` profiles
- Codex CLI and Claude Code CLI
- `wn` and `wnd` from `marmot-protocol/whitenoise-rs`, either packaged beside `agentnoise` or installed with `agentnoise whitenoise install`
- OS keychain access if using automatic White Noise login repair
- a dedicated White Noise group for agent control

## Security Stack

agentnoise is the phone and White Noise control plane. It does not try to make
remote chat messages safe by itself; it keeps command parsing small, requires a
first-pairing PIN before trusting a sender, and hands local execution to the
agent security stack.

The intended stack is:

- [White Noise](https://www.whitenoise.chat/) carries the phone chat and desktop identity discovery.
- The OS keychain stores the dedicated desktop helper `nsec`; `config.toml` stores only public identity and runtime configuration.
- [`bondage`](https://agentbondage.org/) is the local launcher/policy boundary for Codex, Claude, and other agent profiles. It keeps launch decisions explicit: pinned target, expected hash, configured args, and selected sandbox profile.
- [`envchain-xtra`](https://envchain-xtra.org/) can be used under bondage when an agent profile needs explicit secret release instead of ambient shell environment.
- [`nono`](https://nono.sh/) can provide the OS sandbox layer used by bondage profiles.
- [Learn to Prompt](https://learntoprompt.org/guides/agent-stack.html) is the living operator guide for the larger local agent stack, sandbox profiles, prompt/workflow conventions, and vendor-independent setup notes.

In short: the phone can ask for work, agentnoise authenticates and routes the
request, and the local stack decides what the agent process is actually allowed
to touch.

## First Run

Install from Homebrew:

```sh
brew install nvk/tap/agentnoise
```

Then start it. For normal desktop use, run it as a Homebrew service:

```sh
brew services start nvk/tap/agentnoise
```

For a foreground test run, use:

```sh
agentnoise up
```

Both paths run the same listener. On first run, agentnoise creates the desktop
identity, stores its `nsec` in the OS keychain, starts White Noise, publishes
the profile/key package, and opens the pairing window.

Pair the phone:

1. Look for the macOS `agentnoise pairing` window.
2. Scan the QR from White Noise on the phone.
3. Create a White Noise chat/group with the agentnoise desktop identity.
4. Type the 6-digit PIN shown on the desktop as the first phone message.
5. Send `/status`, then `/help`.

If the pairing window is hidden or blocked, the same QR/PIN is in the service
logs:

```sh
tail -f "$(brew --prefix)/var/log/agentnoise.log"
tail -f "$(brew --prefix)/var/log/agentnoise.err.log"
```

The service is expected to start even before the White Noise chat exists. It
waits, keeps showing a rotating PIN while pairing is required, and discovers
the phone-created chat automatically. During first pairing it also reads a small
initial message window, so a PIN sent right after chat creation can still be
accepted.

Build from source:

```sh
cargo build --release
target/release/agentnoise up
```

The QR contains the desktop `nprofile`/`npub` plus relay hints. It never exposes
the desktop `nsec`.

If you already have the phone identity `npub`, agentnoise can create the White
Noise control chat too:

```sh
target/release/agentnoise up --phone npub...
```

Otherwise scan the QR, create a White Noise chat/group with the desktop
identity, and leave `agentnoise up` running. If you used `--no-listen` or
stopped the process, start it again:

```sh
target/release/agentnoise up
```

To open another independent session, create another White Noise chat with the
same agentnoise desktop identity. The running listener discovers visible chats
periodically; each chat has separate `/use`, `/cd`, and prompt context.

If `wn`/`wnd` are not already packaged beside `agentnoise`, install them under agentnoise's managed data directory:

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

Each White Noise chat is one agentnoise session. `/new bugfix-ui` creates a new
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

The agentnoise `npub` is public on relays, so first-run command authorization is
separate from discovery. When `allowed_senders` is empty, `agentnoise up` enters
pairing mode:

1. Desktop shows a QR for the agentnoise `nprofile`.
2. On macOS, agentnoise opens a pairing window with the QR, the desktop `npub`,
   a live countdown, and the current 6-digit PIN.
3. The phone sends the PIN as the first message, either `123456` or `/pair 123456`.
4. agentnoise stores that sender in `allowed_senders`.
5. All other messages are ignored until this succeeds.

The PIN also prints to stdout/stderr logs for headless and non-macOS setups. It
rotates on `whitenoise.pairing_pin_seconds`, which defaults to 30 seconds.

## Security Defaults

- Use a dedicated White Noise bot identity for the desktop helper.
- Put only trusted devices/users in agentnoise control chats.
- Keep first pairing local: the sender must prove they can see the desktop PIN.
- Keep repos as configured aliases.
- Keep agent execution behind [`bondage`](https://agentbondage.org/).
- Store the bot `nsec` in the OS keychain, not in `config.toml`.

## More

- [White Noise setup](docs/whitenoise.md)
- [Local bring-up](docs/local-bringup.md)
- [Supervisor services](docs/services.md)
- [Launchd service](docs/launchd.md)
- [Homebrew packaging](docs/homebrew.md)
- [Learn to Prompt agent stack](https://learntoprompt.org/guides/agent-stack.html)
- [Bondage local launcher](https://agentbondage.org/)

## License

MIT License. Copyright (c) 2026 nvk.

This software is provided as-is, without warranty of any kind.
