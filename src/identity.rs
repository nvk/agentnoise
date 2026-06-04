//! Pairing-payload helpers — npub-only, no nsec.
//!
//! After the v0.2.0 cleanup, agentnoise no longer stores any secret material
//! itself. The embedded Marmot v2 engine owns the secret (in the OS keychain
//! via `marmot_account::KeychainSecretStore`); this module just renders the
//! pairing QR and the nprofile a phone client uses to find the desktop's
//! npub.

use anyhow::{Context, Result};
use nostr::nips::nip19::{FromBech32, Nip19Profile, ToBech32};
use nostr::{PublicKey, RelayUrl};
use qrcode::QrCode;
use qrcode::render::unicode;
use serde::Serialize;

use crate::config::DarkmatterConfig;

pub const DEFAULT_IDENTITY_NAME: &str = "desktop";

// NOTE: only writable, event-accepting relays belong here. Indexer/search
// endpoints (e.g. index.hzrd149.com, indexer.coracle.social) accept WebSocket
// connections but never ACK an EVENT publish, which hangs marmot-app's
// publish-to-all-relays step until it times out — failing (and rolling back)
// the whole account creation. Keep this list to mainstream write relays.
pub const DEFAULT_PAIRING_RELAYS: &[&str] = &[
    "wss://relay.damus.io",
    "wss://relay.primal.net",
    "wss://nos.lol",
];

pub const DEFAULT_MESSAGE_RELAYS: &[&str] = &[
    "wss://relay.damus.io",
    "wss://relay.primal.net",
    "wss://nos.lol",
];

#[derive(Debug, Clone)]
pub struct PublicIdentity {
    pub name: String,
    pub npub: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PairingPayload {
    pub kind: String,
    pub version: u8,
    pub name: String,
    pub npub: String,
    pub nprofile: String,
    pub relays: Vec<String>,
}

pub fn pairing_payload_from_npub(
    config: &DarkmatterConfig,
    name: &str,
    npub: &str,
    relay_overrides: &[String],
) -> Result<PairingPayload> {
    let public_key = PublicKey::from_bech32(npub).context("decoding desktop npub")?;
    let relays = pairing_relays(config, relay_overrides);
    let nprofile = nprofile(public_key, &relays)?;
    Ok(PairingPayload {
        kind: "agentnoise".to_string(),
        version: 1,
        name: name.to_string(),
        npub: npub.to_string(),
        nprofile,
        relays,
    })
}

pub fn pairing_relays(config: &DarkmatterConfig, relay_overrides: &[String]) -> Vec<String> {
    let relays = if relay_overrides.is_empty() {
        if !config.pairing_relays.is_empty() {
            config.pairing_relays.clone()
        } else {
            DEFAULT_PAIRING_RELAYS
                .iter()
                .map(|relay| (*relay).to_string())
                .collect()
        }
    } else {
        relay_overrides.to_vec()
    };

    dedupe_relays(relays)
}

pub fn nprofile(public_key: PublicKey, relays: &[String]) -> Result<String> {
    let relay_urls = relays
        .iter()
        .filter_map(|relay| RelayUrl::parse(relay).ok())
        .collect::<Vec<_>>();
    let profile = Nip19Profile::new(public_key, relay_urls);
    profile.to_bech32().context("encoding nprofile")
}

pub fn render_qr(payload: &str) -> Result<String> {
    let code = QrCode::new(payload).context("building qr code")?;
    Ok(code.render::<unicode::Dense1x2>().quiet_zone(true).build())
}

fn dedupe_relays(relays: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(relays.len());
    for relay in relays {
        let trimmed = relay.trim().to_string();
        if trimmed.is_empty() {
            continue;
        }
        if seen.insert(trimmed.clone()) {
            out.push(trimmed);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::Keys;

    #[test]
    fn pairing_payload_round_trip() {
        let keys = Keys::generate();
        let npub = keys.public_key().to_bech32().unwrap();
        let config = crate::config::Config::template().darkmatter;
        let payload =
            pairing_payload_from_npub(&config, DEFAULT_IDENTITY_NAME, &npub, &[]).unwrap();
        assert_eq!(payload.npub, npub);
        assert!(payload.nprofile.starts_with("nprofile1"));
        assert_eq!(payload.kind, "agentnoise");
        assert_eq!(
            payload.relays,
            vec![
                "wss://relay.damus.io".to_string(),
                "wss://relay.primal.net".to_string(),
                "wss://nos.lol".to_string(),
            ]
        );
    }

    #[test]
    fn dedupe_relays_strips_empties_and_duplicates() {
        let relays = vec![
            "wss://a".to_string(),
            "  ".to_string(),
            "wss://a".to_string(),
            "wss://b".to_string(),
        ];
        assert_eq!(
            dedupe_relays(relays),
            vec!["wss://a".to_string(), "wss://b".to_string()]
        );
    }
}
