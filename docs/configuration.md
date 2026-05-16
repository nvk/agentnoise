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

Restart after edits:

```sh
brew services restart nvk/tap/agentnoise
```

## Agent Launcher

Simple setup for raw Codex/Claude:

```toml
[runner]
launcher = "direct"

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
