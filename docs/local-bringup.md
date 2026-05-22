# Local Bring-Up

This is the normal desktop-to-phone path. Run it from a regular user terminal,
not from a restricted agent sandbox, because setup needs the macOS login
keychain and `~/Library/Application Support`.

## Build

```sh
cargo build --release
export AGENTNOISE="$PWD/target/release/agentnoise"
```

## Agent Launcher

The recommended path uses `bondage` profiles named `codex-agentnoise` and
`claude-agentnoise` so phone-triggered jobs run behind a local policy boundary.

If you only have raw Codex or Claude installed, initialize direct mode before
pairing:

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

Existing configs can switch without re-running setup:

```sh
"$AGENTNOISE" config launcher direct
"$AGENTNOISE" config launcher bondage
```

## Pair

If you know the phone Marmot v2 `npub`:

```sh
"$AGENTNOISE" up --phone npub... --name agentnoise-mbp
```

That creates or reuses the desktop keypair in the OS keychain, writes the
config, starts the embedded Darkmatter runtime, and publishes the desktop
profile/key package. Under Marmot v2 the phone creates the control group; the
desktop discovers and saves it while the listener is running.

If you do not know the phone `npub`:

```sh
"$AGENTNOISE" up
```

Scan the QR in the Marmot v2 phone client, create a group with the desktop
identity, then leave the process running. It will keep discovering Marmot v2
groups until the new group appears. If you used `--no-listen` or stopped the
process, run the same command again:

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
Marmot/Nostr profile so the phone can tell multiple agentnoise identities
apart.

To inspect or rename the current machine later:

```sh
agentnoise identity status
agentnoise identity rename agentnoise-mbp
```

Use `agentnoise identity rename <name> --no-publish` when you only want to edit
config and let the next `agentnoise up` publish the profile.

If messages are slow or missing, inspect the embedded runtime and event log, not
just the QR pairing hints:

```sh
agentnoise darkmatter probe
tail -f "$HOME/Library/Application Support/agentnoise/runtime-events.jsonl"
```

## Remote SSH Pairing

When pairing over SSH, pass only the phone `npub` and keep the PIN in the SSH
terminal:

```sh
brew services stop nvk/tap/agentnoise
agentnoise up --ssh --phone npub1... --name agentnoise-linuxbox
```

`--ssh` disables the desktop GUI pairing alert and prints the rotating PIN in
the terminal session. The remote box generates its own desktop keypair locally;
no `nsec` crosses SSH for normal setup.

## Run

```sh
"$AGENTNOISE" up
```

If a service is already running, this attaches as the local UI and follows logs.
If no service owns the engine, it runs the listener in the foreground. This
makes the same command useful for both desktop bring-up and troubleshooting a
stopped service.

From the phone, send:

```text
/status
/repos
/use sandbox
/pwd
/codex say hello
/wiki research agent chat ux
```

Each Marmot v2 group with the agentnoise desktop identity is an independent
agentnoise session. You can create another group from the same phone identity,
send `/use` or `/cd` there, and it will not disturb the workspace state in the
first group. `/new <name>` can also create a parallel group from the desktop
side when the phone sender has a published key package.

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
block the macOS keychain and default Application Support paths that the embedded
Darkmatter runtime uses. Use a normal terminal, or a profile that allows the
agentnoise data directory and OS keychain, for the actual setup run.
