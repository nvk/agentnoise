# Release Notes

## 0.1.9

- Added SSH pairing mode with terminal-only PIN display via `agentnoise up
  --ssh --phone npub1...`.
- Added `--name` for publishing a distinct White Noise/Nostr profile name per
  machine.
- Added a remote SSH pairing guide that keeps `nsec` off the wire and uses the
  phone `npub` for remote chat creation.

## 0.1.8

This pass focuses on parity and operability while keeping the trusted path
native Rust plus `bondage`.

- Added `agentnoise fake-phone` for isolated White Noise test chats using a
  separate `wnd` data directory and fake-phone-owned burner `nsec`.
- Added `agentnoise status`, durable runtime event journaling, and richer
  doctor coverage for event, approval, and attachment stores.
- Added progress messages for Codex and Claude JSON streams with a conservative
  rate limit.
- Added approval objects for profiles that look intentionally elevated, with
  `/approvals`, `/approve`, and `/deny`.
- Added attachment metadata capture with `/attachments` and `/attach`.
- Added `/agents` and `agentnoise agents` for local capability visibility.
- Added opt-in git worktree sessions with `/worktree new`, `/worktree use`, and
  confirmed removal.
- Added a tested JSON-line Unix socket probe for future direct `wnd` transport.
- Added `scripts/release-smoke.sh` for local fmt, clippy, tests, build, and
  basic CLI command checks without depending on hosted CI.

## 0.1.7

Optional Hermes backend. `/hermes` and `/hermes-resume` route Hermes through the
same `bondage` local policy boundary used for Codex and Claude. Live Hermes CLI
execution remains alpha.

## 0.1.6

Service console and dev burner identity. `agentnoise up` can attach to an
already-running service as a local console, and disposable development
identities can use `--dev-burner-nsec` without repeated OS keychain prompts.
