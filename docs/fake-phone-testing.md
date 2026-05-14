# Fake Phone Testing

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

The harness creates a White Noise chat with the configured desktop agentnoise
`npub`, then resends the requested message for the timeout window. That makes it
usable with the normal listener discovery loop, which may need one cycle before
subscribing to the new fake-phone chat.

Useful flags:

```sh
agentnoise fake-phone roundtrip --timeout-seconds 120 /status
agentnoise fake-phone roundtrip --root /tmp/agentnoise-fake-phone /help
```

If the command reports no replies before timeout, check:

```sh
agentnoise status
agentnoise doctor
tail -f "$(brew --prefix)/var/log/agentnoise.log"
```
