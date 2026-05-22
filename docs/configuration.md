# Configuration

> <!-- stale-for-v2 --> **Note:** parts of this guide pre-date the v0.2.0 Marmot v2 migration. CLI flags and config sections (e.g. `[whitenoise]`, `agentnoise whitenoise *`, `wn` / `wnd`) referenced here may no longer exist. See [docs/darkmatter.md](darkmatter.md) for the current architecture, [docs/release-notes.md](release-notes.md) for what changed.

Find the live config:

```sh
agentnoise config path
agentnoise config print-template
agentnoise doctor
agentnoise agents
```

On macOS/Homebrew the normal config is under:

```text
~/Library/Application Support/agentnoise/config.toml
```

### Named instances

Named instances use isolated config roots. This is the recommended setup when
multiple people (or multiple identities) share one machine:

```sh
agentnoise --instance alice config path
agentnoise --instance bob config path
```

```text
~/Library/Application Support/agentnoise/instances/alice/config.toml
~/Library/Application Support/agentnoise/instances/bob/config.toml
```

The generated Alice/Bob configs also get separate data dirs, log dirs, keychain
services (`agentnoise-alice` / `agentnoise-bob` — see [darkmatter.md](darkmatter.md)),
worktree dirs, service names, Marmot v2 profile names, and default `sandbox`
repo paths. That is stronger than pairing two phone npubs to one global config,
because the global config shares repos and launcher policy. `--instance` cannot
be combined with `--config`.

This is also how to run the packaged release and a checkout build at the same
time. Let the Homebrew/default service keep the default instance, then run the
checkout as a named development instance:

```sh
brew services start nvk/tap/agentnoise
just up              # uses --instance dev and debug logs
just up-quiet dev    # same dev instance without debug logs
```

The phone sees these as separate Agent Noise identities/groups. Pair the dev
instance once, then use that dev chat for checkout testing while the release
service keeps running normally.

Restart after edits:

```sh
brew services restart nvk/tap/agentnoise
```

For a named instance installed through `agentnoise service install`, restart the
native service name instead. On Linux that is `agentnoise-alice.service`. On
macOS, unload/load the generated LaunchAgent label such as
`com.agentnoise.agentnoise.alice`, or rerun:

```sh
agentnoise --instance alice service install --target launchd --force --load
```

## Marmot Streams

Live agent output previews use the broker configured in `[darkmatter]`:

```toml
[darkmatter]
agent_text_stream_broker = "https://quic-broker.ipf.dev:4450"
```

agentnoise normalizes that public service URL to the `quic://` candidate format
used in Marmot agent-text-stream start payloads.

## Agent Launcher

Simple setup for raw Codex/Claude:

```toml
[runner]
launcher = "direct"
progress_interval_seconds = 15
silence_ping_seconds = 60
startup_silence_timeout_seconds = 90
startup_retry_attempts = 1
job_timeout_seconds = 1800

[agents.codex]
enabled = true
profile = "codex"
bin = "codex"

[agents.claude]
enabled = true
profile = "claude"
bin = "claude"
model = "sonnet"
permission_mode = "auto"
```

Set it from the CLI:

```sh
agentnoise up --direct-agents
# or
agentnoise config launcher direct
```

Policy-boundary setup with `bondage`:

```toml
[runner]
launcher = "bondage"
bondage_bin = "bondage"
bondage_conf = "~/.config/bondage/bondage.conf"
progress_interval_seconds = 15
silence_ping_seconds = 60
startup_silence_timeout_seconds = 90
startup_retry_attempts = 1
job_timeout_seconds = 1800

[agents.codex]
enabled = true
profile = "codex-agentnoise"
bin = "codex"

[agents.claude]
enabled = true
profile = "claude-agentnoise"
bin = "claude"
model = "sonnet"
permission_mode = "auto"
```

Your `bondage.conf` must contain matching profile sections:

