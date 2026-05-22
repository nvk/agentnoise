//! Fake-phone test harness — runs a self-contained round-trip locally using
//! darkmatter v2 primitives.
//!
//! Mechanism:
//! 1. Boot an in-process [`nostr_relay_builder::MockRelay`].
//! 2. Build one [`marmot_app::MarmotApp`] pointing at that relay; create two
//!    managed accounts on it: `desktop` and `phone`.
//! 3. `phone.create_group([desktop])`, wait for desktop's `GroupJoined` event.
//! 4. Spawn a desktop responder that subscribes to messages, wraps each reply
//!    in an [`crate::dm_streams::AgentTextStream`] lifecycle so the phone sees
//!    `AgentStreamStarted` / `AgentStreamFinalized` events (smoke test for the
//!    v2 QUIC-live-preview wiring).
//! 5. Phone sends the requested test message and collects replies + stream
//!    events until min_replies are seen, expectations matched, or timeout
//!    fires.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use cgka_traits::TransportEndpoint;
use marmot_app::{
    AccountSetupRequest, AgentTextStreamFinishRequest, AppMessageQuery, MarmotApp, MarmotAppEvent,
    MarmotAppRuntime, RuntimeMessageUpdate,
};
use nostr_relay_builder::MockRelay;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::Config;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FakePhonePlan {
    pub root: PathBuf,
    pub data_dir: PathBuf,
    pub logs_dir: PathBuf,
    pub socket: PathBuf,
    pub nsec_file: PathBuf,
}

#[derive(Debug, Clone)]
pub struct FakePhoneRoundtrip {
    pub root: PathBuf,
    pub pin: Option<String>,
    pub message: String,
    pub group_name: String,
    pub timeout: Duration,
    pub expect: Vec<String>,
    pub min_replies: usize,
    pub require_job_final: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FakePhoneResult {
    pub phone_npub: String,
    pub group_id: String,
    pub replies: Vec<String>,
    pub matched: Vec<String>,
    pub saw_job_final: bool,
}

pub fn plan(config: &Config, root: Option<&Path>) -> FakePhonePlan {
    let root = root
        .map(Path::to_path_buf)
        .unwrap_or_else(|| config.resolved_data_dir().join("fake-phone"));
    let data_dir = root.join("dm-data");
    let logs_dir = root.join("dm-logs");
    let socket = data_dir.join("mock-relay.sock");
    let nsec_file = root.join("fake-phone.nsec");
    FakePhonePlan {
        root,
        data_dir,
        logs_dir,
        socket,
        nsec_file,
    }
}

/// Run the end-to-end fake-phone round-trip. Builds its own tokio runtime so
/// callers don't have to.
pub fn roundtrip(_config: &Config, options: FakePhoneRoundtrip) -> Result<FakePhoneResult> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building tokio runtime for fake-phone roundtrip")?;
    runtime.block_on(run_roundtrip(options))
}

