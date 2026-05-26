# Fake Phone Testing

`agentnoise fake-phone` is for local bring-up when you want to test the desktop
helper without using your real phone identity.

There are two darkmatter test paths:

- `fake-phone live-roundtrip` starts an isolated real `agentnoise transport run`
  process, a local mock relay, and a separate fake phone identity. This is the
  release smoke because it exercises group discovery, routing, replies, and the
  runtime event journal.
- `fake-phone roundtrip` is protocol-only. It uses one in-process `MarmotApp`
  plus a synthetic desktop responder. It is useful for stream-envelope checks,
  but it does not prove the real daemon can reply.

The live harness does not touch your normal agentnoise config, identity, OS
keychain item, or paired chats. It creates a temporary desktop config under the
fake-phone root, uses a development burner desktop identity, creates a separate
fake phone account, then has the phone create a darkmatter group with the
desktop.

Inspect paths:

```sh
agentnoise fake-phone plan
```

Real daemon smoke:

```sh
agentnoise fake-phone live-roundtrip --expect running /status
agentnoise fake-phone live-roundtrip --expect /status /help
agentnoise fake-phone live-roundtrip \
  --start-worker \
  --min-replies 2 \
  --expect "codex queued" \
  --expect "agentnoise-darkmatter-live-ok" \
  --require-job-final \
  /codex "Reply with exactly: agentnoise-darkmatter-live-ok"
```

Full scripted smoke:

```sh
./scripts/test-e2e-fake.sh
```

The script runs `/status`, `/help`, and `/codex` through the live harness. For
`/codex`, it starts an isolated worker with a fake local Codex binary, then
requires the final job reply. It also requires the daemon to write both inbound
and successful outbound entries to `runtime-events.jsonl`, so a phone-visible
reply without daemon journaling is not considered a pass.

Useful flags:

```sh
agentnoise fake-phone live-roundtrip --timeout-seconds 120 /status
agentnoise fake-phone live-roundtrip --root /tmp/agentnoise-dm-fake /help
agentnoise fake-phone live-roundtrip --start-worker --expect "agentnoise-darkmatter-live-ok" /codex test
```

If a live roundtrip fails, the command prints the isolated transport stdout,
stderr, and event-log paths plus excerpts. Start there before checking the real
phone or real service.
