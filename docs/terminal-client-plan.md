# Terminal Client Plan

Status: design plan, not shipped.

## Goal

Build a human-driven terminal client for White Noise inside the agentnoise
repo. The first use is reliable end-to-end testing of agentnoise as a user
would experience it: a separate phone-like identity sends messages, watches
the same chat, follows handoff sessions, and verifies replies without relying
on the mobile app.

The longer-term goal is a real White Noise terminal client with feature parity
with the current app where the upstream `wn` and `wnd` interfaces expose the
needed behavior.

## Rationale

The automated `agentnoise fake-phone` harness is useful, but it is not enough.
It can prove a narrow roundtrip, but it does not let us drive the app like a
person, inspect session handoffs, switch chats, see formatting problems, or
debug delayed replies interactively.

The mobile app remains the real user target, but a terminal client gives us:

- a repeatable fake-phone identity that does not use the desktop bot keychain;
- a way to test over SSH on machines such as frontier;
- direct visibility into White Noise chats, events, and send state;
- a path to debug app-vs-relay-vs-agentnoise failures without guessing;
- a useful standalone terminal White Noise client if it grows well.

This should live in the agentnoise repo so it can reuse the White Noise
adapter, fake-phone identity flow, config conventions, text formatting, and
test fixtures we already maintain.

## Packaging Decision

Keep the normal `agentnoise` install small and fast.

Add the terminal client as an optional Rust binary, tentatively named
`agentnoise-client`, behind a Cargo feature such as `tui`.

Example shape:

```toml
[features]
tui = ["dep:ratatui", "dep:crossterm", "dep:tui-textarea"]

[[bin]]
name = "agentnoise-client"
path = "src/bin/agentnoise-client.rs"
required-features = ["tui"]
```

The default Homebrew formula should keep installing the daemon and bundled
White Noise CLI pieces only. A separate formula, for example
`agentnoise-client`, can build the optional TUI feature later. If we want the
main CLI to discover it, `agentnoise client` can eventually exec
`agentnoise-client` when installed and print clear install guidance when it is
not.

## Reuse

Reuse existing code before adding new transport logic:

- `src/wn.rs`: message send/list/subscribe parsing, send retries, event shapes.
- `src/whitenoise_cli.rs`: `wn`/`wnd` discovery, daemon/login/group helpers,
  profile and relay helpers.
- `src/fake_phone.rs`: burner identity root, separate data directory, isolated
  fake-phone account flow, handoff link following.
- `src/text.rs`: phone-readable message cleanup rules.
- `src/attachments.rs`: media metadata and future upload/download display.
- `src/wnd_socket.rs`: direct daemon health checks.
- `src/config.rs`: paths, relays, identity, and workspace conventions.
- `tests/fixtures/wn/*`: upstream JSON contract fixtures.

The first implementation should refactor shared fake-phone pieces into a small
client core instead of copying them into the TUI.

## Architecture

Keep the UI thin and the client behavior testable without a terminal.

Proposed layout:

```text
src/client/
  mod.rs
  identity.rs      # burner/real account selection
  transport.rs     # White Noise commands and subscription bridge
  state.rs         # chats, messages, selected chat, send state
  commands.rs      # slash commands and local client actions
  transcript.rs    # raw event capture and replay
  render.rs        # view models, not terminal drawing

src/bin/agentnoise-client.rs
```

The non-TUI client core should compile in normal tests without terminal
dependencies where practical. The TUI binary owns `ratatui`, `crossterm`,
keyboard handling, layout, and terminal lifecycle only.

## Identity Modes

Support three modes, in this order:

1. Burner fake-phone mode for testing. Uses a separate White Noise data root
   and a local burner `nsec` file under that root. It must not read or write
   the agentnoise desktop bot `nsec` or keychain item.
2. Existing White Noise account mode. Uses the configured `wn`/`wnd` account
   when the user intentionally wants a terminal client for their own identity.
