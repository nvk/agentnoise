# Release Notes

## 0.1.41 - 2026-06-27

- Disables the legacy macOS `local.agentnoise.wnd` LaunchAgent during White Noise daemon restart/recovery when that old agentnoise-owned plist is present.
- Prevents Homebrew transports from repeatedly reviving stale `/Users/.../bin/wnd` daemons after v0.1.40 migrates `wn_bin` back to the packaged CLI.

## 0.1.40 - 2026-06-27

- Migrates legacy managed White Noise CLI paths such as `/Users/.../bin/agentnoise-whitenoise/wn` back to the packaged `wn` next to the installed `agentnoise` binary.
- Restarts the White Noise daemon on transport startup after that migration so stale old `wnd` processes do not keep owning the socket.
- Restarts the White Noise daemon before retrying sends that fail with closed-connection, not-connected socket, broken-pipe, connection-refused/reset, or too-many-open-files errors.
- Keeps the v0.1.39 supervised tmux worker behavior, so daemon/send failures no longer strand the worker or the queue.

## 0.1.39 - 2026-06-27

- Changed `agentnoise worker start --tmux` to run a supervised tmux loop that
  restarts the worker if the worker process exits. This avoids the recurring
  failure mode where the transport stays healthy but remote jobs pile up queued
  because the tmux worker disappeared.
- Kept queued-job send/reply failures from bubbling out of the worker loop after
  the job has already been marked succeeded or failed; the worker logs those
  reply errors and continues claiming future jobs.
- Updated the offline-worker chat hint to point users at the supervised tmux
  worker path.

## 0.1.38 - 2026-06-24

- Added primary-chat onboarding for the first two main-chat replies, explaining
  that the primary paired chat is the launcher for new work chats.
- Updated pairing, startup hello, handoff, ready, help, and bare-text replies so
  users learn that work chats accept plain-text follow-ups without slash
  commands once an agent mode is established.

## 0.1.37 - 2026-06-24

- Added `/doctor` as a paired chat command so trusted phone clients can trigger the same diagnostics summary as `agentnoise doctor` remotely.

## 0.1.36 - 2026-06-06

- Made direct raw Codex/Claude launching the default for newly generated
  configs so public first-run setup no longer requires agentbondage.
- Added `--bondage` / `--secure` setup flags for operators who want the
  hardened `codex-agentnoise` / `claude-agentnoise` profile path.
- Promoted `agentnoise start` to the friendly setup/listen alias; it now runs
  the same first-run path as `agentnoise up` instead of requiring a preexisting
  config.
- Added same-chat/latest-job context to bare work-chat follow-up prompts before
  they enter the transport queue, keeping ambiguous requests like "show me the
  write-up here" tied to the current White Noise work chat instead of a random
  matching wiki topic.
- Made runtime event journal appends single-record writes so the transport and
  worker processes do not interleave JSONL records and break post-mortems.
- Updated Homebrew and configuration docs to show direct mode first while
  keeping the bondage install path explicit.

## 0.1.35 - 2026-06-03

- Fixed Claude Code launches with `--output-format stream-json` by adding the
  now-required `--verbose` flag for both new `/claude` jobs and
  `/claude-resume` jobs.
- Kept new Claude prompts before variadic `--add-dir` arguments so Claude does
  not accidentally treat the user prompt as another directory when the workspace
  is attached.

## 0.1.34 - 2026-06-03

- Kept failed Codex/Claude jobs from dumping raw JSON stream fragments into
  phone chat when no final assistant answer was decoded; failed replies now use
  a concise failure reason and leave raw logs in `/tail <job>`.

## 0.1.33 - 2026-06-03

- Made quiet-mode heartbeats less chatty: `progress_mode = "quiet"` now clamps
  still-running chat pings to a five-minute minimum, while
  `silence_ping_seconds = 0` still disables them entirely.
- Reworded the still-running ping to say how long the job has had no output and
  keep `/tail` plus `/cancel` on one compact action line.

## 0.1.32 - 2026-06-03

- Fixed a `0.1.31` active-job follow-up regression where agentnoise's own
  echoed outbound messages could bypass normal bot/sender filtering and trigger
  repeated "Still working" replies in a work chat.
- Shortened the active-job follow-up notice to a compact phone-safe status with
  `/tail` and `/cancel` hints.

## 0.1.31 - 2026-06-03

- Added `runner.progress_mode` with a default `quiet` mode that suppresses raw
  command/tool progress and routine agent self-narration from phone chat while
  keeping approvals, errors, retries, and quiet-job pings visible.
- Reworked job accepted, progress, work-chat handoff, and final-result copy to
  be outcome-first mobile chat messages instead of job-log style text. Long
  finals are compacted for phone and point to `/tail <job>` for the full answer.
