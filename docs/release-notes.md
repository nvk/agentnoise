# Release Notes

## Unreleased

- Added an opt-in local session watcher. `agentnoise config local-sessions-watch
  on` makes the listener notify the primary paired chat when new same-account
  Codex/Claude session metadata appears, while the default remains off for
  privacy.

## 0.1.23 - 2026-05-18

- Added `whitenoise://chat/<group>` open links to session list, new-session,
  and cross-session resume replies, shortened chat refs to five characters, and
  added `/jump <session>` as a friendlier `/resume` alias.
- Made the primary paired chat behave as an inbox for new jobs. `/codex`,
  `/claude`, `/hermes`, and `/wiki` commands from that chat now open a fresh
  White Noise work session named `hostname - prompt summary`; progress and
  final output continue in the new chat, while follow-up jobs inside that chat
  stay there.
- Reconcile saved White Noise control chats against the active `wn groups list`
  result at listener startup so removed groups do not keep appearing as live
  sessions.

## 0.1.22 - 2026-05-17

- Added a startup no-output watchdog for agent launches. If Codex, Claude,
  Hermes, or the launcher emits no stdout/stderr during startup, agentnoise
  terminates that launch, retries once by default, then reports a clear failure
  instead of leaving the phone chat with a permanently running job.
- Changed direct Codex launches to start from agentnoise's stable data
  directory while still passing the selected workspace through `codex -C`.
  This avoids service-launched children stalling when the repo lives under
  iCloud Drive or another GUI-backed folder.
- Added doctor/runtime hints for iCloud Drive and CloudDocs repo paths, because
  Codex itself can hang under launchd before writing output when `-C` points
  there.
- Added a macOS launchd guard for Codex jobs. When agentnoise is running as a
  launchd/brew service, `/codex` now fails immediately with a service-mode
  explanation instead of accepting a job that will not produce output.
- Made reply send failures non-fatal for the listener and limited startup
  hellos to the primary configured chat, avoiding restart loops and historical
  group spam when White Noise reports pending MLS proposals.
- Filtered removed/self-removed White Noise groups during discovery and
  automatically accepted pending control-chat confirmations from the paired
  sender, so stale fake-phone groups and unconfirmed DMs do not leave the phone
  seeing an accepted job with no useful follow-up.

## 0.1.21 - 2026-05-16

- Added chat-visible "still running" pings for jobs that are alive but have
  produced no new output, with `/tail <job>` and `/cancel <job>` hints.
- Added `runner.silence_ping_seconds` so quiet-job pings can be tuned or
  disabled.

## 0.1.20 - 2026-05-16

- Hardened `agentnoise fake-phone roundtrip` so it can require a final job
  reply and expected text, not just the initial command ack.
- Extended the live fake-phone E2E smoke to send a `/codex` command and require
  both ack and final reply through White Noise.
- Run Codex jobs with `--skip-git-repo-check` so phone-launched jobs work in
  configured workspace directories that are not Git repos.

## 0.1.19 - 2026-05-16

- Added local agent session visibility with `agentnoise local-sessions` and
  `/agent-sessions`, listing recent Codex/Claude session metadata and explicit
  resume commands without returning transcript content.
- Added a configurable job timeout so wedged launcher/agent processes are
  terminated and reported instead of staying `running` forever.
- Run Codex-through-bondage from the agentnoise data dir while still passing
  the selected repo through `codex exec -C`, avoiding fragile launcher `getcwd`
  behavior in iCloud-backed workspaces.

## 0.1.18 - 2026-05-16

- Added Homebrew caveats with the simple raw Codex/Claude setup path,
  background service command, and config discovery commands.
- Added a configuration guide with raw CLI, `bondage`, repo alias, identity,
  and agent profile variant examples.
- Improved missing profile errors so users get a direct-mode fallback and a
  manual link instead of a bare launcher failure.

## 0.1.17 - 2026-05-16

- Added config-driven agent profile variants. A config entry like
  `[[agents.codex.profiles]] name = "fix"` exposes `/codex-fix` and
  `/codex-fix-resume` without hardcoding frontier-specific profile names.

