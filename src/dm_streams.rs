//! Agent text stream lifecycle — replaces today's "chunk codex/claude stdout
//! into multiple chat messages" pattern with darkmatter's purpose-built QUIC
//! live-preview channel (`marmot.group.agent-text-stream.quic.v1`).
//!
//! Usage shape:
//! 1. Job starts → call [`AgentTextStream::start`] to publish the start
//!    envelope and bind a stream id to the running job.
//! 2. Job progress → push text deltas to the configured brokered-QUIC endpoint.
//! 3. Job exit → call [`AgentTextStream::finish`] with the transcript hash
//!    and final chunk count.

use std::net::{SocketAddr, ToSocketAddrs};
use std::str;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use cgka_traits::agent_text_stream::{
    AGENT_TEXT_STREAM_RECORD_TEXT_DELTA, AgentTextStreamKeyContextV1, AgentTextStreamTranscriptV1,
};
use cgka_traits::{EpochId, GroupId, MemberId, MessageId};
use marmot_app::{AgentTextStreamFinishRequest, SendSummary};
use sha2::{Digest, Sha256};
use transport_quic_broker::{BrokerServerTrust, BrokerTextPublisher, OpenBrokerTextPublisher};
use transport_quic_stream::AgentTextStreamCrypto;

use crate::darkmatter_app::DarkmatterEngine;

const DEFAULT_STREAM_CHUNK_BYTES: usize = 4096;

