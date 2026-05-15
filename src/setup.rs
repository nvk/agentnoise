use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::{Config, WhitenoiseConfig};
use crate::identity::{self, DEFAULT_IDENTITY_NAME, PairingPayload, PublicIdentity};
use crate::paths::expand_tilde;
use crate::whitenoise_cli;

pub const DEFAULT_GROUP_NAME: &str = "agentnoise";

#[derive(Debug, Clone)]
pub struct SetupOptions {
    pub phone_npub: Option<String>,
    pub profile_name: Option<String>,
    pub group_name: String,
    pub force_identity: bool,
    pub relays: Vec<String>,
    pub dev_burner_nsec: bool,
    pub start_daemon: bool,
}

#[derive(Debug, Clone)]
pub struct SetupResult {
    pub config_path: PathBuf,
    pub created_config: bool,
    pub identity_created: bool,
    pub npub: String,
    pub nprofile: String,
    pub profile_name: String,
    pub profile_display_name: String,
    pub relays: Vec<String>,
    pub qr: String,
    pub daemon_started: bool,
    pub login_repaired: bool,
    pub profile_published: bool,
    pub key_package_published: bool,
    pub message_relay_entries_added: usize,
    pub group_id: Option<String>,
    pub group_output: Option<String>,
    pub dev_burner_nsec_file: Option<PathBuf>,
}

pub fn setup(config_path: &Path, options: SetupOptions) -> Result<SetupResult> {
    let created_config = !config_path.exists();
    let mut config = if created_config {
        Config::template()
    } else {
        Config::load(config_path)?
    };

    if options.dev_burner_nsec {
        config.whitenoise.dev_burner_nsec = true;
        config.whitenoise.dev_burner_nsec_file = Some(
            config
                .resolved_data_dir()
                .join("dev-burner.nsec")
                .display()
                .to_string(),
        );
        config.whitenoise.use_keychain_nsec = false;
    } else if !config.whitenoise.dev_burner_nsec {
        config.whitenoise.use_keychain_nsec = true;
    }
    if let Some(name) = options
        .profile_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        config.whitenoise.profile_name = normalize_profile_name(name);
        config.whitenoise.profile_display_name = name.to_string();
    }
    if whitenoise_cli::should_reset_wn_bin_to_default(&config.whitenoise.wn_bin) {
        config.whitenoise.wn_bin = "wn".to_string();
    }
    if created_config {
        config.save(config_path)?;
    }

    let (identity, identity_created) =
        load_or_create_identity(&config.whitenoise, options.force_identity)?;
    config.whitenoise.account = Some(identity.npub.clone());
    config.whitenoise.bot_npub = Some(identity.npub.clone());
    ensure_runtime_dirs(&config)?;
    config.save(config_path)?;

    let payload = identity::pairing_payload_from_npub(
        &config.whitenoise,
        DEFAULT_IDENTITY_NAME,
        &identity.npub,
        &options.relays,
    )?;
    let qr = identity::render_qr(&payload.nprofile)?;

    let mut group_id = None;
    let mut group_output = None;

    let daemon_started = if options.start_daemon {
        whitenoise_cli::ensure_daemon(&config.whitenoise)?.is_some()
    } else {
        false
    };
    let login_repaired = whitenoise_cli::ensure_login_from_configured_nsec(&config.whitenoise)?;
    let message_relays = whitenoise_cli::ensure_message_relays(&config.whitenoise)?;
    whitenoise_cli::update_profile(
        &config.whitenoise,
        &config.whitenoise.profile_name,
        &config.whitenoise.profile_display_name,
        &config.whitenoise.profile_about,
    )?;
    let profile_published = true;
    whitenoise_cli::publish_key_package(&config.whitenoise)?;
    let key_package_published = true;

    if let Some(phone_npub) = options
        .phone_npub
        .as_deref()
        .map(str::trim)
        .filter(|phone_npub| !phone_npub.is_empty())
    {
        let created = whitenoise_cli::create_group(
            &config.whitenoise,
            &options.group_name,
            &[phone_npub.to_string()],
        )?;
        group_output = Some(created.output);
        if let Some(id) = created.group_id {
            config.whitenoise.add_control_group_id(&id);
            config.save(config_path)?;
            group_id = Some(id);
        }
    }

    Ok(SetupResult {
        config_path: config_path.to_path_buf(),
        created_config,
        identity_created,
        npub: identity.npub,
        nprofile: payload.nprofile,
        profile_name: config.whitenoise.profile_name,
        profile_display_name: config.whitenoise.profile_display_name,
        relays: payload.relays,
        qr,
        daemon_started,
        login_repaired,
        profile_published,
        key_package_published,
        message_relay_entries_added: message_relays.added_entries,
        group_id,
        group_output,
        dev_burner_nsec_file: config
            .whitenoise
            .dev_burner_nsec_file
            .as_deref()
            .map(expand_tilde),
    })
}

