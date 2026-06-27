# White Noise Setup

agentnoise uses the upstream `wn` client and `wnd` daemon from `marmot-protocol/whitenoise-rs`. A packaged agentnoise install should ship `wn` and `wnd` beside the `agentnoise` binary. If not, agentnoise can install them into its managed data directory.

## Managed Install

```sh
agentnoise whitenoise install
agentnoise whitenoise status
```

This installs the upstream `whitenoise-cli` package under:

```text
~/Library/Application Support/agentnoise/whitenoise-cli/bin/
```

agentnoise discovers `wn` in this order:

1. explicit `whitenoise.wn_bin` path in `config.toml`, except known old agentnoise-managed paths are migrated back to the packaged default
2. `wn` beside the running `agentnoise` executable
3. agentnoise-managed install under Application Support
4. repo-local development build under `.local-whitenoise/bin`
5. `wn` on `PATH`

On Homebrew upgrades, `agentnoise transport run` resets legacy managed paths such as `~/bin/agentnoise-whitenoise/wn` to the packaged `wn` beside `agentnoise`, kickstarts the old macOS `local.agentnoise.wnd` LaunchAgent when present so its existing Keychain authorization is preserved, and restarts the White Noise daemon so stale `wnd` processes are refreshed. If sends fail later with closed-socket, broken-pipe, or too-many-open-files errors, agentnoise restarts `wnd` before retrying delivery.

## Manual Build

```sh
git clone https://github.com/marmot-protocol/whitenoise-rs.git
cd whitenoise-rs
cargo install --path crates/whitenoise-cli
```

The upstream README also supports:

```sh
just install-cli
```

For local development in this checkout, the CLI was built successfully into:

```text
.local-whitenoise/bin/wn
.local-whitenoise/bin/wnd
```

Use that path in `config.toml` if you want to force the local dev build:

```toml
[whitenoise]
wn_bin = ".local-whitenoise/bin/wn"
```

## Create the agentnoise Keypair

agentnoise uses a dedicated White Noise/Nostr identity for the desktop helper.
The normal path is:

```sh
agentnoise up
```

`up` creates or reuses the default `desktop` identity, stores the `nsec` in
the OS keychain by default, writes the desktop `npub` into `config.toml`, starts White
Noise, logs in, publishes the desktop profile/key package, enables startup login
repair, prints a phone pairing QR when pairing is still needed, discovers
visible control chats when possible, waits for the first chat when needed, and
then listens. It never writes the `nsec` to `config.toml`.

Give each computer a unique published profile name:

```sh
agentnoise up --name agentnoise-mbp
agentnoise up --phone npub1... --name agentnoise-linuxbox
agentnoise identity status
agentnoise identity rename agentnoise-freebsd
```

The display name is saved in `config.toml` and published through the White
Noise/Nostr profile, so the phone can tell multiple agentnoise desktops apart.
`identity status` uses the public account already saved in config when
available, so checking the label does not require passing the phone `npub` or
reading the desktop `nsec`. `identity rename <name>` changes the configured
label and publishes it; add `--no-publish` to save only.

The QR encodes the desktop `npub` because that is the most reliable scan target
in the current phone app. The terminal output also prints a Nostr `nprofile`
with relay hints for manual copy/paste. Neither value contains the desktop
`nsec`.

Pairing relay hints are only discovery hints. Message delivery uses the White
Noise account relay list, which agentnoise can reconcile after login.

Use a dedicated desktop identity. Do not reuse the phone identity secret.

## OS Keychain Bootstrap

For unattended or remote boxes, agentnoise can store the dedicated bot `nsec` in the OS keychain and use it to repair the White Noise login at process startup.

The simple command above is equivalent to the advanced identity flow:

```sh
agentnoise identity create
agentnoise pair
```

For HA planning or staged rotation, create a small set explicitly:

```sh
agentnoise identity create --count 3
```

- `desktop` at `agentnoise / whitenoise-nsec`
- `desktop-2` at `agentnoise / whitenoise-nsec/desktop-2`
- `desktop-3` at `agentnoise / whitenoise-nsec/desktop-3`