3. Explicit import mode. Accept an `nsec` path or stdin only for throwaway test
   accounts, with redacted logs and clear warnings.

For agentnoise testing, burner fake-phone mode is the default.

## Milestones

### 1. Human Fake Phone

Build the smallest useful terminal client:

- create or reuse a burner fake-phone identity;
- show the fake-phone `npub`;
- create/open a chat with the configured agentnoise desktop `npub`;
- send free text and slash commands;
- subscribe to replies live;
- follow `whitenoise://chat/<group>` handoff links from agentnoise;
- show raw event ids and group ids on demand, shortened in the main view;
- write a transcript JSONL file for failing tests.

This should make it possible to test:

```text
/status
/help
/codex whats 1+1
plain follow-up text in a work chat
/tail <short-job>
/cancel <short-job>
```

### 2. Chat Browser

Add the basic White Noise client shell:

- list chats and groups;
- switch chats;
- create a chat by `npub`;
- rename, archive, unarchive, mute, and unmute chats;
- accept and decline invites;
- search messages;
- show members and admins.

### 3. Media And Profile

Add the app-level features exposed by `wn`:

- upload, download, and list media;
- show and update the active profile;
- list accounts;
- search users;
- list/add/remove/check follows;
- show settings;
- subscribe to notifications.

### 4. Full Parity Pass

Track parity against the White Noise app and upstream CLI surface:

- messages: list, send, delete, retry, search, search-all, subscribe, react,
  unreact;
- chats: list, show, subscribe, archive, unarchive, list-archived,
  subscribe-archived, mute, unmute;
- groups: list, create, show, add/remove members, members, admins, relays,
  leave, rename, invites, accept, decline, promote, demote, self-demote,
  subscribe-state;
- media: upload, download, list;
- accounts: list;
- users: show, search;
- follows: list, add, remove, check;
- profile: show, update;
- settings: show, theme, language;
- notifications: subscribe.

If the app has behavior that `wn` does not expose yet, record it as an upstream
dependency instead of inventing a fragile local protocol.

### 5. Test Harness Mode

Make the client driveable without a human for release testing:

- send scripted input;
- assert replies by substring or regex;
- require a final job reply;
- follow session handoff links;
- dump the final transcript;
- print actionable failure summaries with daemon status, relay hints, and last
  send/subscribe errors.

This becomes the replacement for brittle one-shot fake-phone checks while still
keeping `agentnoise fake-phone roundtrip` for quick smoke tests.

## UX Rules

- Optimize for narrow terminals and SSH first.
- Keep IDs short in the main UI, with a detail pane or command for full values.
- Show message delivery state separately from agent job state.
- Make session handoffs obvious: current chat, target chat, and how to jump.
- Keep debug output available, but out of the normal reading flow.
- Prefer keyboard commands that mirror the phone workflow: send, switch chat,
  list sessions, open handoff, tail job, cancel job.

## Security Defaults

- Do not install the terminal client by default.
- Do not share the desktop agentnoise bot identity with the client.
- Keep burner fake-phone secrets in the fake-phone root, not the OS keychain.
- Redact `nsec`, auth tokens, and private paths in transcripts unless an
  explicit unsafe/debug flag is set.
- Do not launch Codex, Claude, or Hermes from the client directly. The client
  talks through White Noise like the phone does, so agent execution still goes
  through agentnoise and its configured launcher.
- Preserve the project rule that chat commands map to structured argv arrays
  inside agentnoise; the client should only send text messages.

## Open Questions

- Can upstream White Noise expose enough read-state, reactions, reply threads,
  and media metadata to match the mobile app cleanly?
- Should `agentnoise client` be a thin wrapper that execs `agentnoise-client`,
  or should the optional binary stay completely separate?
- How should real-account mode behave on macOS over SSH when `wnd` needs
  platform keychain access?
- Should the terminal client support multiple local identities at once, or keep
  one active identity per data root?
- What is the smallest transcript format that can reproduce a failed user
  session without leaking secrets?