pub fn normalize_profile_name(name: &str) -> String {
    let mut output = String::new();
    let mut last_dash = false;
    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            output.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            output.push('-');
            last_dash = true;
        }
    }
    let output = output.trim_matches('-').to_string();
    if output.is_empty() {
        "agentnoise".to_string()
    } else {
        output
    }
}

pub fn pairing(config_path: &Path, relays: &[String]) -> Result<PairingPayload> {
    let config = Config::load_or_template(config_path)?;
    if let Some(identity) =
        identity::configured_public_identity(&config.whitenoise, DEFAULT_IDENTITY_NAME)?
    {
        return identity::pairing_payload_from_npub(
            &config.whitenoise,
            DEFAULT_IDENTITY_NAME,
            &identity.npub,
            relays,
        );
    }

    identity::pairing_payload(&config.whitenoise, DEFAULT_IDENTITY_NAME, relays)
}

fn load_or_create_identity(
    config: &WhitenoiseConfig,
    force: bool,
) -> Result<(PublicIdentity, bool)> {
    if force {
        let identity = identity::create_identity(config, DEFAULT_IDENTITY_NAME, true)
            .context("creating agentnoise desktop identity in configured identity store")?;
        return Ok((identity, true));
    }

    if let Some(identity) = identity::configured_public_identity(config, DEFAULT_IDENTITY_NAME)? {
        return Ok((identity, false));
    }

    match identity::load_public_identity(config, DEFAULT_IDENTITY_NAME) {
        Ok(identity) => Ok((identity, false)),
        Err(_) => {
            let identity = identity::create_identity(config, DEFAULT_IDENTITY_NAME, false)
                .context("creating agentnoise desktop identity in configured identity store")?;
            Ok((identity, true))
        }
    }
}

fn ensure_runtime_dirs(config: &Config) -> Result<()> {
    let data_dir = config.resolved_data_dir();
    fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating data dir {}", data_dir.display()))?;

    let log_dir = config.resolved_log_dir();
    fs::create_dir_all(&log_dir).with_context(|| format!("creating log dir {}", log_dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::Keys;
    use nostr::nips::nip19::ToBech32;

    #[test]
    fn normalizes_profile_name_for_nostr_name_field() {
        assert_eq!(
            normalize_profile_name("agentnoise MBP M5"),
            "agentnoise-mbp-m5"
        );
        assert_eq!(normalize_profile_name(" linux_box "), "linux-box");
        assert_eq!(normalize_profile_name("!!!"), "agentnoise");
    }

    #[test]
    fn load_or_create_identity_prefers_cached_public_config() {
        let keys = Keys::generate();
        let npub = keys.public_key().to_bech32().unwrap();
        let mut config = Config::template().whitenoise;
        config.account = Some(npub.clone());
        config.use_keychain_nsec = true;

        let (identity, created) = load_or_create_identity(&config, false).unwrap();

        assert!(!created);
        assert_eq!(identity.npub, npub);
    }
}
