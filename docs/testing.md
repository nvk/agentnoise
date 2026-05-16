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
White Noise identity or OS keychain. To also run one real direct Codex/frontier
job through that same fake chat flow:

```sh
AGENTNOISE_CHAT_UX_FRONTIER=1 ./scripts/test-chat-ux.sh
```

White Noise adapter contract fixtures:

```sh
just test-fixtures
# or
./scripts/test-fixtures.sh
```

The fixture tests cover checked-in `wn` JSON shapes for `whoami`, group
discovery, relay type merging, and message subscription streams. These tests
should catch upstream output-shape changes before a real phone or service is
involved.

Isolated fake-phone live roundtrip:

```sh
just test-e2e-fake
# or
./scripts/test-e2e-fake.sh
```

This starts agentnoise with a development burner identity, launches the
fake-phone harness with its own `wnd` data directory and burner phone `nsec`,
pairs through the printed SSH PIN, verifies `/status` and `/help`, then sends
`/codex Reply with exactly: agentnoise-fake-phone-e2e-ok` and requires both the
ack and the final job reply. It still depends on local White Noise binaries,
relay reachability, and a working Codex profile, so it is intentionally separate
from the offline release check.

Run this live roundtrip once as a pre-release smoke on a workstation. Do not put
it in a repeat loop on the primary machine. Repeated live relay runs belong on a
spare machine, disposable macOS install, or other environment that can be
rebooted without interrupting work.

Current White Noise daemons may still use the platform keyring for account login
after agentnoise passes a dev burner `nsec` directly. If this fails with a
`Keyring error` or `Operation not permitted`, rerun it from an unsandboxed user
session with access to the platform keyring/Secret Service.

Release preflight:

```sh
just release-check
# or
./scripts/release-check.sh
```

This stays local and does not depend on hosted CI. It runs the fast checks,
fixture contracts, the offline chat UX smoke, `cargo package --offline`, and
formula placeholder checks.

Use `release-check` as the offline gate. Add one live fake-phone job smoke after
that before shipping changes that affect service startup, White Noise routing,
agent launching, job lifecycle, or reply formatting.

Manual phone smoke:

1. Start or attach with `agentnoise up`.
2. Send `/status` from the real phone chat.
3. Confirm the phone displays the reply.
4. If the phone is silent, check `runtime-events.jsonl` and `wn messages list`
   before restarting anything. A local `reply-sent` with no phone display is a
   White Noise delivery/sync delay, not an agent command failure.