If you already created the identity with `wn`, export or copy the bot `nsec` once, then store it with agentnoise instead:

```sh
wn export-nsec <bot-npub>
```

```sh
agentnoise keychain store-nsec
agentnoise keychain status
```

`store-nsec` prompts on a terminal and also accepts a single piped line:

```sh
printf '%s\n' 'nsec1...' | agentnoise keychain store-nsec
```

Render a pairing QR for the phone:

```sh
agentnoise pair
```

The QR encodes the desktop bot `npub`; the adjacent text prints the standard
Nostr `nprofile`, which contains the same `npub` plus relay hints. If
`agentnoise up` is already running and waiting for first pairing, this command
also prints the current live pairing PIN. Override relays at render time when
needed:

```sh
agentnoise pair --relay wss://relay.example
```

`up` enables startup repair automatically. The relevant config is:

```toml
[whitenoise]
use_keychain_nsec = true
pairing_relays = [
    "wss://index.hzrd149.com",
    "wss://indexer.coracle.social",
    "wss://relay.primal.net",
    "wss://relay.damus.io",
    "wss://relay.ditto.pub",
    "wss://nos.lol",
]
message_relays = [
    "wss://index.hzrd149.com",
    "wss://relay.primal.net",
    "wss://relay.ditto.pub",
    "wss://nos.lol",
    "wss://nostr.mom",
]
keychain_service = "agentnoise"
keychain_item = "whitenoise-nsec"
login_relay = "wss://relay.example" # optional
```

Run `agentnoise identity status` to see configured pairing and message relays.
Run `agentnoise whitenoise relays` to see the current White Noise account relay
state, and `agentnoise whitenoise ensure-relays` to add the configured
`message_relays` as `nip65`, `inbox`, and `key_package` relays.

## Delivery Diagnostics

agentnoise can prove that it received a phone message and handed a reply to
White Noise before the phone app renders that reply. This is normal for
troubleshooting: local send success and phone receipt are two different facts.

Use these checks when a phone appears silent:

```sh
agentnoise status
tail -f "$HOME/Library/Application Support/agentnoise/runtime-events.jsonl"
wn messages list <group-id> --json
wn debug health --json
wn debug relay-control-state
```

If the journal shows `reply-sent` and `wn messages list` contains the reply,
agentnoise already handed the message to White Noise. Delayed appearance on the
phone points at relay/mobile sync. If the journal has inbound phone messages but
no `reply-queued`, debug agentnoise command handling. If there is no inbound
phone message, debug group subscription and relay reachability.

To test the path manually:

```sh
agentnoise whitenoise login-from-keychain
agentnoise doctor
```

agentnoise stores the desktop public key in config after setup. Normal
`agentnoise up` and service restarts use that cached `npub` for pairing QR and
profile setup, so they do not read the desktop `nsec` just to start. agentnoise
checks `wn whoami --json` at startup. If the configured White Noise account is
already logged in, matching either `npub` or the hex pubkey returned by White
Noise, it does not read the keychain. If the account is missing and
`use_keychain_nsec = true`, it reads the `nsec` once, feeds it to `wn login`,
then zeroizes the in-process copy. The listener still needs a logged-in White
Noise signing account to send replies; the long-running message loop does not
poll the keychain.

## Development Burner Identity

For disposable development runs, use a plaintext burner identity instead of the
OS keychain:

```sh
agentnoise up --dev-burner-nsec
```

This creates `dev-burner.nsec` under the agentnoise data dir, writes:

```toml
[whitenoise]
dev_burner_nsec = true
dev_burner_nsec_file = "/path/to/dev-burner.nsec"
use_keychain_nsec = false
```

Later `agentnoise up` and Homebrew service starts reuse that file, so keychain
prompts stay out of the development loop. Treat this as public-testnet-grade
only: the file is plaintext and should never hold a real identity.

This disables agentnoise's own OS keychain dependency for the desktop helper
identity. White Noise still controls daemon account persistence after `wn
login`; if that upstream store is unavailable, agentnoise reports the White
Noise login failure instead of falling back to the agentnoise keychain.

