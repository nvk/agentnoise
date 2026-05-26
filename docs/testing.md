# Testing

agentnoise has three local test layers.

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
`/cd`, `/list`, `/new`, `/resume`, and `/close` without touching your real
darkmatter identity or OS keychain. To also run one real direct Codex/frontier
job through that same fake chat flow:

```sh
AGENTNOISE_CHAT_UX_FRONTIER=1 ./scripts/test-chat-ux.sh
```

Legacy White Noise adapter contract fixtures:

```sh
just test-fixtures
# or
./scripts/test-fixtures.sh
```

The fixture tests cover checked-in `wn` JSON shapes for `whoami`, group
discovery, relay type merging, and message subscription streams. These tests
should catch upstream output-shape changes before a real phone or service is
involved.

Isolated darkmatter fake-phone live roundtrip:

```sh
just test-e2e-fake
# or
./scripts/test-e2e-fake.sh
```

This starts an isolated real `agentnoise transport run` process against a local
mock relay, creates a separate fake phone identity, has the phone create a
darkmatter group with the desktop, verifies `/status` and `/help`, then sends a
`/codex` prompt through an isolated worker backed by a fake local Codex binary,
and requires the final job reply. It also checks that `runtime-events.jsonl`
contains both inbound and successful outbound events.

Run this live roundtrip once as a pre-release smoke on a workstation. It is
self-contained and uses burner identities, but it still starts real local
processes, so avoid putting it in a tight repeat loop on the primary machine.

Release preflight:

```sh
just release-check
# or
./scripts/release-check.sh
```

This stays local and does not depend on hosted CI. It runs the fast checks,
fixture contracts, the offline chat UX smoke, `cargo package --offline`, and
formula placeholder checks.

Use `release-check` as the offline gate. Add one live fake-phone smoke after
that before shipping changes that affect service startup, darkmatter routing,
agent launching, job lifecycle, or reply formatting.

Manual phone smoke:

1. Start or attach with `agentnoise up`.
2. Send `/status` from the real phone chat.
3. Confirm the phone displays the reply.
4. If the phone is silent, check `runtime-events.jsonl` before restarting
   anything. A local `reply-sent` with no phone display means the agent routed
   and sent the reply locally; the remaining failure is delivery or app sync.