```toml
[profile "codex-agentnoise"]
# target, sandbox, env, and policy settings live here

[profile "claude-agentnoise"]
# target, sandbox, env, and policy settings live here
```

If a profile is missing, either add it to `bondage.conf` or switch to direct
mode.

`agents.claude.model` is optional. Set it when your local Claude default points
at a model alias that is not available for non-interactive `--print` runs, such
as a 1M-context alias that requires usage credits.

`job_timeout_seconds` keeps phone-triggered jobs from staying `running`
forever if a launcher or agent process wedges before returning output. Set it
to `0` only if you intentionally want no timeout.

`silence_ping_seconds` controls the "still running" chat ping for a job that
has produced no new output. Set it to `0` to disable those pings. The default
keeps the phone chat alive when Codex, Claude, Hermes, or a launcher is still
running but quiet.

`startup_silence_timeout_seconds` handles a different failure mode: a launcher
or agent process starts but emits no stdout/stderr at all. agentnoise
terminates that launch after the timeout and retries
`startup_retry_attempts` times before returning a clear failure to the chat.

For direct Codex launches, agentnoise starts the child process from its stable
data directory and passes the selected workspace with `codex -C`. This keeps
launchd services away from fragile GUI-backed cwd paths such as iCloud Drive
while preserving the repo/cwd chosen in chat.

Codex itself can still hang under launchd when the selected `-C` workspace is
inside iCloud Drive/CloudDocs. `agentnoise doctor` warns about those repo paths.
For Homebrew service use, keep configured repos in normal local directories
such as `~/src` or `~/src-repo`. If you must work inside iCloud Drive, run
`agentnoise up` from an interactive terminal instead of a background service.

## Agent Profile Variants

Variants expose extra chat commands without hardcoding local profile names in
agentnoise. This config:

```toml
[[agents.codex.profiles]]
name = "fix"
profile = "codex-fix"

[[agents.codex.profiles]]
name = "unsafe"
profile = "codex-unsafe"
```

Adds these White Noise commands:

```text
/codex-fix <prompt>
/codex-fix-resume <session> <prompt>
/codex-unsafe <prompt>
/codex-unsafe-resume <session> <prompt>
```

Names must use lowercase letters, digits, and dashes. `resume` is reserved.
Profiles containing words such as `unsafe` or `rawdog` require chat approval
before the job runs.

## Repos

Repos are aliases. Phone commands select aliases, not arbitrary filesystem
paths:

```toml
[[repos]]
alias = "sandbox"
path = "~/src/sandbox"

[[repos]]
alias = "site"
path = "~/src/agentnoise.org"
```

Use from White Noise:

```text
/repos
/use site
/cd src
/codex fix the failing test
```

## Local Session Visibility

Manual session lookup is available from chat with `/agent-sessions`. It lists
recent same-account Codex/Claude metadata and explicit resume commands without
returning transcript content.

Background notifications are a separate opt-in. Leave this off on machines
where local session names or cwd paths are sensitive:

```toml
[local_sessions]
watch = false
watch_interval_seconds = 60
notify_limit = 5
```

Enable it from the CLI on machines where that metadata exposure is acceptable:

```sh
agentnoise config local-sessions-watch on
brew services restart nvk/tap/agentnoise
```

When enabled, agentnoise baselines existing sessions at listener startup and
then sends newly seen local session ids, update times, cwd when available, and
resume commands to the primary paired White Noise chat. It does not send
transcript content, inspect process environments, or attach automatically.

## White Noise Identity

The desktop helper uses its own White Noise/Nostr identity. The public `npub`
and profile labels live in config. The private `nsec` should stay in the OS
keychain for real use.

Useful commands:

```sh
agentnoise identity status
agentnoise identity rename agentnoise-mbp
agentnoise keychain status
```

For development-only testing:

```sh
agentnoise up --dev-burner-nsec
```

That writes a throwaway plaintext `nsec` under the agentnoise data directory.
Do not use it for a real identity.
