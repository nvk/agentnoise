# Local Bring-Up

> <!-- stale-for-v2 --> **Note:** parts of this guide pre-date the v0.2.0 Marmot v2 migration. CLI flags and config sections (e.g. `[whitenoise]`, `agentnoise whitenoise *`, `wn` / `wnd`) referenced here may no longer exist. See [docs/darkmatter.md](darkmatter.md) for the current architecture, [docs/release-notes.md](release-notes.md) for what changed.

This is the normal desktop-to-phone path. Run it from a regular user terminal,
not from a restricted agent sandbox, because setup needs the macOS login
keychain and `~/Library/Application Support`.

## Build

```sh
cargo build --release
export AGENTNOISE="$PWD/target/release/agentnoise"
```

## Agent Launcher

The simplest path does not require `agentbondage`. If you have raw Codex or
Claude installed and logged in, initialize direct mode before pairing:

```sh
"$AGENTNOISE" init --direct-agents
```

Or set it during the first setup run:

```sh
"$AGENTNOISE" up --direct-agents --no-listen
```

This persists `runner.launcher = "direct"` in config. Direct mode skips
`bondage` and runs the raw agent CLIs with structured argv. Confirm the active
mode with:

```sh
"$AGENTNOISE" doctor
"$AGENTNOISE" agents
```

For the hardened local-agent-stack setup, use `bondage` profiles named
`codex-agentnoise` and `claude-agentnoise` so phone-triggered jobs run behind a
local policy boundary.

Existing configs can switch without re-running setup:

```sh
"$AGENTNOISE" config launcher direct
"$AGENTNOISE" config launcher bondage
```

## Pair

If you know the phone White Noise `npub`:

```sh
"$AGENTNOISE" up --phone npub... --name agentnoise-mbp
```

That creates or reuses the desktop keypair in the OS keychain, writes the config,
starts the White Noise daemon if needed, logs in from the configured bootstrap
nsec, publishes the desktop profile/key package, creates the `agentnoise`
control chat, and saves the group id when `wn` returns it.

If you do not know the phone `npub`:

```sh
"$AGENTNOISE" up
```

Scan the QR in White Noise, create a chat/group with the desktop identity, then
leave the process running. It will keep discovering White Noise chats until the
new chat appears. If you used `--no-listen` or stopped the process, run the same
command again:

```sh
"$AGENTNOISE" up
```

If `allowed_senders` is still empty, `up` prints a QR and a 6-digit PIN. On
macOS it also opens a pairing window with the QR, desktop `npub`, PIN, and live
countdown. Send the PIN from the phone as the first message.
agentnoise ignores every other message until the PIN succeeds, then saves that
sender to `allowed_senders`.

Use a distinct `--name` for every computer, for example `agentnoise-mbp` or
`agentnoise-linuxbox`. The name is saved in config and published as the desktop
White Noise/Nostr profile so the phone can tell multiple agentnoise identities
apart.

To inspect or rename the current machine later:

```sh
agentnoise identity status
agentnoise identity rename agentnoise-mbp
```

Use `agentnoise identity rename <name> --no-publish` when you only want to edit
config and let the next `agentnoise up` publish the profile.

If messages are slow or missing, inspect the actual White Noise account relay
state, not just the QR pairing hints:

```sh
agentnoise whitenoise relays
agentnoise whitenoise ensure-relays
```

## Remote SSH Pairing

When pairing over SSH, pass only the phone `npub` and keep the PIN in the SSH
terminal:

```sh
brew services stop nvk/tap/agentnoise
agentnoise up --ssh --phone npub1... --name agentnoise-linuxbox
brew services start nvk/tap/agentnoise
agentnoise worker start
```

`--ssh` disables the desktop GUI pairing alert and prints the rotating PIN in
the terminal session. The remote box generates its own desktop keypair locally;
no `nsec` crosses SSH for normal setup. Stop the foreground `up` process after
pairing succeeds, then start the service and worker.

## Run

```sh
"$AGENTNOISE" up
```

If a service is already running, this attaches as the local UI and follows logs.
If no service owns the transport, it runs the listener and jobs in the
foreground. For the reboot-safe path, keep the service running and keep a login
shell worker alive with `agentnoise worker start` or, with tmux installed,
`agentnoise worker start --tmux`.

From the phone, send:

```text
/status
/repos
/use sandbox
/pwd
/codex say hello
/wiki research agent chat ux
```

Each White Noise chat with the agentnoise desktop identity is an independent
agentnoise session. You can create another chat from the same phone identity,
send `/use` or `/cd` there, and it will not disturb the workspace state in the
first chat.

When the foreground test works:

```sh
"$AGENTNOISE" service install --target launchd --force --load
```

## Optional Hermes Backend

Hermes is off by default. To test it, install the Hermes CLI, create a dedicated
`bondage` profile, and add this to the agentnoise config:

```toml
[agents.hermes]
enabled = true
profile = "hermes-agentnoise"
bin = "hermes"
```

Restart agentnoise and send:

```text
/hermes say hello
```

agentnoise runs Hermes as `hermes chat --quiet --source agentnoise --toolsets
skills -q ...` through `bondage`. Put `HERMES_HOME`, model endpoint settings,
and filesystem restrictions in the `bondage` profile.

Codex and Claude should also use dedicated `bondage` profiles for phone-driven
runs: `codex-agentnoise` and `claude-agentnoise`. Older configs that name the
generic `codex` or `claude` profiles are mapped to the matching
`*-agentnoise` profile unless `runner.allow_generic_agent_profiles = true` is
set.

Machines can expose additional configured profiles as explicit chat commands.
For example, this keeps `/codex` on the normal profile while adding
`/codex-fix` and `/codex-unsafe`:

```toml
[[agents.codex.profiles]]
name = "fix"
profile = "codex-fix"

[[agents.codex.profiles]]
name = "unsafe"
profile = "codex-unsafe"
```

Variant names become command suffixes. A variant named `fix` exposes
`/codex-fix <prompt>` and `/codex-fix-resume <session> <prompt>`. Profiles
containing risky words such as `unsafe` still require chat approval before the
job runs.

When `runner.launcher = "direct"`, the configured profile names are unused and
Hermes runs directly as `hermes chat ...`.

## Permission Note

`codex-fix` and `codex-unsafe` are enough for editing and testing agentnoise.
They are not enough for local bring-up, because those `nono` profiles still
block the macOS keychain and default Application Support paths that White Noise
uses. Use a normal terminal, or a no-`nono` profile such as `codex-rawdog`, for
the actual setup run.

For development-only burner identities where keychain prompts get in the way:

```sh
"$AGENTNOISE" up --dev-burner-nsec
```

This writes a plaintext throwaway `nsec` under the agentnoise data dir and sets
the config so later `up` or service starts reuse that file. Do not use it for a
real identity.
