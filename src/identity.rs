use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use nostr::nips::nip19::{Nip19Profile, ToBech32};
use nostr::{Keys, PublicKey, RelayUrl};
use qrcode::QrCode;
use qrcode::render::unicode;
use serde::Serialize;
use zeroize::Zeroize;

use crate::config::WhitenoiseConfig;
use crate::paths::expand_tilde;
use crate::secrets::{self, SecretStore};

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
    if !force && identity_nsec_present(config, &name)? {
        bail!(
            "identity {name} already exists in {}; use --force to replace it",
            identity_secret_label(config, &name)
        );
    }

    let keys = Keys::generate();
    let npub = keys.public_key().to_bech32().expect("npub bech32");
    let mut nsec = keys.secret_key().to_bech32().expect("nsec bech32");
    store_identity_nsec(config, &name, &nsec)?;
    nsec.zeroize();

    Ok(PublicIdentity {
        keychain_item: keychain_item_for_identity(&config.keychain_item, &name),
        name,
        npub,
    })
}

pub fn load_public_identity(config: &WhitenoiseConfig, name: &str) -> Result<PublicIdentity> {
    let name = normalize_identity_name(name)?;
    let mut nsec = load_identity_nsec(config, &name)?;
    let keys = Keys::parse(&nsec).context("parsing identity nsec")?;
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
    let mut nsec = load_identity_nsec(config, &name)?;
    let keys = Keys::parse(&nsec).context("parsing identity nsec")?;
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

pub fn identity_secret_label(config: &WhitenoiseConfig, name: &str) -> String {
    if config.dev_burner_nsec {
        return match dev_burner_nsec_path(config, name) {
            Some(path) => format!("dev burner file {}", path.display()),
            None => "dev burner file <missing path>".to_string(),
        };
    }

    identity_store(config, name).label()
}

pub fn load_default_nsec(config: &WhitenoiseConfig) -> Result<String> {
    load_identity_nsec(config, DEFAULT_IDENTITY_NAME)
}

pub fn delete_identity_nsec(config: &WhitenoiseConfig, name: &str) -> Result<String> {
    let name = normalize_identity_name(name)?;

    if config.dev_burner_nsec {
        let path = require_dev_burner_nsec_path(config, &name)?;
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("deleting dev burner nsec file {}", path.display()));
            }
        }
        return Ok(format!("dev burner file {}", path.display()));
    }

    let store = identity_store(config, &name);
    store.delete_nsec()?;
    Ok(store.label())
}

pub fn identity_store(config: &WhitenoiseConfig, name: &str) -> SecretStore {
    SecretStore::new(
        &config.keychain_service,
        keychain_item_for_identity(&config.keychain_item, name),
    )
}

pub fn dev_burner_nsec_path(config: &WhitenoiseConfig, name: &str) -> Option<PathBuf> {
    if !config.dev_burner_nsec {
        return None;
    }
    let base = config
        .dev_burner_nsec_file
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())?;
    let base = expand_tilde(base);
    if name == DEFAULT_IDENTITY_NAME {
        return Some(base);
    }

    let file_name = base
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("dev-burner.nsec");
    let sibling = base.with_file_name(format!("{file_name}.{name}"));
    Some(sibling)
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

fn identity_nsec_present(config: &WhitenoiseConfig, name: &str) -> Result<bool> {
    if config.dev_burner_nsec {
        let path = require_dev_burner_nsec_path(config, name)?;
        return match fs::read_to_string(&path) {
            Ok(secret) => Ok(secrets::validate_nsec(secret.trim()).is_ok()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error)
                .with_context(|| format!("reading dev burner nsec file {}", path.display())),
        };
    }

    identity_store(config, name).nsec_status()
}

fn load_identity_nsec(config: &WhitenoiseConfig, name: &str) -> Result<String> {
    if config.dev_burner_nsec {
        let path = require_dev_burner_nsec_path(config, name)?;
        let secret = fs::read_to_string(&path)
            .with_context(|| format!("reading dev burner nsec file {}", path.display()))?;
        let nsec = secret.trim().to_string();
        secrets::validate_nsec(&nsec)?;
        return Ok(nsec);
    }

    identity_store(config, name).load_nsec()
}

fn store_identity_nsec(config: &WhitenoiseConfig, name: &str, nsec: &str) -> Result<()> {
    if config.dev_burner_nsec {
        let path = require_dev_burner_nsec_path(config, name)?;
        return store_dev_burner_nsec(&path, nsec);
    }

    identity_store(config, name).store_nsec(nsec)
}

fn require_dev_burner_nsec_path(config: &WhitenoiseConfig, name: &str) -> Result<PathBuf> {
    dev_burner_nsec_path(config, name).ok_or_else(|| {
        anyhow!("dev burner nsec enabled but whitenoise.dev_burner_nsec_file is not configured")
    })
}

fn store_dev_burner_nsec(path: &Path, nsec: &str) -> Result<()> {
    secrets::validate_nsec(nsec)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    fs::write(path, format!("{nsec}\n"))
        .with_context(|| format!("writing dev burner nsec file {}", path.display()))?;
    set_secret_file_permissions(path)
}

#[cfg(unix)]
fn set_secret_file_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)
        .with_context(|| format!("reading permissions for {}", path.display()))?
        .permissions();
    permissions.set_mode(0o600);
    fs::set_permissions(path, permissions)
        .with_context(|| format!("setting permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn set_secret_file_permissions(_path: &Path) -> Result<()> {
    Ok(())
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

    #[test]
    fn dev_burner_identity_uses_plaintext_file() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = crate::config::Config::template().whitenoise;
        config.dev_burner_nsec = true;
        config.dev_burner_nsec_file =
            Some(temp.path().join("dev-burner.nsec").display().to_string());

        let created = create_identity(&config, DEFAULT_IDENTITY_NAME, false).unwrap();
        assert!(created.npub.starts_with("npub1"));
        assert!(temp.path().join("dev-burner.nsec").is_file());

        let loaded = load_public_identity(&config, DEFAULT_IDENTITY_NAME).unwrap();
        assert_eq!(loaded.npub, created.npub);

        let label = delete_identity_nsec(&config, DEFAULT_IDENTITY_NAME).unwrap();
        assert!(label.contains("dev burner file"));
        assert!(!temp.path().join("dev-burner.nsec").exists());
    }
}
