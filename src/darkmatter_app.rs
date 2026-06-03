//! Bootstrap and lifecycle for the embedded [`marmot_app::MarmotApp`] /
//! [`marmot_app::MarmotAppRuntime`] used as agentnoise's protocol bridge.
//!
//! agentnoise pins to a single managed account inside a darkmatter home
//! directory, identified by the npub persisted in `config.darkmatter.account`.
//! On startup the engine reuses that account if present, otherwise creates a
//! fresh identity, then starts the runtime workers.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use cgka_traits::TransportEndpoint;
use marmot_account::AccountHome;
use marmot_app::{AccountRelayListBootstrap, ManagedAccount, MarmotApp, MarmotAppRuntime};
use nostr::PublicKey;
use nostr::nips::nip19::FromBech32;

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
    /// persisted to the configured secret store. Network discovery material is
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

    /// Broadcast the account relay lists clients use to find this desktop.
    ///
    /// This publishes all relay-list kinds Dark Matter currently cares about:
    /// NIP-65, inbox, and key-package relay lists. Callers should treat this as
    /// best-effort during setup/startup because slow public relays must not
    /// roll back an otherwise usable local identity.
    pub async fn publish_relay_lists(&self, account_id_hex: &str, relays: &[String]) -> Result<()> {
        if relays.is_empty() {
            bail!("cannot publish empty darkmatter relay list");
        }
        let endpoints = relays
            .iter()
            .map(|url| TransportEndpoint(url.clone()))
            .collect::<Vec<_>>();
        self.app
            .publish_account_relay_lists(
                account_id_hex,
                AccountRelayListBootstrap::new(endpoints.clone(), endpoints),
            )
            .await
            .context("publishing darkmatter account relay lists")?;
        Ok(())
    }

    /// Publish discovery material that lets another client create a new group
    /// with this desktop. This is intentionally separate from account creation:
    /// a slow or unavailable relay must not roll back the local keypair.
    pub async fn publish_discovery(&self, account_id_hex: &str, relays: &[String]) -> Result<()> {
        self.publish_relay_lists(account_id_hex, relays).await?;
        self.runtime
            .publish_key_package(account_id_hex)
            .await
            .map_err(|err| anyhow::anyhow!("publishing darkmatter key package: {err}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::Keys;
    use nostr::nips::nip19::ToBech32;

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
}
