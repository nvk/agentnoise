# Fake Phone Testing

> <!-- stale-for-v2 --> **Note:** parts of this guide pre-date the v0.2.0 Marmot v2 migration. CLI flags and config sections (e.g. `[whitenoise]`, `agentnoise whitenoise *`, `wn` / `wnd`) referenced here may no longer exist. See [docs/darkmatter.md](darkmatter.md) for the current architecture, [docs/release-notes.md](release-notes.md) for what changed.

`agentnoise fake-phone` is for local bring-up when you want to test the desktop
helper without using the real phone identity.

It uses a separate `wnd` data directory, a separate socket, and a throwaway
White Noise identity. The fake phone `nsec` lives at `fake-phone.nsec` under
the fake-phone root. It does not read or write the normal agentnoise `nsec`,
and it does not use the agentnoise OS keychain.

The harness generates and reuses that burner `nsec` directly instead of asking
White Noise to create/export a key. White Noise may still use its own platform
secret store when the fake daemon logs that account in.

Inspect the paths:

```sh
agentnoise fake-phone plan
```

First-pairing test:

```sh
agentnoise up
agentnoise fake-phone roundtrip --pin 123456 /status
```

After pairing:

```sh
agentnoise fake-phone roundtrip /help
agentnoise fake-phone roundtrip /agents
```

Full job-path test:

```sh
agentnoise fake-phone roundtrip \
  --timeout-seconds 180 \
  --min-replies 2 \
  --require-job-final \
  --expect agentnoise-e2e-ok \
  /codex "Reply with exactly: agentnoise-e2e-ok"
```

On macOS over SSH, starting an isolated `wnd` can fail if the session cannot
access the GUI user's Keychain for White Noise's database key. For that
specific test setup, reuse an already-running GUI-authorized daemon:

```sh
agentnoise fake-phone roundtrip --shared-daemon /status
```

`--shared-daemon` still uses the fake phone burner `nsec`, but it logs that
burner account into the configured/default White Noise daemon instead of
starting a separate daemon.

The harness creates a White Noise chat with the configured desktop agentnoise
`npub`, then resends the requested message until the first useful reply. That
makes it usable with the normal listener discovery loop, which may need one
cycle before subscribing to the new fake-phone chat. After the first reply, it
does not resend the command, so long-running agent jobs are not duplicated.

If the first command reaches agentnoise after the displayed PIN has rotated,
the harness reads the live runtime PIN, sends it, and retries the original
command. It also stores the fake-phone chat id under the fake-phone root, scoped
to the desktop `npub`, so a follow-up `/codex` test continues in the same chat
instead of creating a new White Noise group.

Useful flags:

```sh
agentnoise fake-phone roundtrip --timeout-seconds 120 /status
agentnoise fake-phone roundtrip --root /tmp/agentnoise-fake-phone /help
agentnoise fake-phone roundtrip --expect "Status: OK" /status
agentnoise fake-phone roundtrip --require-job-final --expect done /codex "Reply exactly: done"
```

If the command reports a timeout, check:

```sh
agentnoise status
agentnoise doctor
tail -f "$(brew --prefix)/var/log/agentnoise.log"
```
