# agentnoise

```text
░█▀█░█▀▀░█▀▀░█▀█░▀█▀░█▀█░█▀█░▀█▀░█▀▀░█▀▀
░█▀█░█░█░█▀▀░█░█░░█░░█░█░█░█░░█░░▀▀█░█▀▀
░▀░▀░▀▀▀░▀▀▀░▀░▀░░▀░░▀░▀░▀▀▀░▀▀▀░▀▀▀░▀▀▀
```

Chat with local coding agents over Marmot v2.

`agentnoise` is a native desktop helper that lets a phone running a Marmot v2 client drive local Codex, Claude, and optional Hermes sessions. It's intentionally Rust-first and keeps Node/npm/bun out of the trusted bridge path.

As of **v0.2.0** agentnoise embeds the Marmot v2 protocol stack (the [`darkmatter`](https://github.com/marmot-protocol/darkmatter) `marmot-app` crate) directly — no more `wn` / `wnd` subprocess install. See [docs/darkmatter.md](docs/darkmatter.md) for the integration architecture and [docs/release-notes.md](docs/release-notes.md) for the changelog.

agentnoise exists because the available agent-chat bridges were too heavy,
too slow-moving, or too awkward for the simple workflow this project needs: a
native desktop helper, a phone chat UI, strong first-pairing, and local coding
agents launched through a [local policy boundary](https://agentbondage.org/).

The less polite design brief: I had to build it because everything else sucks
and Jeff moves too slow.

such alpha, much wow.

## Changelog

**v0.2.0 (Dark Matter alpha)** - **Marmot v2 transport plus mainline parity.**
agentnoise now embeds the Dark Matter/Marmot v2 Rust stack instead of spawning
`wn`/`wnd`, keeps transport and worker roles split, and has parity with the
latest mainline phone UX: quiet progress mode, compact finals, active-job
follow-up guards, full supported media ingest into workspace
`.agentnoise/attachments/`, `/wiki` file-ingestion context, and automatic media
uploads for agent-created workspace files.

**v0.1.24** - **Mobile chat cleanup and opt-in local session watch.** White
Noise replies are shorter and more phone-readable: compact startup hellos,
queue/final job replies, progress pings, status, help, session lists, and local
agent session metadata. Job ids and local session ids can be referenced by
short unique prefixes for `/tail`, `/cancel`, and `*-resume` commands. Added
the opt-in local session watcher so `agentnoise config local-sessions-watch on`
can notify the primary paired chat when same-account Codex/Claude metadata
appears, while staying off by default for privacy.

**v0.1.23** - **Inbox sessions and stale group cleanup.** The primary paired
chat now acts as an inbox: new `/codex`, `/claude`, `/hermes`, and `/wiki`
jobs from that chat open a fresh White Noise work session named from the
hostname and a short prompt summary, then progress and final output continue
there. The fake-phone harness follows those handoff links, session open links
are shown with shorter refs, and startup reconciles saved control chats against
active White Noise groups so removed chats stop looking live.

**v0.1.22** - **Service-mode and White Noise delivery fix.** Codex jobs now
fail fast with a clear explanation when launched from macOS launchd/brew
service contexts that make Codex hang before producing output. Direct Codex
launches use a stable agentnoise data dir while still passing the selected
workspace through `codex -C`, startup has a no-output watchdog/retry, removed
White Noise groups are ignored, pending paired control chats are accepted, and
reply send failures no longer kill the listener.

**v0.1.21** - **Quiet-job chat pings.** Running jobs now send a visible
`still running` message when Codex, Claude, Hermes, or the launcher stays alive
but produces no new output. The ping includes `/tail <job>` and `/cancel <job>`
so a phone user can inspect or stop the job instead of staring at an accepted
command with no follow-up.

**v0.1.20** - **Fake-phone E2E and Codex launch fix.** The fake-phone harness
now waits for real command replies and final job output instead of passing on
startup hellos or unrelated auth replies. Codex jobs also pass
`--skip-git-repo-check`, which fixes phone-launched jobs in configured
workspaces that are plain directories.

**v0.1.16** - **Startup hello.** Once the listener is up, already-paired
control chats get a timestamped `agentnoise is up` message with profile and
workspace context. First-pairing mode stays quiet until the PIN succeeds.

**v0.1.15** - **Homebrew service keychain prompt fix.** Service startup now
waits longer for `wnd` to become ready and avoids a redundant second daemon
startup check after setup. This prevents launchd keep-alive loops from
repeatedly touching the macOS Keychain when White Noise startup is slow.

**v0.1.14** - **Optional direct launcher and reliability polish.**
`bondage` remains the default and recommended local policy boundary, but
first-run commands can now opt into `runner.launcher = "direct"` with
`--direct-agents`. Direct mode runs raw `codex`, `claude`, or optional
`hermes` CLIs with structured argv and skips the `bondage` binary/config
checks in `agentnoise doctor`. Session replies now spell out current chat,
target chat, workspace, and next command more clearly for phone use.
Live fake-phone tests and service diagnostics were tightened, and `status` /
`doctor` avoid slow implicit keychain probes. A local White Noise send can still
arrive on the phone late; the docs now call out how to separate local send
success from phone sync/display lag.

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

agentnoise listens to one or more Marmot v2 chats and accepts a small command set from allowlisted senders. It launches local coding agents through the configured launcher, [`bondage`](https://agentbondage.org/) by default or direct raw CLIs by explicit opt-in, stores job state and logs locally, and posts results back through Marmot v2. The primary paired chat acts like an inbox: starting a new job there creates a new Marmot v2 work session named `hostname - short prompt summary`, then progress and final output continue in that session. Each Marmot group id gets its own workspace state, so the same phone user can keep multiple independent agentnoise sessions open in separate chat windows.

The most tested target is macOS. Linux and FreeBSD service templates are
included and should be treated as newer paths.

## Requirements

- Rust toolchain for building from source
- recommended/default: `bondage` with `codex-agentnoise` and `claude-agentnoise` profiles
- Codex CLI and Claude Code CLI
- optional: Hermes Agent CLI plus a dedicated `bondage` profile, or direct Hermes mode
- The Marmot v2 stack ([`darkmatter`](https://github.com/marmot-protocol/darkmatter)) is embedded as a Cargo dependency — no external `wn` / `wnd` install
- OS keychain access for the desktop helper `nsec` (managed by the embedded engine)
- a dedicated Marmot v2 group for agent control (created from a Marmot v2 phone client)

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
and the same Marmot v2 pairing/allowlist, but local filesystem, network,
secret, and approval behavior is whatever the raw agent CLI enforces. Use
`agentnoise doctor` or `agentnoise agents` to confirm the active launcher.

Existing installs can switch explicitly:

```sh
agentnoise config launcher direct
agentnoise config launcher bondage
```

See [Configuration](docs/configuration.md) for copy/paste examples covering
raw Codex/Claude mode, `bondage` profiles, repo aliases, and extra commands
like `/codex-fix`.

## Security Stack

agentnoise is the phone and Marmot v2 control plane. It does not try to make
remote chat messages safe by itself; it keeps command parsing small, requires a
first-pairing PIN before trusting a sender, and hands local execution to the
agent security stack.

The intended stack is:

- The [Marmot v2 protocol](https://github.com/marmot-protocol/darkmatter) (embedded `marmot-app` crate) carries the phone chat and desktop identity discovery.
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

Then start it. Homebrew services are the simple setup and boot path:

```sh
brew services start nvk/tap/agentnoise
```

The service starts the embedded Marmot v2 engine, ensures the desktop account
is logged in, discovers chats, handles pairing, and keeps non-Codex commands
available after reboot.

For `/codex` jobs on macOS, run the engine from a login shell instead of the
Homebrew/launchd service. Current Codex CLI builds can hang before producing
output when launched directly by launchd:

```sh
brew services stop nvk/tap/agentnoise
agentnoise up
```

Over SSH, keep that foreground engine alive with tmux:

```sh
tmux new -s agentnoise 'agentnoise up'
```

`agentnoise up` is still safe to run while the Homebrew service is enabled. If
the service already owns the listener, `up` attaches as the terminal UI and
follows the service logs. It cannot move `/codex` jobs out of the launchd-owned
service process, so stop the service first when you want phone-launched Codex
jobs on macOS.

On first run, agentnoise creates the desktop identity, stores its `nsec` in the
OS keychain, starts the embedded Marmot v2 engine, publishes the profile/key
package, and opens the pairing window.

After the desktop `npub` is written to config, service restarts use that public
identity for QR/profile setup and avoid reading the `nsec` unless the engine
needs to log in. The listener still requires a logged-in Marmot v2 signing
account before it can send replies. If macOS blocks background Keychain access
during repair, authorize it from Terminal once:

```sh
agentnoise keychain status
```

Pair the phone:

1. Look for the macOS `agentnoise pairing` window.
2. Scan the QR with your Marmot v2 phone client.
3. Create a Marmot v2 chat/group with the agentnoise desktop identity.
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
agentnoise config path
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
`--name` per machine so your Marmot v2 phone client can show `agentnoise-mbp`,
`agentnoise-linuxbox`, or whatever label makes sense.

After setup, check or change the published machine label without passing any
phone identity:

```sh
agentnoise identity status
agentnoise identity rename agentnoise-linuxbox
```

The service is expected to start even before the Marmot v2 chat exists. It
waits, keeps showing a rotating PIN while pairing is required, and discovers
the phone-created chat automatically. During first pairing it also reads a small
initial message window, so a PIN sent right after chat creation can still be
accepted.

Build from source:

```sh
cargo build --release
target/release/agentnoise up
```

The QR scans as the desktop `npub`, which matches the current Dark Matter phone
scanner. The terminal also prints the richer `nprofile` with relay hints for
debugging and future client support. Neither value exposes the desktop `nsec`.

Pairing relay hints are for discovery. Message delivery uses the Marmot v2
account relay list. agentnoise keeps a broader message relay list in config
and publishes it via the embedded engine's `runtime.publish_account_relay_lists`:

```toml
[darkmatter]
agent_text_stream_broker = "https://quic-broker.ipf.dev:4450"
message_relays = [
  "wss://relay.damus.io",
  "wss://relay.primal.net",
  "wss://nos.lol",
]
```

`agent_text_stream_broker` is used for live QUIC previews while an agent is
responding. agentnoise announces the configured broker to Marmot clients as a
`quic://` candidate in the stream start payload.

Smoke-test the embedded engine + relay list:

```sh
agentnoise darkmatter probe --relay wss://relay.primal.net
```

If the phone does not show a reply, first check whether the desktop created one:

```sh
agentnoise status
tail -f "$HOME/Library/Application Support/agentnoise/runtime-events.jsonl"
```

`reply-sent` means agentnoise handed the message to the embedded Marmot v2
engine and stored it locally. The phone can still render it late if the relay
or mobile sync path is delayed. Reopening the chat or restarting the local
`agentnoise up` can flush old replies, but treat that as a transport
diagnostic, not proof that the agent ignored the command.

### Development Burner Identity

For local development and throwaway relay testing, skip OS keychain prompts:

```sh
agentnoise up --dev-burner-nsec
```

That creates a burner `nsec` at
`~/Library/Application Support/agentnoise/dev-burner.nsec`, sets
`darkmatter.dev_burner_nsec = true`, and makes future `agentnoise up` or
Homebrew service runs use that file instead of the OS keychain. This is
plaintext by design. Do not use it for a real phone identity or a production
desktop helper.

This flag bypasses the agentnoise keychain store. The embedded Marmot v2 engine
imports the nsec on startup and manages its own per-account SQLCipher state
under `~/.local/agentnoise/darkmatter/`.

Under Marmot v2 the phone creates the control group (the desktop discovers it
via `MarmotAppEvent::GroupJoined`). Scan the QR with your Marmot v2 phone
client, create a chat/group with the desktop identity, and leave the listener
running. If you used `--no-listen` or stopped
the process, start or attach again:

```sh
target/release/agentnoise up
```

To open another independent session, create another Marmot v2 chat with the
same agentnoise desktop identity. The running listener picks up new groups via
`MarmotAppEvent::GroupJoined` from the embedded engine; each chat has separate
`/use`, `/cd`, and prompt context.

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

Under Marmot v2 the protocol stack is embedded — no external CLI install is
needed.

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
- `/agent-sessions [limit]`
- `/new [name]`
- `/rename [name]`
- `/list`
- `/jump <number|name|id>`
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
- `/download <number|id> [file-number]`
- `/upload <workspace-path> [caption]`
- `/worktrees`
- `/worktree new <name>`
- `/worktree use <name>`
- `/worktree remove <name> confirm`
- `/help`

Each Dark Matter chat is one agentnoise session. The first paired chat is the
inbox. Send `/codex ...`, `/claude ...`, `/hermes ...`, or `/wiki ...` there to
open a fresh work chat; agentnoise names it from the machine hostname and a
2-4 word prompt summary, sends an open link back to the inbox, and posts
quiet progress plus compact final output in the new chat. Follow-up plain text
sent inside that work chat continues with the same agent/profile/wiki mode after
the prior queued job has finished; if a job is still active, agentnoise replies
with `/tail` and `/cancel` hints instead of silently starting a second job.

`/new bugfix-ui` still creates a manual parallel Dark Matter chat with the
paired phone identity and clones the current workspace into it. `/rename main`
names the current chat, `/list` shows known sessions with short chat refs and
open links, `/jump 2` or `/resume 2` resumes a session
from that list, and `/close` marks the current session closed locally.
`/sessions` remains accepted as a readable alias for `/list`.

Repos are aliases from the config, not arbitrary paths. `/use` selects a repo
for the session, `/cd ..` moves within that selected repo, and plain `/codex` or
`/claude` uses the selected workspace. `/hermes` does the same when Hermes is
enabled. `/wiki` follows the local Codex `codex-wiki` convention by prefixing
`@wiki`; `/claude-wiki` sends a `wiki ...` prompt for Claude installations with
the LLM Wiki instructions/plugin available.

Plain text and unknown slash commands are not executed, but they are answered.
The reply points the user at `/help` and `/codex <prompt>` so a mistyped phone
message does not look like a dead daemon.

Codex and Claude JSON streams are converted into occasional live stream
previews while a job runs. The default `runner.progress_mode = "quiet"` keeps
raw tool calls, shell commands, and routine agent self-narration in `/tail`
while still sending approvals, errors, retry notices, and quiet-job pings. If
the QUIC broker is unavailable, agentnoise falls back to chat progress. If a
running job goes quiet, agentnoise sends a "still working" ping after
`runner.silence_ping_seconds = 60`; quiet mode clamps those pings to a
five-minute minimum and includes `/tail <job>` plus `/cancel <job>` hints.
If a new launch emits no output at all for
`runner.startup_silence_timeout_seconds = 90`, agentnoise terminates that
attempt and retries once by default. Final job output still arrives as one
normal reply, compacted for phone when needed, with `/tail <job>` for logs.

If a configured agent profile looks intentionally elevated, for example a
profile or permission mode containing `unsafe`, agentnoise creates an approval
object instead of launching immediately. Approvals are bound to the same chat
that requested them and expire after
`runner.approval_ttl_seconds`.

For Homebrew service use, keep configured repos outside iCloud Drive/CloudDocs.
Codex can hang under launchd before writing output when `-C` points at those
GUI-backed sync folders. `agentnoise doctor` warns about this; run
`agentnoise up` from an interactive terminal if you must use an iCloud
workspace.

Images and files sent by the phone are saved as Marmot media metadata. Supported
media is copied into `.agentnoise/attachments/` inside the active
repo/worktree, added to captioned agent prompts, and framed for the LLM Wiki
file-ingestion workflow when the prompt is `/wiki`. Supported chat media
mirrors mainline: JPEG/PNG/GIF/WebP images, MP4/WebM/MOV video,
MP3/OGG/M4A/WAV audio, and PDF. `/download <attachment-id|number>
[file-number]` is still available for manual retrieval. To send a workspace
file back to the phone, use `/upload <workspace-path> [caption]`; paths are
resolved inside the selected repo/worktree, not as arbitrary filesystem paths.
When a completed agent job references a supported media file it created inside
the selected workspace, agentnoise sends that file back through Dark Matter
media upload automatically. `/attachments` and `/attach <id>` list saved media
references and show local paths after retrieval.

Git worktrees are opt-in per chat. `/worktree new fix-ui` creates a git
worktree under the configured `runner.worktree_dir`, switches only that chat to
the new path, and keeps other Dark Matter sessions on their existing workspaces.
Removal requires the explicit `confirm` word.

## Fake Phone Testing

`agentnoise fake-phone live-roundtrip` starts an isolated real darkmatter
transport against a local mock relay, creates a separate fake phone identity,
has that phone create a group with the desktop, and sends test messages without
touching the real phone identity or the normal agentnoise keychain.

```sh
agentnoise fake-phone plan
agentnoise fake-phone live-roundtrip --expect running /status
agentnoise fake-phone live-roundtrip --expect /status /help
agentnoise fake-phone live-roundtrip \
  --start-worker \
  --min-replies 2 \
  --expect "codex queued" \
  --expect "agentnoise-darkmatter-live-ok" \
  --require-job-final \
  /codex "Reply with exactly: agentnoise-darkmatter-live-ok"
```

The scripted smoke is `./scripts/test-e2e-fake.sh`. It runs `/status`, `/help`,
and a `/codex` worker-final path through the real transport and worker, then
requires both inbound and successful outbound entries in `runtime-events.jsonl`. The older
`agentnoise fake-phone roundtrip` command is protocol-only and uses a synthetic
in-process responder; keep it for stream-envelope checks, not daemon release
confidence.

## Screenshots

Product screenshots and Open Graph assets are generated from a dedicated
`agentnoise.org/shots.html` staging page, not from the marketing site. See
[Screenshots](docs/screenshots.md) for the exact commands and privacy checklist
before using a real phone or terminal capture.

![agentnoise product screenshot](https://agentnoise.org/shots/desktop.png)

## Transport Notes

The darkmatter branch embeds Marmot v2 directly. `agentnoise transport run`
owns the message subscription and job queue; `agentnoise worker start` consumes
queued agent jobs. Historical White Noise `wn`/`wnd` subprocess integration was
removed from the live daemon path. Use `agentnoise fake-phone live-roundtrip`
before release when routing, service startup, or reply delivery changes.

### Local Agent Session Visibility

`/agent-sessions` shows recent local Codex and Claude session metadata from the
same user account, including sessions started outside agentnoise. It returns
session ids, update times, cwd when available, and the explicit resume command
to use next:

```text
/agent-sessions
/codex-resume <session> continue
/claude-resume <session> continue
```

This is intentionally conservative: it does not return transcript content,
inspect process environments, or silently attach unrelated local work to a
White Noise chat. The phone user must explicitly resume a listed session.

Automatic local session notifications are disabled by default. Enable them only
on machines where it is okay to send same-account Codex/Claude session metadata
to the primary paired White Noise chat:

```sh
agentnoise config local-sessions-watch on
brew services restart nvk/tap/agentnoise
```

The watcher baselines existing sessions at startup, then reports newly seen
local session ids, update times, and cwd metadata. It does not send transcript
content or attach automatically. To turn it off:

```sh
agentnoise config local-sessions-watch off
brew services restart nvk/tap/agentnoise
```

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

1. Desktop shows a QR for the agentnoise desktop `npub`.
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
- Keep automatic local-session notifications off unless that machine is meant
  to expose same-account Codex/Claude metadata to the paired chat.
- Use `--dev-burner-nsec` only for throwaway development identities.

## More

- [Marmot v2 (darkmatter) integration](docs/darkmatter.md)
- [Local bring-up](docs/local-bringup.md)
- [Remote SSH pairing](docs/remote-ssh.md)
- [Fake phone testing](docs/fake-phone-testing.md)
- [Testing](docs/testing.md)
- [Screenshots](docs/screenshots.md)
- [Supervisor services](docs/services.md)
- [Launchd service](docs/launchd.md)
- [Homebrew packaging](docs/homebrew.md)
- [Release notes](docs/release-notes.md)
- [Learn to Prompt agent stack](https://learntoprompt.org/guides/agent-stack.html)
- [Bondage local launcher](https://agentbondage.org/)

## License

MIT License. Copyright (c) 2026 nvk.

This software is provided as-is, without warranty of any kind.
