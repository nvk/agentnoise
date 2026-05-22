# Fake Phone Testing

`agentnoise fake-phone` is a synthetic local protocol smoke. It starts an
in-process mock relay, creates one Darkmatter desktop account and one fake phone
account, creates a Marmot v2 group, sends a message, collects replies, and
requires agent-text-stream finalization when requested.

It does not start the live desktop listener, use a real phone identity, or touch
the normal agentnoise keychain. Use it to catch embedded Darkmatter routing
regressions quickly; use a real phone `/status` smoke for live listener pairing.

Inspect the isolated paths:

```sh
agentnoise fake-phone plan
```

Basic roundtrips:

```sh
agentnoise fake-phone roundtrip --expect "received: /status" /status
agentnoise fake-phone roundtrip --expect "commands: /help /status /codex" /help
```

Synthetic job-stream roundtrip:

```sh
agentnoise fake-phone roundtrip \
  --timeout-seconds 180 \
  --require-job-final \
  --expect "codex queued: Reply with exactly: agentnoise-e2e-ok" \
  /codex "Reply with exactly: agentnoise-e2e-ok"
```

The release smoke wraps those checks:

```sh
./scripts/test-e2e-fake.sh
```

Useful flags:

```sh
agentnoise fake-phone roundtrip --timeout-seconds 120 /status
agentnoise fake-phone roundtrip --root /tmp/agentnoise-fake-phone /help
agentnoise fake-phone roundtrip --require-job-final --expect done /codex "Reply exactly: done"
```

If the command reports a timeout, run `agentnoise doctor` and `cargo test
fake_phone` first. If the synthetic smoke passes but a real phone is silent,
debug the live listener with `agentnoise up`, `agentnoise status`, and
`runtime-events.jsonl`.
