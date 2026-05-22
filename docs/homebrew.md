# Homebrew Packaging

The intended Homebrew token is:

```text
agentnoise
```

Homebrew did not have a formula or cask named `agentnoise` when checked on 2026-05-14.

## Tap Formula

Use `packaging/homebrew/agentnoise.rb` as the starting formula once the source
is pushed and tagged. The formula builds agentnoise with the pinned Darkmatter
Cargo dependencies from `Cargo.lock`.

Local tap install shape:

```sh
brew install nvk/tap/agentnoise
agentnoise init
brew services start nvk/tap/agentnoise
```

The formula installs `agentnoise`. The Marmot v2 protocol stack is embedded in
the binary; there are no `wn` or `wnd` subprocesses in the daemon path. The
service uses `agentnoise up`, so setup repair, embedded runtime startup, group
discovery, first-pairing PIN auth, and keychain access stay in one code path. On
a fresh install the service starts before a control group exists, shows the
QR/PIN pairing flow, and keeps discovering Marmot v2 groups until the
phone-created group appears. Homebrew owns restart and boot through
`brew services`.

`agentnoise up` is also the local console. If the Homebrew service is already
running, an interactive `agentnoise up` attaches to the existing engine and
follows logs instead of starting a second listener. If the service is not
running, it takes the foreground engine lock and behaves like the service until
the terminal exits.

Current macOS Codex CLI builds can hang before producing output when launched
directly by launchd. Use Homebrew services for setup, pairing, status, and boot.
For phone-launched `/codex` jobs on macOS, stop the service and run the listener
from a login shell or tmux:

```sh
brew services stop nvk/tap/agentnoise
agentnoise up
```

```sh
tmux new -s agentnoise 'agentnoise up'
```

For config examples:

```sh
agentnoise config path
agentnoise config print-template
agentnoise doctor
```

See [Configuration](configuration.md) for raw Codex/Claude mode, `bondage`
profile mode, repo aliases, and profile variants such as `/codex-fix`.

To check or change the machine label shown in the phone client:

```sh
agentnoise identity status
agentnoise identity rename agentnoise-mbp
```

To inspect the embedded runtime and relay configuration:

```sh
agentnoise darkmatter probe
```

## Release Checklist

1. Tag a release.
2. Update the agentnoise formula tag if the version changed.
3. Update `Cargo.lock` when upgrading the pinned Darkmatter revision.
4. Run:

   ```sh
   brew audit --strict --online agentnoise
   brew test agentnoise
   ```