/// Wall-clock seconds since UNIX epoch, used for stream `started_at` /
/// `finished_at`. Clamps to 0 on the (impossible) negative-skew case.
pub fn current_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub struct AgentTextStream {
    engine: DarkmatterEngine,
    account_id_hex: String,
    group_id: GroupId,
    job_id: String,
    stream_id: [u8; 32],
    stream_id_hex: String,
    start_event_id_hex: String,
    broker_candidate: String,
    broker_addr: SocketAddr,
    started_at: u64,
    publisher: Option<BrokerTextPublisher>,
    transcript: AgentTextStreamTranscriptV1,
    next_seq: u64,
    max_chunk_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamAppendReport {
    pub input_bytes: usize,
    pub chunks_published: u64,
    pub transcript_chunks: u64,
    pub transcript_hash_hex: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamFinishReport {
    pub summary: SendSummary,
    pub broker_finished: bool,
    pub broker_chunks: Option<u64>,
    pub broker_hash_hex: Option<String>,
    pub transcript_chunks: u64,
    pub transcript_hash_hex: String,
}

impl AgentTextStream {
    /// Start a new agent text stream for `job_id`. The 32-byte stream id is
    /// derived from `job_id` via SHA-256 so subsequent finish/cancel calls can
    /// reconstruct it without storing a side table.
    pub async fn start(
        engine: DarkmatterEngine,
        account_id_hex: String,
        group_id_hex: &str,
        job_id: &str,
        started_at: u64,
        broker_endpoint: &str,
    ) -> Result<(Self, SendSummary)> {
        let group_bytes = hex::decode(group_id_hex).context("decoding darkmatter group id hex")?;
        let group_id = GroupId::new(group_bytes);
        let stream_id = stream_id_for_job(job_id);
        let stream_id_hex = hex::encode(stream_id);
        let broker = StreamBrokerEndpoint::parse(broker_endpoint)?;
        let (_envelope, summary) = engine
            .runtime()
            .start_agent_text_stream(
                &account_id_hex,
                &group_id,
                &stream_id,
                started_at,
                vec![broker.quic_candidate.clone()],
            )
            .await
            .map_err(|err| anyhow::anyhow!("start_agent_text_stream: {err}"))?;
        let (start_event_id, start_event_id_hex) = start_event_id_from_summary(&summary)?;
        tracing::debug!(
            job_id,
            group_id = group_id_hex,
            stream_id = %stream_id_hex,
            start_event_id = %start_event_id_hex,
            broker = %broker.quic_candidate,
            started_at,
            published = summary.published,
            start_message_ids = %summary.message_ids.join(","),
            "agentnoise stream start"
        );
        let crypto = stream_crypto(
            &engine,
            &account_id_hex,
            &group_id,
            &stream_id,
            &start_event_id,
        )
        .await?;
        let (publisher, broker_addr) = broker
            .connect_publisher(stream_id.to_vec(), start_event_id.clone(), Some(crypto))
            .await
            .map_err(|err| anyhow::anyhow!("connect agent text stream broker: {err}"))?;
        tracing::debug!(
            job_id,
            stream_id = %stream_id_hex,
            start_event_id = %start_event_id_hex,
            broker = %broker.quic_candidate,
            addr = %broker_addr,
            server_name = %broker.server_name,
            "agentnoise stream broker connected"
        );
        let transcript =
            AgentTextStreamTranscriptV1::new(stream_id.to_vec(), start_event_id.clone());
        Ok((
            Self {
                engine,
                account_id_hex,
                group_id,
                job_id: job_id.to_string(),
                stream_id,
                stream_id_hex,
                start_event_id_hex,
                broker_candidate: broker.quic_candidate,
                broker_addr,
                started_at,
                publisher: Some(publisher),
                transcript,
                next_seq: 1,
                max_chunk_bytes: DEFAULT_STREAM_CHUNK_BYTES,
            },
            summary,
        ))
    }

    /// Send a text delta to the brokered-QUIC preview stream and update the
    /// local transcript hash with the exact record sequence.
    pub fn append_text_blocking(
        &mut self,
        text: &str,
        handle: &tokio::runtime::Handle,
    ) -> Result<StreamAppendReport> {
        let input_bytes = text.len();
        if text.is_empty() {
            return Ok(self.append_report(input_bytes, 0));
        }
        if self.publisher.is_none() {
            bail!("agent text stream publisher is closed");
        }
        let chunks = transport_quic_stream::split_text_deltas(text, self.max_chunk_bytes);
        let mut chunks_published = 0_u64;
        for chunk in chunks {
            let chunk_text =
                str::from_utf8(&chunk).context("agent text stream chunk was not valid utf-8")?;
            let publisher = self
                .publisher
                .as_mut()
                .context("agent text stream publisher is closed")?;
            match handle.block_on(publisher.append_text(
                chunk_text,
                self.max_chunk_bytes,
                Duration::ZERO,
            )) {
                Ok(appended) => chunks_published = chunks_published.saturating_add(appended),
                Err(err) => {
                    self.publisher = None;
                    tracing::debug!(
                        job_id = %self.job_id,
                        stream_id = %self.stream_id_hex,
                        start_event_id = %self.start_event_id_hex,
                        broker = %self.broker_candidate,
                        addr = %self.broker_addr,
                        input_bytes,
                        chunks_published,
                        error = %err,
                        "agentnoise stream append failed"
                    );
                    return Err(anyhow::anyhow!("publish agent text stream chunk: {err}"));
                }
            }
            self.transcript
                .append(self.next_seq, AGENT_TEXT_STREAM_RECORD_TEXT_DELTA, &chunk);
            self.next_seq = self.next_seq.saturating_add(1);
        }
        let report = self.append_report(input_bytes, chunks_published);
        tracing::debug!(
            job_id = %self.job_id,
            stream_id = %self.stream_id_hex,
            start_event_id = %self.start_event_id_hex,
            broker = %self.broker_candidate,
            addr = %self.broker_addr,
            input_bytes = report.input_bytes,
            chunks_published = report.chunks_published,
            transcript_chunks = report.transcript_chunks,
            transcript_hash = %report.transcript_hash_hex,
            "agentnoise stream append ok"
        );
        Ok(report)
    }

    pub fn live_preview_active(&self) -> bool {
        self.publisher.is_some()
    }

    /// Finalize the stream: publish the final/result envelope, sealing
    /// transcript_hash + chunk_count.
    pub async fn finish(
        mut self,
        final_text_or_reference: String,
        finished_at: u64,
    ) -> Result<StreamFinishReport> {
        let mut broker_finished = false;
        let mut broker_chunks = None;
        let mut broker_hash_hex = None;
        if let Some(publisher) = self.publisher.take() {
            match publisher.finish().await {
                Ok(sent) => {
                    let local_hash = self.transcript.hash();
                    if sent.transcript_hash != local_hash
                        || sent.chunk_count != self.transcript.chunk_count()
                    {
                        tracing::debug!(
                            job_id = %self.job_id,
                            stream_id = %self.stream_id_hex,
                            start_event_id = %self.start_event_id_hex,
                            broker = %self.broker_candidate,
                            addr = %self.broker_addr,
                            broker_chunks = sent.chunk_count,
                            broker_hash = %hex::encode(sent.transcript_hash),
                            transcript_chunks = self.transcript.chunk_count(),
                            transcript_hash = %hex::encode(local_hash),
                            "agentnoise stream broker finish mismatch"
                        );
                        bail!("agent text stream broker transcript did not match local transcript");
                    }
                    broker_finished = true;
                    broker_chunks = Some(sent.chunk_count);
                    broker_hash_hex = Some(hex::encode(sent.transcript_hash));
                    tracing::debug!(
                        job_id = %self.job_id,
                        stream_id = %self.stream_id_hex,
                        start_event_id = %self.start_event_id_hex,
                        broker = %self.broker_candidate,
                        addr = %self.broker_addr,
                        broker_chunks = sent.chunk_count,
                        broker_hash = %broker_hash_hex.as_deref().unwrap_or(""),
                        "agentnoise stream broker finish ok"
                    );
                }
                Err(err) => {
                    tracing::debug!(
                        job_id = %self.job_id,
                        stream_id = %self.stream_id_hex,
                        start_event_id = %self.start_event_id_hex,
                        broker = %self.broker_candidate,
                        addr = %self.broker_addr,
                        error = %err,
                        "agentnoise stream broker finish failed"
                    );
                }
            }
        } else {
            tracing::debug!(
                job_id = %self.job_id,
                stream_id = %self.stream_id_hex,
                start_event_id = %self.start_event_id_hex,
                broker = %self.broker_candidate,
                addr = %self.broker_addr,
                live_preview = false,
                "agentnoise stream broker finish skipped"
            );
        }
        let transcript_hash = self.transcript.hash();
        let transcript_chunks = self.transcript.chunk_count();
        let request = AgentTextStreamFinishRequest {
            stream_id: self.stream_id.to_vec(),
            start_event_id: self.start_event_id_hex.clone(),
            final_text_or_reference,
            transcript_hash,
            chunk_count: transcript_chunks,
            finished_at,
        };
        let (_envelope, summary) = self
            .engine
            .runtime()
            .finish_agent_text_stream(&self.account_id_hex, &self.group_id, request)
            .await
            .map_err(|err| anyhow::anyhow!("finish_agent_text_stream: {err}"))?;
        let transcript_hash_hex = hex::encode(transcript_hash);
        tracing::debug!(
            job_id = %self.job_id,
            stream_id = %self.stream_id_hex,
            start_event_id = %self.start_event_id_hex,
            broker = %self.broker_candidate,
            addr = %self.broker_addr,
            finished_at,
            broker_finished,
            transcript_chunks,
            transcript_hash = %transcript_hash_hex,
            published = summary.published,
            final_message_ids = %summary.message_ids.join(","),
            "agentnoise stream finish durable"
        );
        Ok(StreamFinishReport {
            summary,
            broker_finished,
            broker_chunks,
            broker_hash_hex,
            transcript_chunks,
            transcript_hash_hex,
        })
    }

    pub fn stream_id(&self) -> [u8; 32] {
        self.stream_id
    }

    pub fn started_at(&self) -> u64 {
        self.started_at
    }

    /// Blocking convenience: build a stream via [`Self::start`] using the
    /// provided tokio handle to `block_on` the async call. Convenient for the
    /// sync `thread::spawn` job runner.
    pub fn start_blocking(
        engine: DarkmatterEngine,
        account_id_hex: String,
        group_id_hex: &str,
        job_id: &str,
        started_at: u64,
        broker_endpoint: &str,
        handle: &tokio::runtime::Handle,
    ) -> Result<(Self, SendSummary)> {
        handle.block_on(Self::start(
            engine,
            account_id_hex,
            group_id_hex,
            job_id,
            started_at,
            broker_endpoint,
        ))
    }

    /// Blocking convenience around [`Self::finish`].
    pub fn finish_blocking(
        self,
        final_text_or_reference: String,
        finished_at: u64,
        handle: &tokio::runtime::Handle,
    ) -> Result<StreamFinishReport> {
        handle.block_on(self.finish(final_text_or_reference, finished_at))
    }

    fn append_report(&self, input_bytes: usize, chunks_published: u64) -> StreamAppendReport {
        StreamAppendReport {
            input_bytes,
            chunks_published,
            transcript_chunks: self.transcript.chunk_count(),
            transcript_hash_hex: hex::encode(self.transcript.hash()),
        }
    }
}

fn stream_id_for_job(job_id: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"agentnoise.agent-text-stream.v1:");
    hasher.update(job_id.as_bytes());
    hasher.finalize().into()
}

pub(crate) fn start_event_id_from_summary(summary: &SendSummary) -> Result<(MessageId, String)> {
    let message_id = summary
        .message_ids
        .first()
        .context("agent text stream start did not return a message id")?;
    let bytes = hex::decode(message_id).context("decoding agent text stream start message id")?;
    if bytes.len() != 32 {
        bail!(
            "agent text stream start message id must be 32 bytes, got {}",
            bytes.len()
        );
    }
    Ok((MessageId::new(bytes), message_id.to_string()))
}

async fn stream_crypto(
    engine: &DarkmatterEngine,
    account_id_hex: &str,
    group_id: &GroupId,
    stream_id: &[u8; 32],
    start_event_id: &MessageId,
) -> Result<AgentTextStreamCrypto> {
    let group_state = engine
        .runtime()
        .group_mls_state(account_id_hex, group_id)
        .await
        .map_err(|err| anyhow::anyhow!("darkmatter group_mls_state: {err}"))?;
    let stream_secret = engine
        .runtime()
        .agent_text_stream_exporter_secret(account_id_hex, group_id)
        .await
        .map_err(|err| anyhow::anyhow!("darkmatter agent_text_stream_exporter_secret: {err}"))?;
    let sender_id = MemberId::new(
        hex::decode(account_id_hex).context("decoding darkmatter account id for stream sender")?,
    );
    Ok(AgentTextStreamCrypto::new(
        stream_secret,
        AgentTextStreamKeyContextV1::new(
            group_id.clone(),
            stream_id.to_vec(),
            EpochId(group_state.epoch),
            sender_id,
            start_event_id.clone(),
        ),
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StreamBrokerEndpoint {
    quic_candidate: String,
    addrs: Vec<SocketAddr>,
    server_name: String,
}

impl StreamBrokerEndpoint {
    fn parse(endpoint: &str) -> Result<Self> {
        let endpoint = endpoint.trim();
        if endpoint.is_empty() {
            bail!("agent text stream broker endpoint cannot be empty");
        }
        let without_scheme = endpoint
            .strip_prefix("quic://")
            .or_else(|| endpoint.strip_prefix("https://"))
            .unwrap_or(endpoint);
        let authority = without_scheme.split('/').next().unwrap_or(without_scheme);
        if authority.is_empty() {
            bail!("agent text stream broker endpoint is missing an authority");
        }
        let server_name = candidate_server_name(authority)?;
        let addrs = authority
            .to_socket_addrs()
            .with_context(|| format!("resolving agent text stream broker {authority}"))?;
        let mut addrs = addrs.collect::<Vec<_>>();
        if addrs.is_empty() {
            bail!("agent text stream broker endpoint did not resolve: {authority}");
        }
        prefer_ipv4_addrs(&mut addrs);
        Ok(Self {
            quic_candidate: format!("quic://{authority}"),
            addrs,
            server_name,
        })
    }

    async fn connect_publisher(
        &self,
        stream_id: Vec<u8>,
        start_event_id: MessageId,
        crypto: Option<AgentTextStreamCrypto>,
    ) -> Result<(BrokerTextPublisher, SocketAddr)> {
        let mut errors = Vec::new();
        for addr in &self.addrs {
            match BrokerTextPublisher::connect(OpenBrokerTextPublisher {
                broker_addr: *addr,
                server_name: self.server_name.clone(),
                trust: broker_trust_for_addr(*addr),
                stream_id: stream_id.clone(),
                start_event_id: start_event_id.clone(),
                crypto: crypto.clone(),
            })
            .await
            {
                Ok(publisher) => return Ok((publisher, *addr)),
                Err(error) => errors.push(format!("{addr}: {error}")),
            }
        }
        bail!(
            "all resolved broker addresses failed for {}: {}",
            self.quic_candidate,
            errors.join("; ")
        )
    }
}

fn broker_trust_for_addr(addr: SocketAddr) -> BrokerServerTrust {
    if addr.ip().is_loopback() {
        BrokerServerTrust::InsecureLocal
    } else {
        BrokerServerTrust::Platform
    }
}

fn prefer_ipv4_addrs(addrs: &mut [SocketAddr]) {
    addrs.sort_by_key(|addr| if addr.is_ipv4() { 0 } else { 1 });
}

fn candidate_server_name(authority: &str) -> Result<String> {
    if let Some(rest) = authority.strip_prefix('[') {
        let Some((host, after_host)) = rest.split_once(']') else {
            bail!("invalid agent text stream broker endpoint: {authority}");
        };
        if !after_host.starts_with(':') {
            bail!("agent text stream broker endpoint must include a port: {authority}");
        }
        return Ok(host.to_string());
    }
    authority
        .rsplit_once(':')
        .map(|(host, _port)| host.to_string())
        .filter(|host| !host.is_empty())
        .with_context(|| {
            format!("agent text stream broker endpoint must include a port: {authority}")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_id_is_deterministic_per_job() {
        let a = stream_id_for_job("job-123");
        let b = stream_id_for_job("job-123");
        let c = stream_id_for_job("job-456");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn broker_endpoint_normalizes_https_to_quic_candidate() {
        let endpoint = StreamBrokerEndpoint::parse("https://127.0.0.1:4450").unwrap();

        assert_eq!(endpoint.quic_candidate, "quic://127.0.0.1:4450");
        assert_eq!(endpoint.server_name, "127.0.0.1");
        assert_eq!(endpoint.addrs, vec!["127.0.0.1:4450".parse().unwrap()]);
        assert!(matches!(
            broker_trust_for_addr(endpoint.addrs[0]),
            BrokerServerTrust::InsecureLocal
        ));
    }

    #[test]
    fn broker_endpoint_keeps_quic_candidate_shape() {
        let endpoint = StreamBrokerEndpoint::parse("quic://[::1]:4450").unwrap();

        assert_eq!(endpoint.quic_candidate, "quic://[::1]:4450");
        assert_eq!(endpoint.server_name, "::1");
        assert_eq!(endpoint.addrs, vec!["[::1]:4450".parse().unwrap()]);
    }

    #[test]
    fn broker_endpoint_prefers_ipv4_before_ipv6() {
        let mut addrs = vec![
            "[::1]:4450".parse().unwrap(),
            "127.0.0.1:4450".parse().unwrap(),
        ];

        prefer_ipv4_addrs(&mut addrs);

        assert_eq!(addrs[0], "127.0.0.1:4450".parse().unwrap());
    }

    #[test]
    fn start_event_id_from_summary_returns_message_id_and_hex() {
        let message_id = "11".repeat(32);
        let summary = SendSummary {
            published: 1,
            message_ids: vec![message_id.clone()],
        };

        let (parsed, parsed_hex) = start_event_id_from_summary(&summary).unwrap();

        assert_eq!(parsed.as_slice(), vec![0x11; 32].as_slice());
        assert_eq!(parsed_hex, message_id);
    }
}
