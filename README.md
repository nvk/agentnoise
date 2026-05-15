# agentnoise

```text
░█▀█░█▀▀░█▀▀░█▀█░▀█▀░█▀█░█▀█░▀█▀░█▀▀░█▀▀
░█▀█░█░█░█▀▀░█░█░░█░░█░█░█░█░░█░░▀▀█░█▀▀
░▀░▀░▀▀▀░▀▀▀░▀░▀░░▀░░▀░▀░▀▀▀░▀▀▀░▀▀▀░▀▀▀
```

Chat with local coding agents through White Noise.

`agentnoise` is a native desktop helper for using a phone running White Noise as the control surface for local Codex, Claude, and optional Hermes sessions. It is intentionally Rust-first and keeps Node/npm/bun out of the trusted bridge path.

agentnoise exists because the available agent-chat bridges were too heavy,
too slow-moving, or too awkward for the simple workflow this project needs: a
native desktop helper, a phone chat UI, strong first-pairing, and local coding
agents launched through a [local policy boundary](https://agentbondage.org/).

The less polite design brief: I had to build it because everything else sucks
and Jeff moves too slow.

such alpha, much wow.

## Changelog

**Unreleased** - **Optional direct agent launcher and cleaner session UX.**
`bondage` remains the default and recommended local policy boundary, but
first-run commands can now opt into `runner.launcher = "direct"` with
`--direct-agents`. Direct mode runs raw `codex`, `claude`, or optional
`hermes` CLIs with structured argv and skips the `bondage` binary/config
checks in `agentnoise doctor`. Session replies now spell out current chat,
target chat, workspace, and next command more clearly for phone use.

**v0.1.13** - **White Noise login detection fix.** `wn whoami` reports logged
in accounts as hex pubkeys while agentnoise config stores the desktop account
as `npub`. The startup check now normalizes both forms, so agentnoise only
skips Keychain repair when the actual White Noise signing account is present.

**v0.1.12** - **Service keychain startup fix.** `agentnoise up` now reuses the
cached desktop `npub` from config for QR/profile setup instead of loading the
desktop `nsec` on every service restart. Homebrew services should only touch
the OS keychain when creating the identity or repairing a missing White Noise
login. Keychain repair errors now explain the Terminal authorization step.

**v0.1.11** - **Pairing identity fix.** Normalized Nostr sender identity
checks across hex pubkeys and `npub` values. This stops agentnoise from
treating its own White Noise replies as unpaired inbound messages when the
desktop identity is stored as `npub` but White Noise reports authors as hex.
Allowed phone senders also match across both forms.

**v0.1.10** - **Reliability and identity polish.** Bare text, unknown
commands, unauthorized senders, and startup catch-up events now get explicit
phone replies instead of silent drops. Added reply send retries,
`agentnoise identity status`, `agentnoise identity rename <name>`, and support
for the common typo `agentnoise -- help`. Added configured White Noise message
relays and `agentnoise whitenoise relays`/`ensure-relays` so message delivery
can use a broader account relay set instead of only the QR discovery hints.

**v0.1.9** - **Remote pairing polish.** Added SSH pairing mode with
terminal-only PIN display, `--name` for per-machine White Noise/Nostr profile
labels, and a remote SSH guide that passes the phone `npub` without moving any
`nsec` over SSH.

**v0.1.8** - **Parity hardening pass.** Added a fake phone harness for
isolated White Noise testing, durable runtime event journaling, `agentnoise
status`, progress messages from Codex/Claude JSON streams, approval replay for
risky local profiles, attachment metadata capture, a direct `wnd` socket probe,
`/agents`, opt-in git worktree sessions, and local release smoke checks.

**v0.1.7** - **Optional Hermes backend.** Added `/hermes` and
`/hermes-resume` as disabled-by-default commands that route Hermes through the
same `bondage` local policy boundary used for Codex and Claude. Command
parsing, config compatibility, command construction, doctor output, and
packaging are tested; live Hermes CLI/runtime execution is still alpha and
untested.

**v0.1.6** - **Service console and dev burner identity.** `agentnoise up` can
attach to an already-running Homebrew service as a local console instead of
starting a second listener, and disposable development identities can use
`--dev-burner-nsec` without repeated OS keychain prompts.

## What It Does

agentnoise listens to one or more White Noise chats and accepts a small command set from allowlisted senders. It launches local coding agents through the configured launcher, [`bondage`](https://agentbondage.org/) by default or direct raw CLIs by explicit opt-in, stores job state and logs locally, and posts results back into the same White Noise chat that sent the command. Each White Noise group id gets its own workspace state, so the same phone user can keep multiple independent agentnoise sessions open in separate chat windows.

The most tested target is macOS. Linux and FreeBSD service templates are
included and should be treated as newer paths.

## Requirements

- Rust toolchain for building from source
- recommended/default: `bondage` with `codex-agentnoise` and `claude-agentnoise` profiles
- Codex CLI and Claude Code CLI
- optional: Hermes Agent CLI plus a dedicated `bondage` profile, or direct Hermes mode
- `wn` and `wnd` from `marmot-protocol/whitenoise-rs`, either packaged beside `agentnoise` or installed with `agentnoise whitenoise install`
- OS keychain access if using automatic White Noise login repair with a real identity
- a dedicated White Noise group for agent control

agentnoise launches coding agents through dedicated `bondage` profiles by
default: `codex-agentnoise`, `claude-agentnoise`, and optional
`hermes-agentnoise`. If an older config still says `profile = "codex"` or
`profile = "claude"`, agentnoise uses the matching `*-agentnoise` profile for
remote chat runs unless `runner.allow_generic_agent_profiles = true` is set.
This keeps phone-triggered jobs separate from human terminal profiles and gives
the launcher a place to pin sandbox, secrets, and non-interactive behavior.

For a minimal install that only has raw Codex or Claude, opt into direct mode:

```sh
agentnoise init --direct-agents
```

Or set it during first setup:

```sh
agentnoise up --direct-agents --no-listen
```

That writes:

```toml
[runner]
launcher = "direct"
```

Direct mode does not require `bondage` at runtime. It still uses structured argv
and the same White Noise pairing/allowlist, but local filesystem, network,
secret, and approval behavior is whatever the raw agent CLI enforces. Use
`agentnoise doctor` or `agentnoise agents` to confirm the active launcher.

Existing installs can switch explicitly:

```sh
agentnoise config launcher direct
agentnoise config launcher bondage
```

## Security Stack

agentnoise is the phone and White Noise control plane. It does not try to make
remote chat messages safe by itself; it keeps command parsing small, requires a
first-pairing PIN before trusting a sender, and hands local execution to the
agent security stack.

The intended stack is:

- [White Noise](https://www.whitenoise.chat/) carries the phone chat and desktop identity discovery.
- The OS keychain stores the dedicated desktop helper `nsec` for normal use; `config.toml` stores only public identity and runtime configuration.
- [`bondage`](https://agentbondage.org/) is the local launcher/policy boundary for Codex, Claude, and other agent profiles. It keeps launch decisions explicit: pinned target, expected hash, configured args, and selected sandbox profile.
- [`envchain-xtra`](https://envchain-xtra.org/) can be used under bondage when an agent profile needs explicit secret release instead of ambient shell environment.
- [`nono`](https://nono.sh/) can provide the OS sandbox layer used by bondage profiles.
- [Learn to Prompt](https://learntoprompt.org/guides/agent-stack.html) is the living operator guide for the larger local agent stack, sandbox profiles, prompt/workflow conventions, and vendor-independent setup notes.

In short: the phone can ask for work, agentnoise authenticates and routes the
request, and the local stack decides what the agent process is actually allowed
to touch. In direct mode that local stack is just the raw agent CLI and its own
permissions model.

## First Run

Install from Homebrew:

```sh
brew install nvk/tap/agentnoise
```

The Homebrew formula installs the same binary either way. Choose the local
agent launcher in agentnoise config:

```sh
# recommended policy boundary
agentnoise init

# minimal raw Codex/Claude mode
agentnoise init --direct-agents
```

For an existing config:

```sh
agentnoise config launcher direct
```

Then start it. For normal desktop use, run it as a Homebrew service:

```sh
brew services start nvk/tap/agentnoise
```

Then open the local console:

```sh
agentnoise up
```

`agentnoise up` is safe to run while the Homebrew service is enabled. If the
service already owns the listener, `up` attaches as the terminal UI and follows
the service logs. If no listener is running, `up` takes the foreground lock and
runs the same engine itself. Non-interactive service starts wait for that lock,
so the service can take over after a foreground troubleshooting run exits.

On first run, agentnoise creates the desktop identity, stores its `nsec` in the
OS keychain, starts White Noise, publishes the profile/key package, and opens
the pairing window.

After the desktop `npub` is written to config, service restarts use that public
identity for QR/profile setup and avoid reading the `nsec` unless White Noise
login repair is needed. The listener still requires a logged-in White Noise
signing account before it can send replies. If macOS blocks background Keychain
access during repair, authorize it from Terminal once:

```sh
agentnoise keychain status
```

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

Useful local diagnostics:

```sh
agentnoise status
agentnoise doctor
agentnoise agents
agentnoise identity status
agentnoise identity rename agentnoise-mbp
agentnoise fake-phone plan
```

Remote SSH pairing:

```sh
brew services stop nvk/tap/agentnoise
agentnoise up --ssh --phone npub1... --name agentnoise-mbp
```

In `--ssh` mode, agentnoise prints the pairing PIN in the SSH session and does
not open a desktop GUI alert. Pass the phone `npub`, not an `nsec`; the remote
machine creates and stores its own desktop identity locally. Use a distinct
`--name` per machine so White Noise can show `agentnoise-mbp`,
`agentnoise-linuxbox`, or whatever label makes sense.

After setup, check or change the published machine label without passing any
phone identity:

```sh
agentnoise identity status
agentnoise identity rename agentnoise-linuxbox
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

Pairing relay hints are for discovery. Message delivery uses the White Noise
account relay list. agentnoise now keeps a broader message relay list in config
and reconciles it through `wn relays add` after login:

```toml
[whitenoise]
message_relays = [
  "wss://index.hzrd149.com",
  "wss://indexer.coracle.social",
  "wss://relay.primal.net",
  "wss://relay.damus.io",
  "wss://relay.ditto.pub",
  "wss://nos.lol",
  "wss://relay.nostr.band",
  "wss://relay.snort.social",
  "wss://relay.nostr.bg",
  "wss://nostr.mom",
]
```

Inspect or apply those relays manually:

```sh
agentnoise whitenoise relays
agentnoise whitenoise ensure-relays
```

### Development Burner Identity

For local development and throwaway relay testing, skip OS keychain prompts:

```sh
agentnoise up --dev-burner-nsec
```

That creates a burner `nsec` at
`~/Library/Application Support/agentnoise/dev-burner.nsec`, sets
`whitenoise.dev_burner_nsec = true`, and makes future `agentnoise up` or
Homebrew service runs use that file instead of the OS keychain. This is
plaintext by design. Do not use it for a real phone identity or a production
desktop helper.

This flag bypasses the agentnoise keychain store. White Noise still owns its
own daemon account store after `wn login`; on platforms where that store is
unavailable, `agentnoise doctor` or startup will show the upstream White Noise
login error explicitly.

If you already have the phone identity `npub`, agentnoise can create the White
Noise control chat too:

```sh
target/release/agentnoise up --phone npub... --name agentnoise-mbp
```

Otherwise scan the QR, create a White Noise chat/group with the desktop
identity, and leave the listener running. If you used `--no-listen` or stopped
the process, start or attach again:

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

## Linux Quick Start

Build or install `agentnoise`, then put it on the user `PATH`:

```sh
cargo build --release
install -Dm755 target/release/agentnoise ~/.local/bin/agentnoise
```

Install the bundled White Noise CLI tools if `wn` and `wnd` are not already on
the service user's `PATH`:

```sh
agentnoise whitenoise install
```

Run the first pairing in the foreground. On Linux the QR and rotating PIN are
printed to the terminal/logs:

```sh
agentnoise up
```

After the phone is paired, install a user systemd service:

```sh
agentnoise service install --target systemd-user --force --load
systemctl --user status agentnoise.service
journalctl --user -u agentnoise.service -f
```

For boot without an active login session, enable lingering:

```sh
loginctl enable-linger "$USER"
```

Secret storage on Linux uses keyutils plus Secret Service persistence. Headless
servers need the same user service context to reach an unlocked Secret Service
collection; test that before relying on unattended restart:

```sh
agentnoise keychain status
```

## FreeBSD Quick Start

Build or install `agentnoise`, then install the binary somewhere in the service
`PATH`:

```sh
cargo build --release
sudo install -m 0755 target/release/agentnoise /usr/local/bin/agentnoise
```

Create the config and do first pairing as the user that should own the helper:

```sh
agentnoise whitenoise install
agentnoise up
```

Then install the rc.d service using that user's config path:

```sh
CONFIG="$(agentnoise config path)"
sudo agentnoise --config "$CONFIG" service install --target freebsd-rc --force
sudo sysrc agentnoise_enable=YES
sudo sysrc agentnoise_user="$USER"
sudo sysrc agentnoise_config="$CONFIG"
sudo service agentnoise start
sudo service agentnoise status
```

For service logs, use the normal rc/daemon logs for the host, commonly:

```sh
tail -f /var/log/messages
```

FreeBSD uses DBus Secret Service for real `nsec` storage. Confirm the service
account can read the stored secret before depending on restart repair:

```sh
agentnoise keychain status
```

For disposable relay testing only, `agentnoise up --dev-burner-nsec` uses a
plaintext burner identity and avoids Secret Service setup.

## Chat Commands

- `/status`
- `/agents`
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
- `/hermes <prompt>`
- `/hermes <repo> <prompt>`
- `/hermes-resume <session> <prompt>`
- `/wiki <prompt>`
- `/codex-wiki <prompt>`
- `/claude-wiki <prompt>`
- `/jobs`
- `/tail <job>`
- `/cancel <job>`
- `/approvals`
- `/approve <approval>`
- `/deny <approval>`
- `/attachments`
- `/attach <number|id>`
- `/worktrees`
- `/worktree new <name>`
- `/worktree use <name>`
- `/worktree remove <name> confirm`
- `/help`

Each White Noise chat is one agentnoise session. `/new bugfix-ui` creates a new
parallel White Noise chat with the paired phone identity and clones the current
workspace into it. `/rename main` names the current chat, `/list` shows known
sessions, `/resume 2` resumes a session from that list, and `/close` marks the
current session closed locally. `/sessions` remains accepted as a readable alias
for `/list`.

Repos are aliases from the config, not arbitrary paths. `/use` selects a repo
for the session, `/cd ..` moves within that selected repo, and plain `/codex` or
`/claude` uses the selected workspace. `/hermes` does the same when Hermes is
enabled. `/wiki` follows the local Codex `codex-wiki` convention by prefixing
`@wiki`; `/claude-wiki` sends a `wiki ...` prompt for Claude installations with
the LLM Wiki instructions/plugin available.

Plain text and unknown slash commands are not executed, but they are answered.
The reply points the user at `/help` and `/codex <prompt>` so a mistyped phone
message does not look like a dead daemon.

Codex and Claude JSON streams are converted into occasional progress messages
while a job runs. The default interval is conservative
(`runner.progress_interval_seconds = 15`) so the phone chat does not become
unreadable. Final job output still arrives as one normal reply, with `/tail
<job>` for logs.

If a configured agent profile looks intentionally elevated, for example a
profile or permission mode containing `unsafe`, agentnoise creates an approval
object instead of launching immediately. Approvals are bound to the same White
Noise chat that requested them and expire after
`runner.approval_ttl_seconds`.

Images and files sent by the phone are not handed to coding agents yet.
agentnoise saves the metadata it can see, replies with an attachment id, and
lets the user inspect it with `/attachments` and `/attach <id>`.

Git worktrees are opt-in per chat. `/worktree new fix-ui` creates a git
worktree under the configured `runner.worktree_dir`, switches only that chat to
the new path, and keeps other White Noise sessions on their existing
workspaces. Removal requires the explicit `confirm` word.

## Fake Phone Testing

`agentnoise fake-phone` starts from a separate White Noise daemon data
directory, keeps a throwaway phone `nsec` in the fake-phone test root, creates
a chat with the desktop agentnoise identity, and sends test messages without
touching the real phone identity or the normal agentnoise keychain.

```sh
agentnoise fake-phone plan
agentnoise fake-phone roundtrip --pin 123456 /status
agentnoise fake-phone roundtrip /help
```

For first-pairing tests, pass the current desktop PIN. After pairing, omit
`--pin`. The harness resends the test message for the timeout window because a
running agentnoise service may need one discovery cycle before subscribing to
the newly-created chat.

## Transport Notes

The default White Noise transport remains the tested `wn` CLI path. Setting
`whitenoise.socket` points `wn` at a specific `wnd` daemon socket. The
experimental direct socket adapter is exposed as a probe only:

```sh
agentnoise whitenoise socket-probe --method ping
```

This keeps production message send/subscribe behavior on the stable upstream
CLI while giving us a tested JSON-line Unix socket client for future direct
`wnd` work.

### Maybe Later: Local Agent Session Visibility

agentnoise currently shows only sessions and jobs it owns. A future opt-in
local visibility mode could expose metadata for Codex, Claude, or Hermes
sessions that were started elsewhere on the same machine, then let the phone
explicitly import one into the current White Noise chat. The default should
remain conservative: metadata first, no transcript scraping, no process/env
inspection, and no silent exposure of unrelated local agent work.

## Optional Hermes Support

Hermes support is disabled by default. agentnoise does not run the Hermes Agent
gateway and does not expose a second remote API. It launches the Hermes CLI as a
local backend through `bondage`, the same way it launches Codex and Claude.

Enable it in `~/.config/agentnoise/config.toml` after installing Hermes and
creating a dedicated `bondage` profile:

```toml
[agents.hermes]
enabled = true
profile = "hermes-agentnoise"
bin = "hermes"
```

Then restart the listener:

```sh
brew services restart nvk/tap/agentnoise
```

From White Noise:

```text
/hermes summarize this repo
/hermes-resume <session> continue
```

The command shape is intentionally narrow:

```sh
bondage exec hermes-agentnoise ~/.config/bondage/bondage.conf -- hermes chat --quiet --source agentnoise --toolsets skills -q "<prompt>"
```

Use the `bondage` profile to set a dedicated `HERMES_HOME`, model endpoint
environment, filesystem policy, and any local secrets release rules. Start with
restricted toolsets and widen policy only after the local profile is behaving.

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

The PIN also prints to stdout/stderr logs for headless and non-macOS setups.
While the listener is running, `agentnoise pair` prints the same live PIN in the
terminal alongside the QR. It rotates on `whitenoise.pairing_pin_seconds`, which
defaults to 30 seconds.

## Security Defaults

- Use a dedicated White Noise bot identity for the desktop helper.
- Put only trusted devices/users in agentnoise control chats.
- Keep first pairing local: the sender must prove they can see the desktop PIN.
- Keep repos as configured aliases.
- Keep agent execution behind [`bondage`](https://agentbondage.org/).
- Store the bot `nsec` in the OS keychain for normal use, not in `config.toml`.
- Use `--dev-burner-nsec` only for throwaway development identities.

## More

- [White Noise setup](docs/whitenoise.md)
- [Local bring-up](docs/local-bringup.md)
- [Remote SSH pairing](docs/remote-ssh.md)
- [Fake phone testing](docs/fake-phone-testing.md)
- [Testing](docs/testing.md)
- [Supervisor services](docs/services.md)
- [Launchd service](docs/launchd.md)
- [Homebrew packaging](docs/homebrew.md)
- [Release notes](docs/release-notes.md)
- [Learn to Prompt agent stack](https://learntoprompt.org/guides/agent-stack.html)
- [Bondage local launcher](https://agentbondage.org/)

## License

MIT License. Copyright (c) 2026 nvk.

This software is provided as-is, without warranty of any kind.