## 0.1.16 - 2026-05-15

- Added a startup hello for already-paired control chats. When agentnoise
  reaches the listener, it posts `agentnoise is up` with an RFC3339 UTC
  timestamp, profile, and workspace so the phone can tell the service is alive
  after a restart.

## 0.1.15 - 2026-05-15

- Fixed Homebrew service startup churn that could repeatedly touch the macOS
  Keychain when `wnd` was slow to become ready.
- Extended White Noise daemon startup readiness from 5 seconds to 60 seconds.
- Avoided a second daemon startup check in `agentnoise up` after setup already
  started and verified the daemon.

## 0.1.14 - 2026-05-15

- Added `runner.launcher = "direct"` for installs that only have raw Codex,
  Claude, or Hermes CLIs and do not want to configure `bondage`.
- Added `--direct-agents` to `agentnoise init`, `agentnoise setup`, and
  `agentnoise up` so first-run setup can persist direct mode.
- Added `agentnoise config launcher <bondage|direct>` for existing configs.
- Kept `bondage` as the default launcher and updated `doctor`, `status`, and
  `agents` output to make the active launcher visible.
- Reworked phone-facing session replies for `/new`, `/list`, `/resume`,
  `/close`, and `/status` so they name the current chat, target chat,
  workspace, and next action without relying on symbolic markers.
- Added `scripts/test-chat-ux.sh`, an offline fake-phone chat smoke test with
  an optional real frontier Codex leg via `AGENTNOISE_CHAT_UX_FRONTIER=1`.
- Hardened the live fake-phone E2E harness so it rebuilds before running,
  waits for a fresh pairing PIN, ignores pairing/catch-up noise, and asserts
  real `/status` and `/help` replies.
- Trimmed default message relays to a smaller set that behaved reliably in
  live White Noise tests, and retried key-package publishing when the daemon
  times out on slow relay work.
- Standardized pairing QR rendering on the bare desktop `npub`, while keeping
  the richer `nprofile` and relay hints printed as adjacent text.
- Kept `agentnoise status` and `agentnoise doctor` responsive by avoiding
  implicit OS keychain probes and bounding White Noise daemon status checks.
- Documented the practical White Noise delivery diagnostic: agentnoise can
  enqueue and persist a reply locally before the phone app renders it, so
  phone smoke tests should distinguish local send success from phone receipt.

## 0.1.13

- Normalized `wn whoami --json` login detection across hex pubkeys and
  configured `npub` account values.
- Preserved the invariant that agentnoise only listens after White Noise has a
  usable signing account, while avoiding unnecessary Keychain repair when that
  account is already logged in.

## 0.1.12

- Reused the cached desktop `npub` from config for pairing QR/profile setup so
  normal `agentnoise up` and Homebrew service restarts do not load the desktop
  `nsec` unnecessarily.
- Limited OS keychain access during startup to identity creation and real White
  Noise login repair.
- Added clearer keychain repair guidance for non-interactive service contexts
  where macOS may not show an authorization prompt.

## 0.1.11

- Normalized Nostr sender identity checks across hex pubkeys and `npub`
  values.
- Fixed the confusing “not paired” replies caused by agentnoise seeing its own
  outbound White Noise messages as inbound unpaired senders.
- Kept paired phone allowlist matching compatible with either hex or `npub`
  sender values.

## 0.1.10

- Added explicit replies for bare text, unknown commands, unauthorized senders,
  and startup catch-up messages that were previously easy to mistake for a dead
  daemon.
- Added three-attempt reply sends with outbound journal details when White
  Noise send fails.
- Added `agentnoise identity status` and `agentnoise identity rename <name>` so
  a machine's published White Noise/Nostr label can be checked or changed
  without supplying a phone `npub`.
- Accepted `agentnoise -- help` as a forgiving alias for normal CLI help.
- Added configured White Noise message relays, reconciled as `nip65`, `inbox`,
  and `key_package` account relays after login, plus `agentnoise whitenoise
  relays` and `agentnoise whitenoise ensure-relays` for inspection/repair.

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