async fn run_roundtrip(options: FakePhoneRoundtrip) -> Result<FakePhoneResult> {
    let tmp = tempfile::tempdir().context("creating fake-phone tempdir")?;
    let relay = MockRelay::run()
        .await
        .map_err(|e| anyhow::anyhow!("starting MockRelay: {e}"))?;
    let url = relay.url().await.to_string();
    let endpoints = vec![TransportEndpoint(url.clone())];

    let app = MarmotApp::with_relays(tmp.path(), vec![url.clone()]);
    let runtime = MarmotAppRuntime::new(app.clone());

    let setup = AccountSetupRequest {
        identity: None,
        default_relays: endpoints.clone(),
        bootstrap_relays: endpoints.clone(),
        publish_missing_relay_lists: true,
        publish_initial_key_package: true,
    };
    let desktop = runtime
        .create_identity(setup.clone())
        .await
        .map_err(|err| anyhow::anyhow!("creating desktop identity: {err}"))?;
    let phone = runtime
        .create_identity(setup)
        .await
        .map_err(|err| anyhow::anyhow!("creating phone identity: {err}"))?;

    let desktop_id = desktop.account.account_id_hex.clone();
    let phone_id = phone.account.account_id_hex.clone();
    let phone_npub = npub_from_account_id(&phone_id)?;

    let mut events = runtime.subscribe();

    let group_id = runtime
        .create_group(
            &phone_id,
            &options.group_name,
            std::slice::from_ref(&desktop_id),
            None,
        )
        .await
        .map_err(|err| anyhow::anyhow!("phone create_group: {err}"))?;
    let group_id_hex = hex::encode(group_id.as_slice());

    // Wait for desktop to receive the welcome.
    let desktop_id_match = desktop_id.clone();
    let group_id_match = group_id.clone();
    wait_for_event(&mut events, Duration::from_secs(5), move |event| {
        matches!(
            event,
            MarmotAppEvent::GroupJoined { account_id_hex, group_id: gid, .. }
                if account_id_hex == &desktop_id_match && gid == &group_id_match
        )
    })
    .await
    .context("desktop did not receive GroupJoined event within 5s")?;

    // Spawn the desktop responder: wraps every received message in an agent
    // text stream lifecycle and echoes a synthetic reply.
    let runtime_for_desktop = runtime.clone();
    let desktop_id_for_handler = desktop_id.clone();
    let group_id_for_handler = group_id.clone();
    let group_id_hex_for_handler = group_id_hex.clone();
    let pin_for_handler = options.pin.clone();
    let desktop_task = tokio::spawn(async move {
        let mut subscription = match runtime_for_desktop.subscribe_messages(
            &desktop_id_for_handler,
            AppMessageQuery {
                group_id_hex: Some(group_id_hex_for_handler.clone()),
                limit: None,
            },
        ) {
            Ok(subscription) => subscription,
            Err(error) => {
                eprintln!("fake-phone: desktop subscribe_messages failed: {error:#}");
                return;
            }
        };
        while let Some(update) = subscription.recv().await {
            let RuntimeMessageUpdate::Message(received) = update else {
                continue;
            };
            if received.message.sender == desktop_id_for_handler {
                continue;
            }
            if let Err(error) = handle_desktop_message(
                &runtime_for_desktop,
                &desktop_id_for_handler,
                &group_id_for_handler,
                &received.message.plaintext,
                pin_for_handler.as_deref(),
            )
            .await
            {
                eprintln!("fake-phone: desktop reply failed: {error:#}");
            }
        }
    });

    // Phone subscribes for the reply stream BEFORE sending so it doesn't miss
    // anything.
    let mut phone_messages = runtime
        .subscribe_messages(
            &phone_id,
            AppMessageQuery {
                group_id_hex: Some(group_id_hex.clone()),
                limit: None,
            },
        )
        .map_err(|err| anyhow::anyhow!("phone subscribe_messages: {err}"))?;
    let mut phone_events = runtime.subscribe();

    runtime
        .send_message(&phone_id, &group_id, options.message.as_bytes().to_vec())
        .await
        .map_err(|err| anyhow::anyhow!("phone send_message: {err}"))?;

    let mut replies: Vec<String> = Vec::new();
    let mut saw_start = false;
    let mut saw_finalize = false;
    let deadline = Instant::now() + options.timeout;

    while Instant::now() < deadline {
        let satisfied = replies.len() >= options.min_replies.max(1)
            && options
                .expect
                .iter()
                .all(|needle| replies.iter().any(|reply| reply.contains(needle)))
            && (!options.require_job_final || saw_finalize);
        if satisfied {
            break;
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let tick = std::cmp::min(remaining, Duration::from_millis(250));
        tokio::select! {
            _ = tokio::time::sleep(tick) => {}
            update = phone_messages.recv() => {
                match update {
                    Some(RuntimeMessageUpdate::Message(message))
                        if message.message.sender == desktop_id =>
                    {
                        replies.push(message.message.plaintext);
                    }
                    Some(_) => {}
                    None => break,
                }
            }
            event = phone_events.recv() => {
                match event {
                    Ok(MarmotAppEvent::AgentStreamStarted(stream))
                        if stream.account_id_hex == phone_id =>
                    {
                        saw_start = true;
                    }
                    Ok(MarmotAppEvent::AgentStreamFinalized(stream))
                        if stream.account_id_hex == phone_id =>
                    {
                        saw_finalize = true;
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    Err(_) => {}
                }
            }
        }
    }

    desktop_task.abort();
    runtime.shutdown().await;
    let _ = saw_start;

    let matched: Vec<String> = options
        .expect
        .iter()
        .filter(|needle| replies.iter().any(|reply| reply.contains(needle.as_str())))
        .cloned()
        .collect();

    Ok(FakePhoneResult {
        phone_npub,
        group_id: group_id_hex,
        replies,
        matched,
        saw_job_final: saw_finalize,
    })
}

async fn handle_desktop_message(
    runtime: &MarmotAppRuntime,
    account_id_hex: &str,
    group_id: &cgka_traits::GroupId,
    text: &str,
    pin: Option<&str>,
) -> Result<()> {
    let stream_id = stream_id_for_message(text);
    let started_at = current_unix_seconds();

    // The brokered-QUIC route requires at least one candidate even if the
    // harness never opens a real QUIC channel — the start/finish envelopes
    // alone exercise the protocol-layer wiring.
    let quic_candidates = vec!["quic://127.0.0.1:0".to_string()];
    let (_envelope, _summary) = runtime
        .start_agent_text_stream(
            account_id_hex,
            group_id,
            &stream_id,
            started_at,
            quic_candidates,
        )
        .await
        .map_err(|err| anyhow::anyhow!("start_agent_text_stream: {err}"))?;

    let reply = render_fake_desktop_reply(text, pin);
    runtime
        .send_message(account_id_hex, group_id, reply.as_bytes().to_vec())
        .await
        .map_err(|err| anyhow::anyhow!("desktop reply send_message: {err}"))?;

    let mut hasher = Sha256::new();
    hasher.update(reply.as_bytes());
    let transcript_hash: [u8; 32] = hasher.finalize().into();

    let finish_request = AgentTextStreamFinishRequest {
        stream_id: stream_id.to_vec(),
        final_text_or_reference: reply,
        transcript_hash,
        chunk_count: 1,
        finished_at: current_unix_seconds(),
    };
    runtime
        .finish_agent_text_stream(account_id_hex, group_id, finish_request)
        .await
        .map_err(|err| anyhow::anyhow!("finish_agent_text_stream: {err}"))?;
    Ok(())
}

fn render_fake_desktop_reply(prompt: &str, pin: Option<&str>) -> String {
    let trimmed = prompt.trim();
    if let Some(pin) = pin
        && trimmed == pin
    {
        return format!("paired (PIN {pin})");
    }
    if let Some(rest) = trimmed.strip_prefix("/help") {
        let _ = rest;
        return "agentnoise (fake-phone) commands: /help /status /codex <prompt>".to_string();
    }
    if let Some(rest) = trimmed.strip_prefix("/codex") {
        let prompt = rest.trim();
        return format!("codex queued: {prompt}\ncompleted in 0s (synthetic)");
    }
    format!("agentnoise (fake-phone) received: {trimmed}")
}

fn stream_id_for_message(message: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"agentnoise.fake-phone.stream:");
    hasher.update(message.as_bytes());
    hasher.finalize().into()
}

fn current_unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn npub_from_account_id(account_id_hex: &str) -> Result<String> {
    use nostr::PublicKey;
    use nostr::nips::nip19::ToBech32;
    let pk = PublicKey::from_hex(account_id_hex).context("decoding account id hex")?;
    pk.to_bech32().context("encoding npub bech32")
}

async fn wait_for_event<F>(
    events: &mut tokio::sync::broadcast::Receiver<MarmotAppEvent>,
    timeout: Duration,
    mut matches_event: F,
) -> Result<MarmotAppEvent>
where
    F: FnMut(&MarmotAppEvent) -> bool + Send,
{
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            anyhow::bail!("timed out waiting for event");
        }
        let received = tokio::time::timeout(remaining, events.recv())
            .await
            .map_err(|_| anyhow::anyhow!("timed out waiting for event"))?;
        match received {
            Ok(event) => {
                if matches_event(&event) {
                    return Ok(event);
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                anyhow::bail!("event broadcast closed before match");
            }
        }
    }
}
