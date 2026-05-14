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
brew services start nvk/tap/agentnoise
tail -f "$(brew --prefix)/var/log/agentnoise.log"
```

The formula builds and installs `agentnoise`, `wn`, and `wnd` under the same
Homebrew prefix. The service uses `agentnoise up`, so setup repair, daemon
startup, group discovery, first-pairing PIN auth, and keychain login repair stay
in one code path. On a fresh install the service starts before a control chat
exists, shows the QR/PIN pairing flow, and keeps discovering White Noise chats
until the phone-created chat appears. Homebrew owns restart and boot through
`brew services`.

## Release Checklist

1. Tag a release.
2. Update the agentnoise formula tag if the version changed.
3. Update the pinned `whitenoise-rs` resource revision when upgrading the bundled CLI.
4. Run:

   ```sh
   brew audit --strict --online agentnoise
   brew test agentnoise
   ```
