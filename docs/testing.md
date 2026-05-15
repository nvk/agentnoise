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
pairs through the printed SSH PIN, and verifies `/status` and `/help` replies.
It still depends on the local White Noise binaries and relay reachability, so it
is intentionally separate from the offline release check.

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
