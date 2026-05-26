use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use nostr::PublicKey;
use nostr::nips::nip19::ToBech32;

use crate::config::{Config, DarkmatterConfig, RunnerLauncher};
use crate::darkmatter_app::DarkmatterEngine;
use crate::identity::{self, DEFAULT_IDENTITY_NAME, PairingPayload};

pub const DEFAULT_GROUP_NAME: &str = "agentnoise";

#[derive(Debug, Clone)]
pub struct SetupOptions {
    pub phone_npub: Option<String>,
    pub profile_name: Option<String>,
    pub group_name: String,
    pub force_identity: bool,
    pub relays: Vec<String>,
    pub direct_agents: bool,
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
}

/// Bootstrap agentnoise: write config, start the embedded Marmot v2 engine
/// long enough to create (or look up) the managed desktop account, then save
/// the resulting npub to config and render the pairing QR.
///
/// The engine's `KeychainSecretStore`-backed `AccountHome` owns the secret;
/// agentnoise itself never sees the nsec.
pub fn setup(config_path: &Path, options: SetupOptions) -> Result<SetupResult> {
    let created_config = !config_path.exists();
    let mut config = if created_config {
        Config::template_for_path(config_path)
    } else {
        Config::load(config_path)?
    };

    if options.direct_agents {
        config.runner.launcher = RunnerLauncher::Direct;
    }
    if let Some(name) = options
        .profile_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        config.darkmatter.profile_name = normalize_profile_name(name);
        config.darkmatter.profile_display_name = name.to_string();
    }
    if !options.relays.is_empty() {
        config.darkmatter.message_relays = options.relays.clone();
    }
    if created_config {
        config.save(config_path)?;
    }

    ensure_runtime_dirs(&config)?;

    // Open the engine just long enough to ensure the managed account exists.
    let dm_home = config.resolved_data_dir().join("darkmatter");
    let bootstrap_relays = config.darkmatter.message_relays.clone();
    let keychain_service =
        crate::darkmatter_app::keychain_service_for_instance(config.instance.as_deref());
    let (npub, identity_created) = ensure_engine_identity(
        dm_home,
        bootstrap_relays,
        &keychain_service,
        options.force_identity,
        config.darkmatter.account.clone(),
        config.darkmatter.clone(),
    )?;

    config.darkmatter.account = Some(npub.clone());
    config.darkmatter.bot_npub = Some(npub.clone());
    config.save(config_path)?;

    let payload = identity::pairing_payload_from_npub(
        &config.darkmatter,
        DEFAULT_IDENTITY_NAME,
        &npub,
        &options.relays,
    )?;
    let qr = identity::render_qr(&npub)?;

    if options.phone_npub.is_some() {
        // Phone-initiated group creation: under v2 the phone creates the
        // group and the desktop discovers it via MarmotAppEvent::GroupJoined
        // once `agentnoise listen` is running.
        eprintln!(
            "agentnoise: phone_npub provided; under Marmot v2 the phone-side client \
             creates the control group. Start `agentnoise listen` and have the phone scan the QR."
        );
    }
    let _ = options.group_name;

    Ok(SetupResult {
        config_path: config_path.to_path_buf(),
        created_config,
        identity_created,
        npub,
        nprofile: payload.nprofile,
        profile_name: config.darkmatter.profile_name,
        profile_display_name: config.darkmatter.profile_display_name,
        relays: payload.relays,
        qr,
    })
}

/// Boot the engine, ensure the managed account exists, return the npub.
/// `identity_created` is true iff the engine had to mint a new keypair (i.e.
/// the previously-configured account reference resolved to nothing on entry).
fn ensure_engine_identity(
    dm_home: PathBuf,
    bootstrap_relays: Vec<String>,
    keychain_service: &str,
    _force: bool,
    previous_npub: Option<String>,
    profile_config: DarkmatterConfig,
) -> Result<(String, bool)> {
    if bootstrap_relays.is_empty() {
        anyhow::bail!(
            "darkmatter.message_relays is empty; set at least one relay before running setup"
        );
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building tokio runtime for setup")?;
    runtime.block_on(async {
        let engine = DarkmatterEngine::open(dm_home, bootstrap_relays.clone(), keychain_service)?;
        engine.start().await?;
        let configured = previous_npub.as_deref();
        let existed = match configured.map(str::trim).filter(|r| !r.is_empty()) {
            Some(reference) => engine.find_account(reference)?.is_some(),
            None => false,
        };
        let account_id_hex = engine.ensure_account(configured, &bootstrap_relays).await?;
        match tokio::time::timeout(
            Duration::from_secs(30),
            engine.publish_discovery(&account_id_hex, &profile_config),
        )
        .await
        {
            Ok(Ok(())) => {
                eprintln!("agentnoise: darkmatter discovery broadcast complete");
            }
            Ok(Err(error)) => {
                eprintln!("agentnoise: darkmatter discovery broadcast failed: {error:#}");
            }
            Err(_) => {
                eprintln!("agentnoise: darkmatter discovery broadcast timed out; continuing");
            }
        }
        engine.shutdown().await;
        let pk = PublicKey::from_hex(&account_id_hex).context("decoding account_id_hex")?;
        let npub = pk.to_bech32().context("encoding npub bech32")?;
        Ok::<_, anyhow::Error>((npub, !existed))
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
    if let Some(npub) = config.darkmatter.account.as_deref() {
        return identity::pairing_payload_from_npub(
            &config.darkmatter,
            DEFAULT_IDENTITY_NAME,
            npub,
            relays,
        );
    }
    anyhow::bail!(
        "no desktop identity in config; run `agentnoise setup` or `agentnoise listen` once to create one"
    )
}

fn ensure_runtime_dirs(config: &Config) -> Result<()> {
    let data_dir = config.resolved_data_dir();
    fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating data dir {}", data_dir.display()))?;

    let log_dir = config.resolved_log_dir();
    fs::create_dir_all(&log_dir)
        .with_context(|| format!("creating log dir {}", log_dir.display()))?;

    ensure_generated_sandbox_repo_dir(config)
}

fn ensure_generated_sandbox_repo_dir(config: &Config) -> Result<()> {
    if config.instance.is_none() {
        return Ok(());
    }

    let Some(instance_root) = config.resolved_data_dir().parent().map(Path::to_path_buf) else {
        return Ok(());
    };
    let expected = instance_root.join("sandbox");
    let Some(repo) = config.repos.iter().find(|repo| repo.alias == "sandbox") else {
        return Ok(());
    };
    if config.repo_path(&repo.alias).as_deref() != Some(expected.as_path()) {
        return Ok(());
    }

    fs::create_dir_all(&expected)
        .with_context(|| format!("creating sandbox repo dir {}", expected.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn ensure_runtime_dirs_creates_named_instance_default_sandbox_repo() {
        let temp = tempfile::tempdir().unwrap();
        let instance_root = temp.path().join("instances/dev");
        let mut config = Config::template();
        config.instance = Some("dev".to_string());
        config.runner.data_dir = instance_root.join("data").display().to_string();
        config.runner.log_dir = instance_root.join("logs").display().to_string();
        config.repos = vec![crate::config::RepoConfig {
            alias: "sandbox".to_string(),
            path: instance_root.join("sandbox").display().to_string(),
        }];

        ensure_runtime_dirs(&config).unwrap();

        assert!(instance_root.join("sandbox").is_dir());
    }
}
