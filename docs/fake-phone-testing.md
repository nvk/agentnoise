# Fake Phone Testing

`agentnoise fake-phone` is a local Dark Matter/Marmot v2 smoke harness. It
starts an in-process mock relay, creates throwaway desktop and phone accounts,
creates a chat, sends test messages, and observes replies without touching your
real phone identity or the normal agentnoise keychain.

Inspect the isolated paths:

```sh
agentnoise fake-phone plan
```

Basic round trips:

```sh
agentnoise fake-phone roundtrip --pin 123456 /status
agentnoise fake-phone roundtrip /help
agentnoise fake-phone roundtrip /agents
```

Agent/job-style smoke test:

```sh
agentnoise fake-phone roundtrip \
  --timeout-seconds 180 \
  --min-replies 2 \
  --require-job-final \
  --expect agentnoise-e2e-ok \
  /codex "Reply with exactly: agentnoise-e2e-ok"
```

Useful flags:

```sh
agentnoise fake-phone roundtrip --timeout-seconds 120 /status
agentnoise fake-phone roundtrip --root /tmp/agentnoise-fake-phone /help
agentnoise fake-phone roundtrip --expect "agentnoise" /status
agentnoise fake-phone roundtrip --require-job-final --expect done /codex "Reply exactly: done"
```

If the command reports a timeout, check:

```sh
agentnoise status
agentnoise doctor
```

The older White Noise `wnd`/`wn` fake-phone daemon path was removed from the
v0.2.0 Dark Matter branch. See [darkmatter.md](darkmatter.md) for the current
transport architecture.
