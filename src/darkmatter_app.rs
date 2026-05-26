//! Bootstrap and lifecycle for the embedded [`marmot_app::MarmotApp`] /
//! [`marmot_app::MarmotAppRuntime`] used as agentnoise's protocol bridge.
//!
//! agentnoise pins to a single managed account inside a darkmatter home
//! directory, identified by the npub persisted in `config.darkmatter.account`.
//! On startup the engine reuses that account if present, otherwise creates a
//! fresh identity, then starts the runtime workers.

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use cgka_traits::TransportEndpoint;
use marmot_account::AccountHome;
use marmot_app::{
    AccountRelayListBootstrap, ManagedAccount, MarmotApp, MarmotAppRuntime, RelayPlaneHealth,
    UserProfileMetadata,
};
use nostr::PublicKey;
use nostr::nips::nip19::FromBech32;

use crate::config::DarkmatterConfig;

/// Normalize an account reference (npub or hex) to the lowercase
/// `account_id_hex` form marmot-app uses for `ManagedAccount`. A bech32 `npub`
/// is decoded to hex; anything else is passed through trimmed.
fn account_lookup_ref(reference: &str) -> String {
    let reference = reference.trim();
    if reference.starts_with("npub")
        && let Ok(public_key) = PublicKey::from_bech32(reference)
    {
        return public_key.to_hex();
    }
    reference.to_string()
}

/// Base OS-keychain service name used by [`marmot_account::KeychainSecretStore`].
/// All agentnoise managed-account secrets land under this service, keyed by
/// `account_id_hex` per item. Distinct from dm's default service so accounts
/// the user creates with raw dm don't collide with agentnoise's.
pub const KEYCHAIN_SERVICE_NAME: &str = "agentnoise";

/// Per-instance keychain service name. The default (no instance) is
/// `"agentnoise"`; a named instance gets `"agentnoise-<instance>"` so each
/// instance's secrets live under a fully isolated keychain service.
pub fn keychain_service_for_instance(instance: Option<&str>) -> String {
    match instance {
        Some(name) if !name.trim().is_empty() => {
            format!("{KEYCHAIN_SERVICE_NAME}-{}", name.trim())
        }
        _ => KEYCHAIN_SERVICE_NAME.to_string(),
    }
}

/// Owns the long-lived [`MarmotApp`] handle and its [`MarmotAppRuntime`].
///
/// Clone is cheap — both inner types are reference-counted handles.
#[derive(Clone)]
pub struct DarkmatterEngine {
    app: MarmotApp,
    runtime: MarmotAppRuntime,
    account_home: AccountHome,
    home: PathBuf,
}

impl DarkmatterEngine {
    /// Construct the engine from an on-disk home and a non-empty relay list.
    ///
    /// Production opens marmot-app with an OS-keychain-backed `AccountHome`.
    /// Development-only burner mode uses marmot-app's file-backed default so
    /// headless test boxes can run without macOS keychain prompts.
    pub fn open(
        home: PathBuf,
        relays: Vec<String>,
        keychain_service: &str,
        dev_burner_nsec: bool,
    ) -> Result<Self> {
        if relays.is_empty() {
            bail!("darkmatter engine requires at least one relay url");
        }
        let home = if dev_burner_nsec {
            home.join("dev-burner")
        } else {
            home
        };
        fs::create_dir_all(&home)
            .with_context(|| format!("creating darkmatter home {}", home.display()))?;
        let account_home = if dev_burner_nsec {
            AccountHome::open(&home)
        } else {
            AccountHome::open_with_keychain(&home, keychain_service)
                .map_err(|err| anyhow::anyhow!("opening keychain-backed AccountHome: {err}"))?
        };
        let app = MarmotApp::with_relays_and_account_home(&home, relays, account_home.clone());
        let runtime = MarmotAppRuntime::new(app.clone());
        Ok(Self {
            app,
            runtime,
            account_home,
            home,
        })
    }

    pub fn home(&self) -> &PathBuf {
        &self.home
    }

    pub fn app(&self) -> &MarmotApp {
        &self.app
    }

    pub fn runtime(&self) -> &MarmotAppRuntime {
        &self.runtime
    }

    /// Spawn background account workers and reconcile state from disk.
    pub async fn start(&self) -> Result<()> {
        self.runtime
            .start()
            .await
            .context("starting darkmatter runtime")
    }

    pub async fn shutdown(&self) {
        self.runtime.shutdown().await;
    }

    pub async fn catch_up(&self) -> Result<()> {
        self.runtime
            .catch_up_accounts()
            .await
            .context("catching up darkmatter accounts")
    }

    pub async fn relay_health(&self) -> RelayPlaneHealth {
        self.runtime
            .shared_services()
            .relay_plane()
            .relay_health()
            .await
    }

