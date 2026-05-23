# Configuration

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

Named instances use isolated config roots. This is the recommended setup when
multiple people share one machine:

```sh
agentnoise --instance alice config path
agentnoise --instance bob config path
```

```text
~/Library/Application Support/agentnoise/instances/alice/config.toml
~/Library/Application Support/agentnoise/instances/bob/config.toml
```

The generated Alice/Bob configs also get separate data dirs, log dirs, keychain
services, worktree dirs, service names, White Noise profile names, and default
`sandbox` repo paths. That is stronger than pairing two phone npubs to one
global config, because the global config shares repos and launcher policy.

Restart after edits:

```sh
brew services restart nvk/tap/agentnoise
agentnoise worker start
```

For a named instance installed through `agentnoise service install`, restart
the native service name instead. On Linux that is `agentnoise-alice.service`.
On macOS, unload/load the generated LaunchAgent label such as
`com.agentnoise.agentnoise.alice`, or rerun:

```sh
agentnoise --instance alice service install --target launchd --force --load
```

## Agent Launcher

Simple setup for raw Codex/Claude, with no `agentbondage` required:

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
permission_mode = "auto"
```

Set it from the CLI:

```sh
agentnoise init --direct-agents
# or during first setup
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
such as `~/src` or `~/src-repo`. The Homebrew service now owns only the White
Noise transport; run `agentnoise worker start` from your login shell so the
local agent process inherits the expected user context. If tmux is installed,
add `--tmux` to detach it. If you must work inside iCloud Drive, use the
foreground `agentnoise up` path while debugging.

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

For multi-tenant hosts, keep each person's default `sandbox` repo under that
person's instance root, or configure only the repos that person should reach.
Example:

```toml
instance = "alice"

[[repos]]
alias = "sandbox"
path = "~/Library/Application Support/agentnoise/instances/alice/sandbox"
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
