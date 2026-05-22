# Marmot v2 (darkmatter) integration

As of **v0.2.0**, agentnoise embeds the Marmot v2 protocol stack — the [`darkmatter`](https://github.com/marmot-protocol/darkmatter) `marmot-app` Rust crate — directly. This document describes the integration architecture and how to validate it locally.

> "Marmot" is the protocol name. "darkmatter" is the v2 Rust workspace/binary that implements it. The Marmot v1 implementation (White Noise, with its `wn` / `wnd` CLI binaries) was removed in v0.2.0; only historical references remain in the changelog.

---

## Why v2

- v1 (White Noise) shipped as standalone binaries; agentnoise had to subprocess `wn`/`wnd` and parse JSON line streams.
- v2 (darkmatter) ships a `marmot-app` crate that frontends embed directly: native async API, tokio broadcast subscriptions, structured types, no IPC roundtrips.
- v2 adds an explicit **agent text stream** component (QUIC live preview) — purpose-built for streaming codex/claude output back to the chat, replacing v1's chunk-into-multiple-MLS-messages workaround.
- v2 splits the v1 monolithic `marmot_group_data` MLS extension into versioned components (profile, admin-policy, nostr-routing, image, retention, agent-text-stream).

The chosen integration strategy was: **embed `marmot-app` as a library**, **clean replacement** of v1 code paths, **adopt agent text streams**, **single managed account label** (`agentnoise-desktop`).

---

## Architecture

```
┌────────────────────────────────────────────────────────────────────┐
│ agentnoise (Rust binary)                                           │
│                                                                    │
│  CLI dispatch ──► run_listener ──► tokio runtime                   │
│                                       │                            │
│                                       ▼                            │
│                          ┌──────────────────────────┐              │
│                          │ DarkmatterEngine         │              │
│                          │ (src/darkmatter_app.rs)  │              │
│                          │   MarmotApp +            │              │
│                          │   MarmotAppRuntime       │              │
│                          └────────────┬─────────────┘              │
│                                       │                            │
│           ┌───────────────────────────┼─────────────────────┐      │
│           ▼                           ▼                     ▼      │
│  ┌─────────────────┐      ┌────────────────────┐  ┌──────────────┐ │
│  │ DmClient        │      │ AgentTextStream    │  │ runtime      │ │
│  │ (src/dm.rs)     │      │ (src/dm_streams.rs)│  │ .subscribe() │ │
│  │   subscribe     │      │   start / finish   │  │   events     │ │
│  │   send_reply    │      │   transcript hash  │  │              │ │
│  └─────────────────┘      └────────────────────┘  └──────────────┘ │
│                                                                    │
└────────────────────────────────────────────────────────────────────┘
                                       │
                                       ▼
       embedded marmot-app → Nostr relays + SQLCipher per-account state
```

**File layout (post-migration):**

- `src/darkmatter_app.rs` — owns the `MarmotApp` + `MarmotAppRuntime`, single-account bootstrap (login from keychain nsec or create new identity).
- `src/dm.rs` — `DmClient` chat surface: subscribe to a group, send replies. Models the legacy `WnClient` API shape so the rest of the codebase can be ported with minimal cascading changes.
- `src/dm_streams.rs` — `AgentTextStream::start` / `::finish` wrap the v2 QUIC live-preview primitive; stream_id is derived deterministically from a job UUID via SHA-256.

**Dependencies** (added in `Cargo.toml`):

```toml
marmot-app    = { path = "../darkmatter/crates/marmot-app" }
marmot-account = { path = "../darkmatter/crates/marmot-account" }
cgka-traits   = { path = "../darkmatter/crates/traits" }
tokio         = { version = "1", features = ["macros", "rt", "rt-multi-thread", "sync", "time"] }
hex           = "0.4"
```

A symlink `../darkmatter` → `/Users/jeff/code/darkmatter` is created per worktree so the same relative path resolves from both the main checkout and any git worktree.

---

## Concept-rename map (v1 → v2)

| Marmot v1 / White Noise                    | Darkmatter v2                                                                    |
| ------------------------------------------ | -------------------------------------------------------------------------------- |
| `wn` / `wnd` subprocess                    | embedded `MarmotApp::with_relays(...)` + `MarmotAppRuntime::new(...)`            |
| `wn login <nsec>`                          | `runtime.login(nsec, AccountSetupRequest { ... })`                               |
| `wn messages subscribe --json <group>`     | `runtime.subscribe_messages(account_id, AppMessageQuery { group_id_hex, ... })`  |
| `wn messages send <group> <text>`          | `runtime.send_message(account_id, &group_id, text.as_bytes().to_vec())`          |
| `wn groups accept`                         | (automatic — listen for `MarmotAppEvent::GroupJoined`)                           |
| `wn key-packages publish`                  | `runtime.publish_key_package(account_id)`                                        |
| `wn relays set ...`                        | `runtime.publish_account_relay_lists(label, AccountRelayListBootstrap { ... })`  |
| `~/.local/agentnoise/whitenoise-cli/bin/`  | `~/.local/agentnoise/darkmatter/` (SQLCipher per-account, keys via OS keychain)  |
| `marmot_group_data` (monolithic extension) | Versioned components: profile / admin-policy / nostr-routing / image / retention / agent-text-stream |
| (none — chunked-message workaround)        | `runtime.start_agent_text_stream(...)` / `finish_agent_text_stream(...)`         |

---

## Status

| Phase | Description | State |
| ----- | ----------- | ----- |
| 0     | Cargo deps + worktree symlink                            | ✅ done |
| 1a    | Read marmot-app API surface                              | ✅ done |
| 1b    | `src/darkmatter_app.rs`, `src/dm.rs`, `src/dm_streams.rs` skeletons | ✅ done |
| 2     | `WhitenoiseConfig` → `DarkmatterConfig` + serde alias    | ✅ done |
| 3     | Listener swap                                            | ✅ done |
| 4     | `send_message` + agent text streams wired through `runner.rs`'s progress callback | ✅ done — `AgentTextStream::start_blocking` publishes the stream start, opens the brokered-QUIC publisher, progress chunks go to the broker, and `finish_blocking` publishes the final Marmot payload with the darkmatter transcript hash + chunk count |
| 5     | Delete v1 modules + CLI cleanup                          | ✅ done |
| 6     | Fake-phone harness rewrite                               | ✅ done — in-process `MockRelay` + single `MarmotApp` with two managed accounts (`desktop` + `phone`); validated locally with `/help` and `/codex echo` both passing `--require-job-final` |
| 7     | Docs + 0.2.0 release wiring                              | ✅ done |
| —     | **`MarmotAppEvent::GroupJoined` auto-discovery**         | ✅ done — phone-initiated groups auto-register and persist to config |

## How to validate locally

```bash
# Simple reply roundtrip
agentnoise fake-phone roundtrip --timeout-seconds 30 --require-job-final "/help"

# Stream lifecycle assertion (asserts both expectations matched + AgentStreamFinalized seen)
agentnoise fake-phone roundtrip \
  --timeout-seconds 30 --require-job-final \
  --expect "codex queued" --expect "completed" \
  "/codex echo hello"

# Confirm the embedded engine bootstraps cleanly
agentnoise darkmatter probe --relay wss://relay.primal.net
```

The fake-phone harness spins up an in-process Nostr relay (`nostr_relay_builder::MockRelay`), builds a single `MarmotApp` with two managed accounts, has the phone create a group with the desktop, and asserts the round-trip flow including agent text stream start/finalize envelopes.

## Named instances and keychain isolation

`agentnoise --instance <name>` runs a fully isolated instance: its own config
root (`instances/<name>/`), data/log/worktree dirs, service/launchd label, and
Marmot v2 profile name. Crucially, each instance also gets its own OS-keychain
**service** name: `DarkmatterEngine::open` is passed
`keychain_service_for_instance(config.instance)` →
`"agentnoise-<name>"` (or plain `"agentnoise"` with no instance). Combined with
the per-instance `data_dir` (which gives each instance its own marmot-account
home + group DBs), two instances on one machine share nothing — separate
accounts, separate secrets, separate keychain services. See
[docs/configuration.md](configuration.md#named-instances).

## Limitations still in v0.2.0

The v2 listener landed as a single-group implementation. Compared to v1 it temporarily drops:

- **Multi-group operation.** v1 spawned per-group subscription processes and routed across many control chats. v2 subscribes only to the configured `darkmatter.group_id` (plus `group_ids`). Multi-group fan-out is a follow-up.
- **Parallel session creation.** `/codex`/`/claude` with `NewSession` or `ResumeSession` routing returns a polite "not yet wired through darkmatter v2" message. The desktop-side `runtime.create_group` call from a chat command is a follow-up.
- **Startup hello + retry backoff.** The "agentnoise up @ HH:MM" greeting and the pending-proposal retry backoff in v1's `send_reply_recorded` are dropped pending a v2 rewrite.
- **QUIC preview dependency.** Live progress now uses the configured broker (`darkmatter.agent_text_stream_broker`, default `https://quic-broker.ipf.dev:4450`). If broker startup/connect/publish fails, agentnoise falls back to ordinary chat progress for that job while still sending the normal final reply.

---

## Validating progress

The embedded engine has a smoke-test subcommand:

```bash
# Use configured message_relays
agentnoise darkmatter probe

# Or pass relays explicitly
agentnoise darkmatter probe --relay wss://relay.primal.net --relay wss://relay.damus.io

# JSON output for scripting
agentnoise darkmatter probe --json
```

What it does:

1. Builds a multi-thread tokio runtime.
2. Constructs `DarkmatterEngine::open(home, relays)` — by default `<data_dir>/darkmatter`.
3. Calls `runtime.start()` to spawn account workers.
4. Calls `ensure_account("agentnoise-desktop", None, relays)` — creates the managed account if missing, otherwise looks it up.
5. Prints account label, account_id_hex, home, and relays.
6. Cleanly shuts the runtime down.

Successful output proves the dep tree compiles, marmot-app bootstraps, and the account home + relay plane are reachable.

---

## What a follow-up listener port looks like

The current `run_listener` in `src/main.rs` (lines ~1443–2700) does a lot:

- Acquires an exclusive engine lock.
- Restores White Noise login from keychain (`whitenoise_cli::ensure_login_from_configured_nsec`).
- Reconciles message relays.
- Discovers + accepts pending groups (polling `wn groups list`).
- Spawns one `wn messages subscribe --json <group>` per group as a child process; parses JSON lines into `wn::MessageEvent`.
- Multiplexes events + group-discovery + local-session-watcher + pairing-PIN-display into a `std::sync::mpsc<StreamItem>` loop.
- Routes events through `AgentApp::route_message` and sends replies via `WnClient::send_reply_to`.

The v2 shape:

- Replace ensure-login / ensure-relays with `DarkmatterEngine::ensure_account` + `runtime.publish_account_relay_lists`.
- Replace group discovery polling with `runtime.subscribe()` broadcast filtered on `MarmotAppEvent::GroupJoined`.
- Replace per-group subscription processes with `DmClient::subscribe()` returning a tokio `mpsc::Receiver<MessageEvent>`.
- Replace `wn.send_reply_to` with `DmClient::send_reply`.
- Replace chunked agent output with `AgentTextStream::start` + `record_chunk` + `finish` in `runner.rs`'s progress callback.
- Switch `fn main()` to `#[tokio::main(flavor = "multi_thread")]` or build a runtime explicitly in the listener path.

The `dm::MessageEvent` struct is shaped to match `wn::MessageEvent` field-for-field (minus attachments, which need a v2 media-component bridge), so `AgentApp::route_message` does not change.

---

## Open questions

- **Attachments**: `wn::MessageEvent` carries `attachments: Vec<AttachmentInfo>`; v2 surfaces media via `MarmotAppMessagePayloadV1::Media` and `AppGroupImageComponent` (Blossom hash). A bridge is needed.
- **QUIC candidate discovery**: `runtime.start_agent_text_stream` takes a `Vec<String>` of QUIC candidates. The exact production source (e.g., asking the `transport-quic-stream` crate for the bound addresses) is TBD.
- **Pairing PIN parity**: HMAC-SHA256 PIN logic in `src/auth.rs` is protocol-agnostic and survives unchanged; what changes is the storage location of allowed senders (npub allowlist still works since both stacks share Nostr key identity).