    /// Look up a managed account by label, account_id_hex, or npub. marmot-app
    /// labels freshly-created signing accounts with their own `account_id_hex`
    /// (not a friendly name), so an `npub` reference is normalized to hex before
    /// matching.
    pub fn find_account(&self, account_ref: &str) -> Result<Option<ManagedAccount>> {
        let needle = account_lookup_ref(account_ref);
        let accounts = self
            .runtime
            .accounts()
            .managed_accounts()
            .context("listing managed darkmatter accounts")?;
        Ok(accounts
            .into_iter()
            .find(|account| account.account_id_hex == needle || account.label == needle))
    }

    /// Ensure the managed desktop account exists and return its
    /// `account_id_hex`.
    ///
    /// `configured` is the previously-persisted account reference (npub or hex)
    /// from `config.darkmatter.account`, if any. When it resolves to an
    /// existing account the account is reused unchanged. Otherwise a fresh
    /// keypair is generated locally through [`marmot_account::AccountHome`] and
    /// persisted to the configured secret store. Discovery material is
    /// published separately by [`Self::publish_discovery`] so relay failures do
    /// not roll back a usable local identity.
    ///
    /// (Looking up by the persisted reference — rather than a constant friendly
    /// label — is what makes the identity stable across `setup`→`listen` and
    /// across restarts; marmot-app does not honor a custom label on create.)
    pub async fn ensure_account(
        &self,
        configured: Option<&str>,
        relays: &[String],
    ) -> Result<String> {
        if let Some(reference) = configured.map(str::trim).filter(|r| !r.is_empty())
            && let Some(account) = self.find_account(reference)?
        {
            return Ok(account.account_id_hex);
        }
        if relays.is_empty() {
            bail!("ensure_account requires at least one relay url");
        }
        if let Some(account) = self
            .account_home
            .accounts()
            .context("listing local darkmatter accounts")?
            .into_iter()
            .find(|account| account.local_signing)
        {
            return Ok(account.account_id_hex);
        }
        let account = self
            .account_home
            .create_nostr_account()
            .context("creating local darkmatter account")?;
        self.runtime
            .restart_account(&account.account_id_hex)
            .await
            .with_context(|| format!("starting darkmatter account {}", account.account_id_hex))?;
        Ok(account.account_id_hex)
    }

    /// Publish the discovery records clients need to find and invite this
    /// desktop. This is retryable network work, intentionally kept separate
    /// from local account creation.
    pub async fn publish_discovery(
        &self,
        account_id_hex: &str,
        config: &DarkmatterConfig,
    ) -> Result<()> {
        self.publish_configured_profile(account_id_hex, config)
            .await?;
        self.runtime
            .publish_key_package(account_id_hex)
            .await
            .map_err(|err| anyhow::anyhow!("darkmatter publish_key_package: {err}"))?;
        Ok(())
    }

    /// Keep the account's public relay-list records aligned with
    /// `darkmatter.message_relays`. Darkmatter uses these lists for outbox,
    /// inbox, and key-package routing; stale lists can cause publishes to keep
    /// targeting relays the local config no longer names.
    pub async fn sync_configured_relay_lists(
        &self,
        account_id_hex: &str,
        relays: &[String],
    ) -> Result<()> {
        if relays.is_empty() {
            bail!("sync_configured_relay_lists requires at least one relay url");
        }

        let desired = relays.to_vec();
        let desired_endpoints = relay_endpoints(relays);
        let bootstrap_endpoints = desired_endpoints.clone();

        if self.runtime.account_nip65_relays(account_id_hex)? != desired {
            self.runtime
                .set_account_nip65_relays(
                    account_id_hex,
                    desired_endpoints.clone(),
                    bootstrap_endpoints.clone(),
                )
                .await
                .map_err(|err| anyhow::anyhow!("darkmatter set NIP-65 relays: {err}"))?;
        }
        if self.runtime.account_inbox_relays(account_id_hex)? != desired {
            self.runtime
                .set_account_inbox_relays(
                    account_id_hex,
                    desired_endpoints.clone(),
                    bootstrap_endpoints.clone(),
                )
                .await
                .map_err(|err| anyhow::anyhow!("darkmatter set inbox relays: {err}"))?;
        }
        if self.runtime.account_key_package_relays(account_id_hex)? != desired {
            self.runtime
                .set_account_key_package_relays(
                    account_id_hex,
                    desired_endpoints,
                    bootstrap_endpoints,
                )
                .await
                .map_err(|err| anyhow::anyhow!("darkmatter set key-package relays: {err}"))?;
        }

        Ok(())
    }

