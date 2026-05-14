use anyhow::{Context, Result, bail};
use nostr::nips::nip19::{Nip19Profile, ToBech32};
use nostr::{Keys, PublicKey, RelayUrl};
use qrcode::QrCode;
use qrcode::render::unicode;
use serde::Serialize;
use zeroize::Zeroize;

use crate::config::WhitenoiseConfig;
use crate::secrets::SecretStore;

pub const DEFAULT_IDENTITY_NAME: &str = "desktop";

pub const DEFAULT_PAIRING_RELAYS: &[&str] = &[
    "wss://index.hzrd149.com",
    "wss://indexer.coracle.social",
    "wss://relay.primal.net",
    "wss://relay.damus.io",
    "wss://relay.ditto.pub",
    "wss://nos.lol",
];

#[derive(Debug, Clone)]
pub struct PublicIdentity {
    pub name: String,
    pub keychain_item: String,
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

pub fn create_identities(
    config: &WhitenoiseConfig,
    prefix: &str,
    count: usize,
    force: bool,
) -> Result<Vec<PublicIdentity>> {
    if count == 0 {
        bail!("count must be greater than zero");
    }
    if count > 20 {
        bail!("refusing to create more than 20 identities at once");
    }

    let prefix = normalize_identity_name(prefix)?;
    let mut identities = Vec::with_capacity(count);
    for index in 0..count {
        let name = if count == 1 || index == 0 {
            prefix.clone()
        } else {
            format!("{prefix}-{}", index + 1)
        };
        let identity = create_identity(config, &name, force)?;
        identities.push(identity);
    }

    Ok(identities)
}

pub fn create_identity(
    config: &WhitenoiseConfig,
    name: &str,
    force: bool,
) -> Result<PublicIdentity> {
    let name = normalize_identity_name(name)?;
    let store = identity_store(config, &name);
    if !force && store.nsec_status()? {
        bail!("identity {name} already exists in OS keychain; use --force to replace it");
    }

    let keys = Keys::generate();
    let npub = keys.public_key().to_bech32().expect("npub bech32");
    let mut nsec = keys.secret_key().to_bech32().expect("nsec bech32");
    store.store_nsec(&nsec)?;
    nsec.zeroize();

    Ok(PublicIdentity {
        keychain_item: keychain_item_for_identity(&config.keychain_item, &name),
        name,
        npub,
    })
}

pub fn load_public_identity(config: &WhitenoiseConfig, name: &str) -> Result<PublicIdentity> {
    let name = normalize_identity_name(name)?;
    let store = identity_store(config, &name);
    let mut nsec = store.load_nsec()?;
    let keys = Keys::parse(&nsec).context("parsing nsec from OS keychain")?;
    nsec.zeroize();
    let npub = keys.public_key().to_bech32().expect("npub bech32");

    Ok(PublicIdentity {
        keychain_item: keychain_item_for_identity(&config.keychain_item, &name),
        name,
        npub,
    })
}

pub fn pairing_payload(
    config: &WhitenoiseConfig,
    name: &str,
    relay_overrides: &[String],
) -> Result<PairingPayload> {
    let name = normalize_identity_name(name)?;
    let store = identity_store(config, &name);
    let mut nsec = store.load_nsec()?;
    let keys = Keys::parse(&nsec).context("parsing nsec from OS keychain")?;
    nsec.zeroize();

    let relays = pairing_relays(config, relay_overrides);
    let nprofile = nprofile(keys.public_key(), &relays)?;
    let npub = keys.public_key().to_bech32().expect("npub bech32");

    Ok(PairingPayload {
        kind: "agentnoise.identity".to_string(),
        version: 1,
        name,
        npub,
        nprofile,
        relays,
    })
}

pub fn identity_store(config: &WhitenoiseConfig, name: &str) -> SecretStore {
    SecretStore::new(
        &config.keychain_service,
        keychain_item_for_identity(&config.keychain_item, name),
    )
}

pub fn keychain_item_for_identity(base_item: &str, name: &str) -> String {
    if name == DEFAULT_IDENTITY_NAME {
        base_item.to_string()
    } else {
        format!("{base_item}/{name}")
    }
}

pub fn pairing_relays(config: &WhitenoiseConfig, relay_overrides: &[String]) -> Vec<String> {
    let relays = if relay_overrides.is_empty() {
        if !config.pairing_relays.is_empty() {
            config.pairing_relays.clone()
        } else if let Some(relay) = &config.login_relay {
            vec![relay.clone()]
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
        .map(|relay| RelayUrl::parse(relay).with_context(|| format!("invalid relay URL: {relay}")))
        .collect::<Result<Vec<_>>>()?;
    let profile = Nip19Profile::new(public_key, relay_urls);
    profile.to_bech32().context("encoding nprofile")
}

pub fn render_qr(payload: &str) -> Result<String> {
    let code = QrCode::new(payload.as_bytes()).context("building QR code")?;
    Ok(code.render::<unicode::Dense1x2>().quiet_zone(true).build())
}

fn normalize_identity_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        bail!("identity name cannot be empty");
    }
    if name.len() > 64 {
        bail!("identity name cannot be longer than 64 bytes");
    }
    if !name
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        bail!("identity name can only contain ASCII letters, numbers, '-' and '_'");
    }
    Ok(name.to_string())
}

fn dedupe_relays(relays: Vec<String>) -> Vec<String> {
    let mut output = Vec::new();
    for relay in relays {
        let relay = relay.trim();
        if relay.is_empty() {
            continue;
        }
        if !output.iter().any(|existing| existing == relay) {
            output.push(relay.to_string());
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_identity_uses_base_item() {
        assert_eq!(
            keychain_item_for_identity("whitenoise-nsec", DEFAULT_IDENTITY_NAME),
            "whitenoise-nsec"
        );
    }

    #[test]
    fn secondary_identity_uses_named_item() {
        assert_eq!(
            keychain_item_for_identity("whitenoise-nsec", "desktop-2"),
            "whitenoise-nsec/desktop-2"
        );
    }

    #[test]
    fn qr_render_is_nonempty() {
        assert!(!render_qr("nprofile1test").unwrap().trim().is_empty());
    }
}