- Strengthened the injected agent prompt with explicit White Noise mobile-chat
  delivery context: short result first, no raw logs or internal narration, and
  compact wiki digests with full detail stored in files.
- Prevented bare follow-up text in a work chat with an active queued job from
  silently launching a second job; agentnoise now explains that the previous
  job is still active and offers `/cancel`.

## 0.1.30 - 2026-06-02

- Extended White Noise media ingest beyond images to the full current chat-media
  allowlist: JPEG/PNG/GIF/WebP, MP4/WebM/MOV, MP3/OGG/M4A/WAV, and PDF.
  Supported media is copied into `.agentnoise/attachments/` inside the active
  workspace, included in captioned agent prompts and `/wiki` file-ingestion
  context, and referenced workspace media can be uploaded back to chat.

## 0.1.29 - 2026-06-02

- Made first-run White Noise discovery publishing local-first: setup now keeps
  the newly created desktop identity even if relay-list/profile/key-package
  publication times out, prints a warning, and lets listener startup retry the
  relay reconciliation.
- Added White Noise picture ingest: phone-sent images are saved, downloaded
  into `.agentnoise/attachments/` inside the active workspace when a media hash
  is present, and added to captioned agent prompts automatically. `/wiki`
  image prompts now use the LLM Wiki file-ingestion framing, and agent job
  replies that reference image files inside the active workspace are uploaded
  back to the chat.
- Added `agentnoise fake-phone tui`, compiled into the default `agentnoise`
  binary for Homebrew installs. It provides a human-driven burner fake phone
  with live replies, local `:attach <path> [caption]` media sends, chat
  switching, and automatic job handoff following.

## 0.1.28 - 2026-05-23

- Split the managed service path into `agentnoise transport run` plus
  `agentnoise worker start`. The transport owns White Noise subscriptions,
  pairing, discovery, and a local SQLite queue; the worker claims queued jobs
  and runs Codex/Claude/Hermes from a login shell.
- Updated Homebrew, launchd, systemd, FreeBSD rc.d, and OpenBSD rc.d rendering
  to start transport mode instead of the all-in-one `up` path.
- Added role-specific runtime locks/status and queue counts to `agentnoise
  status`, while keeping `agentnoise up` as the foreground all-in-one path.
- Made transport mode publish the normal listener runtime status, so
  `agentnoise pair` and fake-phone tests can see the current SSH pairing PIN.
- Hardened fake-phone E2E testing for macOS SSH: `--shared-daemon` can reuse
  the GUI-authorized White Noise daemon, retry with the live pairing PIN, and
  reuse the same fake-phone chat across multiple prompts.
- Treat `wn keys publish` timeouts as recoverable when a follow-up key-package
  check shows visible packages, matching White Noise behavior observed on
  frontier where publish completed but the CLI response timed out.

## 0.1.27 - 2026-05-21

- Refresh pending White Noise group proposals immediately when a reply send
  fails with `pending proposal exists`, so the normal retry loop can recover
  faster instead of only sleeping.
- Log the active listener PID while non-interactive service startup waits on
  an existing engine lock, making stale foreground/tmux listeners visible in
  service logs.
- Capture fake-phone `wnd` stdout/stderr and report early daemon exits with a
  log excerpt, instead of timing out on a missing socket with no cause.

## 0.1.26 - 2026-05-20

- Added named instances for safer multi-tenant hosts. `agentnoise --instance
  alice ...` and `agentnoise --instance bob ...` now resolve to separate
  config roots, generated data/log/worktree dirs, keychain services, White
  Noise profile names, and native service names.
- Added White Noise subscription reconciliation. `agentnoise up` now keeps a
  subscription health snapshot, polls recent group history as a watchdog,
  recovers missed inbound messages, and restarts stale `wn messages subscribe`
  children instead of leaving phone commands accepted but unanswered.
- Added subscription health to `/status` and `agentnoise status` so stale or
  restarting chat listeners are visible during debugging.

## 0.1.25 - 2026-05-20

- Added work-chat bare replies. The primary paired chat remains an inbox that
  requires slash commands, but non-inbox work chats remember the agent/profile
  and wiki prefix that created or last explicitly ran in them. Plain text in
  those chats now continues with the remembered mode and workspace.

## 0.1.24 - 2026-05-18

- Cleaned up White Noise chat output for phones. Startup hellos, job accepted
  messages, final replies, progress pings, `/status`, `/help`, session lists,
  and local agent session metadata now use shorter plain-text blocks that avoid
  Markdown/table assumptions.
- Added short unique references for jobs and local agent sessions. `/tail`,
  `/cancel`, and `*-resume` commands can use compact ids like `an-ba257` or the
  displayed local session prefix when the prefix is unambiguous.
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
