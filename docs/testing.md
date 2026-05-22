# Testing

agentnoise has four local test layers.

Fast local checks:

```sh
just test-fast
# or
./scripts/test-fast.sh
```

This runs format, clippy, and the full Rust test suite.

Offline chat UX smoke:

```sh
./scripts/test-chat-ux.sh
```

This uses temp state and a fake phone sender to exercise `/status`, `/rename`,
`/cd`, `/list`, `/new`, `/resume`, and `/close` without touching a real Marmot
identity or OS keychain. To also run one direct Codex job through that same fake
chat flow:

```sh
AGENTNOISE_CHAT_UX_FRONTIER=1 ./scripts/test-chat-ux.sh
```

Fixture contracts:

```sh
just test-fixtures
# or
./scripts/test-fixtures.sh
```

This is kept as a narrow contract target for checked-in fixture tests. Under the
Marmot v2 migration it may be empty until new fixture contracts are added, but
it remains part of the release preflight so future fixture coverage has a stable
entry point.

Synthetic fake-phone protocol smoke:

```sh
just test-e2e-fake
# or
./scripts/test-e2e-fake.sh
```

This starts an in-process mock relay and dual Darkmatter runtime, verifies
`/status` and `/help`, then sends `/codex Reply with exactly:
agentnoise-fake-phone-e2e-ok` and requires an agent-text-stream finalization.
It does not start the live listener or depend on real relays, a real phone, or a
working Codex profile.

Release preflight:

```sh
just release-check
# or
./scripts/release-check.sh
```

This stays local and does not depend on hosted CI. It runs the fast checks,
fixture contracts, the offline chat UX smoke, a Cargo package file-list check, a
locked offline release build, and formula placeholder checks.

Use `release-check` as the offline gate. Add one manual real-phone smoke before
shipping changes that affect service startup, pairing, live relay routing,
agent launching, job lifecycle, or reply formatting.

Manual phone smoke:

1. Start or attach with `agentnoise up`.
2. Scan the QR with the Marmot v2 phone client if this is first pairing.
3. Create or open the control group with the desktop identity.
4. Send the displayed PIN if `allowed_senders` is still empty.
5. Send `/status` and confirm the phone displays the reply.

If the phone is silent, check `runtime-events.jsonl` before restarting anything.
A local outbound record with no phone display points at relay/mobile sync, not
agent command routing.
