use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::{Config, WhitenoiseConfig};
use crate::identity::{self, DEFAULT_IDENTITY_NAME, PairingPayload, PublicIdentity};
use crate::whitenoise_cli;

pub const DEFAULT_GROUP_NAME: &str = "agentnoise";
const PROFILE_NAME: &str = "agentnoise";
const PROFILE_DISPLAY_NAME: &str = "agentnoise desktop";
const PROFILE_ABOUT: &str = "Local agentnoise desktop helper.";

#[derive(Debug, Clone)]
pub struct SetupOptions {
    pub phone_npub: Option<String>,
    pub group_name: String,
    pub force_identity: bool,
    pub relays: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SetupResult {
    pub config_path: PathBuf,
    pub created_config: bool,
    pub identity_created: bool,
    pub npub: String,
    pub nprofile: String,
    pub relays: Vec<String>,
    pub qr: String,
    pub daemon_started: bool,
    pub login_repaired: bool,
    pub profile_published: bool,
    pub key_package_published: bool,
    pub group_id: Option<String>,
    pub group_output: Option<String>,
}

pub fn setup(config_path: &Path, options: SetupOptions) -> Result<SetupResult> {
    let created_config = !config_path.exists();
    let mut config = if created_config {
        Config::template()
    } else {
        Config::load(config_path)?
    };

    config.whitenoise.use_keychain_nsec = true;
    let resolved_wn = whitenoise_cli::resolve_wn(&config.whitenoise.wn_bin);
    if resolved_wn.is_file() {
        config.whitenoise.wn_bin = resolved_wn.display().to_string();
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

    let payload =
        identity::pairing_payload(&config.whitenoise, DEFAULT_IDENTITY_NAME, &options.relays)?;
    let qr = identity::render_qr(&payload.nprofile)?;

    let mut group_id = None;
    let mut group_output = None;

    let daemon = whitenoise_cli::ensure_daemon(&config.whitenoise)?;
    let daemon_started = daemon.is_some();
    let login_repaired = whitenoise_cli::ensure_login_from_keychain(&config.whitenoise)?;
    whitenoise_cli::update_profile(
        &config.whitenoise,
        PROFILE_NAME,
        PROFILE_DISPLAY_NAME,
        PROFILE_ABOUT,
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
        relays: payload.relays,
        qr,
        daemon_started,
        login_repaired,
        profile_published,
        key_package_published,
        group_id,
        group_output,
    })
}

pub fn pairing(config_path: &Path, relays: &[String]) -> Result<PairingPayload> {
    let config = Config::load_or_template(config_path)?;
    identity::pairing_payload(&config.whitenoise, DEFAULT_IDENTITY_NAME, relays)
}

fn load_or_create_identity(
    config: &WhitenoiseConfig,
    force: bool,
) -> Result<(PublicIdentity, bool)> {
    if force {
        let identity = identity::create_identity(config, DEFAULT_IDENTITY_NAME, true)
            .context("creating agentnoise desktop identity in OS keychain")?;
        return Ok((identity, true));
    }

    match identity::load_public_identity(config, DEFAULT_IDENTITY_NAME) {
        Ok(identity) => Ok((identity, false)),
        Err(_) => {
            let identity = identity::create_identity(config, DEFAULT_IDENTITY_NAME, false)
                .context("creating agentnoise desktop identity in OS keychain")?;
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