Remove the stored bootstrap secret:

```sh
agentnoise keychain delete-nsec
agentnoise identity delete --name desktop-2
```

High-availability note: the OS keychain must be available to the same service
account that runs agentnoise when login repair is needed. A macOS per-user
LaunchAgent can normally read the login keychain after the user session is
unlocked, but macOS may require one Terminal authorization first:
`agentnoise keychain status`. Headless Linux Secret Service setups often depend
on a DBus/user session and should be tested from the exact supervisor context
before relying on automatic restart repair.

## Create Control Chats

If you know the phone identity `npub`, let agentnoise create the group:

```sh
agentnoise up --phone npub...
```

Otherwise scan the QR from `agentnoise up`, create the group from the phone,
and leave the process running. agentnoise asks `wn groups list --json`, saves
visible group ids, and listens to them. It does not trust a peer just because a
group is visible on the relay; command auth is still the sender allowlist.

```sh
agentnoise up
```

The first paired chat is the inbox. Starting a new `/codex`, `/claude`,
`/hermes`, or `/wiki` job from that inbox creates a fresh White Noise work
chat with the same paired phone identity. The chat name is
`hostname - 2-4 word prompt summary`, agentnoise sends an open link back to the
inbox, and progress plus final output continue in the new chat. Plain text
sent inside that work chat continues with the same agent/profile/wiki mode and
workspace. Use slash commands in a work chat when changing workspace,
inspecting jobs, or intentionally switching agents.

To create a manual parallel session from the same phone, send `/new <name>`
from an existing agentnoise chat. agentnoise creates another White Noise chat
with the same paired phone identity, clones the current workspace, saves the
new MLS group id, subscribes immediately, and posts a ready message in the new
chat.

You can also create multiple White Noise chats with the agentnoise desktop
identity manually. White Noise gives each chat a different MLS group id, and
agentnoise keeps `/use`, `/cd`, and prompt context separate per group id. Use
`/rename <name>` to name a manually-created chat, `/list` to list known
sessions with short chat refs and `whitenoise://chat/...` open links, and
`/jump <number|name|id>` to ping a saved session from that list. `/resume`,
`/sessions`, and `/here <name>` remain accepted as compatibility aliases.

## Sender Allowlist

`allowed_senders` is filled by a first-run PIN handshake. When the allowlist is
empty and `require_pairing_pin = true`, `agentnoise up` prints the QR and a
6-digit PIN. On macOS it also opens a pairing window with the desktop identity
QR, desktop `npub`, current PIN, and live countdown. The phone must send that
PIN as the first message, either as a bare code or `/pair 123456`. agentnoise
stores that sender and ignores every other message until the PIN succeeds.

The relevant config is:

```toml
[whitenoise]
require_pairing_pin = true
pairing_pin_seconds = 30
allowed_senders = []
```

If you need to set `allowed_senders` manually, use the sender value emitted by
`wn messages subscribe --json`. Current upstream formatted messages include an
`author` hex pubkey inside the `message` object, so the manual setup is:

1. Temporarily set `require_pairing_pin = false`.
1. Run `agentnoise up` in the foreground with `allowed_senders = []`.
1. Send `/status` from the phone.
1. Copy the sender shown in the local stream/logs or inspect `wn messages subscribe --json`.
1. Add it to `allowed_senders`.
1. Restore `require_pairing_pin = true`.
1. Restart agentnoise.

Foreground pairing is easiest to inspect. A macOS LaunchAgent can still enter
pairing mode and show the same desktop window when the user GUI session is
available; on headless or non-macOS services, use the supervisor log for the
rotating PIN.

## Initial Messages

agentnoise defaults to:

```toml
subscribe_limit = 0
ignore_initial_messages = true
```

This avoids replaying old chat commands on startup.

During first pairing only, agentnoise raises the subscription limit to a small
initial window so a PIN sent immediately after creating the White Noise chat can
still be accepted. Non-PIN initial messages are still ignored.
