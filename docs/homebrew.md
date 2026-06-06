# Homebrew Packaging

The intended Homebrew token is:

```text
agentnoise
```

Homebrew did not have a formula or cask named `agentnoise` when checked on 2026-05-14.

## Tap Formula

Use `packaging/homebrew/agentnoise.rb` as the starting formula once the source
is pushed and tagged. The initial formula uses pinned git sources: the
agentnoise release tag and the pinned upstream `whitenoise-rs` revision.

Local tap install shape:

```sh
brew install nvk/tap/agentnoise
agentnoise up --no-listen
brew services start nvk/tap/agentnoise
agentnoise worker start
# or, if tmux is installed:
agentnoise worker start --tmux
```

The formula builds and installs `agentnoise`, `wn`, and `wnd` under the same
Homebrew prefix. The service uses `agentnoise transport run`: it starts White
Noise, repairs login when needed, subscribes to paired chats, handles pairing,
and writes agent jobs into the local SQLite queue. `agentnoise worker start` is
the login-shell side that claims queued jobs and runs Codex, Claude, or Hermes;
add `--tmux` when tmux is installed and you want the worker detached. Homebrew
owns restart and boot for the transport through `brew services`.

Direct raw Codex/Claude is the default no-agentbondage path for new configs,
so the only local agent setup required is having those CLIs installed and
logged in. Use `agentnoise init --bondage` or `agentnoise up --bondage` only
when you want `bondage` profiles such as `codex-agentnoise` and
`claude-agentnoise`.

`agentnoise start` / `agentnoise up` are the all-in-one local console paths. If the Homebrew
transport is already running, an interactive `agentnoise up` attaches and
follows logs instead of starting a second listener. If the service is not
running, it takes the foreground engine lock and runs transport plus jobs in
one process until the terminal exits.

Current macOS Codex CLI builds can hang before producing output when launched
directly by launchd. The split transport/worker path avoids that by keeping the
Homebrew service responsible for White Noise only and running jobs from tmux or
your login shell.

```sh
agentnoise worker start
# or, if tmux is installed:
agentnoise worker start --tmux
```

For config examples:

```sh
agentnoise config path
agentnoise worker status
agentnoise config print-template
agentnoise doctor
```

See [Configuration](configuration.md) for the no-agentbondage raw Codex/Claude
mode, `bondage` profile mode, repo aliases, and profile variants such as
`/codex-fix`.

For development-only installs where keychain prompts are noise, run:

```sh
agentnoise up --dev-burner-nsec
```

That writes a plaintext throwaway identity under the agentnoise data dir. Later
Homebrew service starts reuse it because the flag persists in `config.toml`.
Do not use this for a real identity.

To check or change the machine label shown in White Noise:

```sh
agentnoise identity status
agentnoise identity rename agentnoise-mbp
```

To inspect or repair the White Noise account relays used for message delivery:

```sh
agentnoise whitenoise relays
agentnoise whitenoise ensure-relays
```

## Release Checklist

1. Tag a release.
2. Update the agentnoise formula tag if the version changed.
3. Update the pinned `whitenoise-rs` resource revision when upgrading the bundled CLI.
4. Run:

   ```sh
   brew audit --strict --online agentnoise
   brew test agentnoise
   ```