    /// Publish the configured Nostr kind:0 profile for the managed desktop
    /// account. The profile is intentionally derived from agentnoise config so
    /// setup, listener startup, and `identity rename` all publish the same
    /// local machine identity.
    pub async fn publish_configured_profile(
        &self,
        account_id_hex: &str,
        config: &DarkmatterConfig,
    ) -> Result<UserProfileMetadata> {
        if config.message_relays.is_empty() {
            bail!("publish_configured_profile requires at least one relay url");
        }
        self.sync_configured_relay_lists(account_id_hex, &config.message_relays)
            .await?;
        let relays = relay_endpoints(&config.message_relays);
        let bootstrap = AccountRelayListBootstrap::new(relays.clone(), relays);
        let profile = configured_profile_metadata(config, current_unix_seconds());
        self.runtime
            .publish_user_profile(account_id_hex, profile, bootstrap)
            .await
            .map_err(|err| anyhow::anyhow!("darkmatter publish_user_profile: {err}"))
    }
}

fn relay_endpoints(relays: &[String]) -> Vec<TransportEndpoint> {
    relays
        .iter()
        .map(|url| TransportEndpoint(url.clone()))
        .collect()
}

fn configured_profile_metadata(config: &DarkmatterConfig, created_at: u64) -> UserProfileMetadata {
    UserProfileMetadata {
        name: non_empty_profile_field(&config.profile_name),
        display_name: non_empty_profile_field(&config.profile_display_name),
        about: non_empty_profile_field(&config.profile_about),
        picture: None,
        nip05: None,
        lud16: None,
        created_at,
        source_relays: Vec::new(),
    }
}

fn non_empty_profile_field(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn current_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cgka_traits::TransportEndpoint;
    use marmot_app::MarmotAppRuntime;
    use nostr::Keys;
    use nostr::nips::nip19::ToBech32;
    use nostr_relay_builder::MockRelay;

    #[test]
    fn account_lookup_ref_normalizes_npub_to_hex() {
        let keys = Keys::generate();
        let hex = keys.public_key().to_hex();
        let npub = keys.public_key().to_bech32().unwrap();

        // npub resolves to the same account_id_hex marmot-account stores.
        assert_eq!(account_lookup_ref(&npub), hex);
        // hex passes through unchanged (trimmed).
        assert_eq!(account_lookup_ref(&format!("  {hex}  ")), hex);
    }

    #[test]
    fn keychain_service_for_instance_namespaces_by_name() {
        assert_eq!(keychain_service_for_instance(None), "agentnoise");
        assert_eq!(
            keychain_service_for_instance(Some("alice")),
            "agentnoise-alice"
        );
        assert_eq!(keychain_service_for_instance(Some("  ")), "agentnoise");
    }

    #[test]
    fn configured_profile_metadata_uses_darkmatter_config_labels() {
        let mut config = crate::config::Config::template().darkmatter;
        config.profile_name = "desktop-one".to_string();
        config.profile_display_name = "Desktop One".to_string();
        config.profile_about = "Local helper".to_string();

        let profile = configured_profile_metadata(&config, 123);

        assert_eq!(profile.name.as_deref(), Some("desktop-one"));
        assert_eq!(profile.display_name.as_deref(), Some("Desktop One"));
        assert_eq!(profile.about.as_deref(), Some("Local helper"));
        assert_eq!(profile.created_at, 123);
        assert!(profile.source_relays.is_empty());
    }

    #[tokio::test]
    async fn publish_discovery_repairs_existing_account_without_key_package() {
        let relay = MockRelay::run().await.expect("mock relay");
        let relay_url = relay.url().await.to_string();
        let relays = vec![relay_url.clone()];
        let endpoints = vec![TransportEndpoint(relay_url)];
        let home = tempfile::tempdir().expect("temp darkmatter home");
        let account_home = AccountHome::open(home.path());
        let app = MarmotApp::with_relays_and_account_home(
            home.path(),
            relays.clone(),
            account_home.clone(),
        );
        let runtime = MarmotAppRuntime::new(app.clone());
        runtime.start().await.expect("runtime starts");
        let engine = DarkmatterEngine {
            app,
            runtime,
            account_home,
            home: home.path().to_path_buf(),
        };

        let account = engine
            .account_home
            .create_nostr_account()
            .expect("create local signing account");
        let account_id_hex = account.account_id_hex;
        engine
            .runtime
            .restart_account(&account_id_hex)
            .await
            .expect("start local account");

        let before = engine
            .runtime
            .account_key_packages(&account_id_hex, endpoints.clone())
            .await
            .expect("list key packages before repair");
        assert!(before.is_empty());

        let ensured = engine
            .ensure_account(Some(&account_id_hex), &relays)
            .await
            .expect("ensure existing account");
        assert_eq!(ensured, account_id_hex);

        let mut config = crate::config::Config::template().darkmatter;
        config.message_relays = relays.clone();
        engine
            .publish_discovery(&account_id_hex, &config)
            .await
            .expect("publish missing discovery");

        let after = engine
            .runtime
            .account_key_packages(&account_id_hex, endpoints)
            .await
            .expect("list key packages after repair");
        assert_eq!(after.len(), 1);
        assert!(!after[0].key_package_event_id.is_empty());
        assert!(!after[0].key_package_ref_hex.is_empty());

        engine.shutdown().await;
    }
}
