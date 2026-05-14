use std::io::{self, BufRead, IsTerminal, Write};

use anyhow::{Context, Result, bail};
use keyring::{Entry, Error as KeyringError};
use zeroize::Zeroize;

pub const DEFAULT_SERVICE: &str = "agentnoise";
pub const DEFAULT_ITEM: &str = "whitenoise-nsec";

#[derive(Debug, Clone)]
pub struct SecretStore {
    service: String,
    item: String,
}

impl SecretStore {
    pub fn new(service: impl Into<String>, item: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            item: item.into(),
        }
    }

    pub fn entry(&self) -> Result<Entry> {
        Entry::new(&self.service, &self.item).context("opening OS keychain entry")
    }

    pub fn store_nsec(&self, nsec: &str) -> Result<()> {
        let nsec = nsec.trim();
        validate_nsec(nsec)?;
        self.entry()?
            .set_password(nsec)
            .context("storing nsec in OS keychain")
    }

    pub fn load_nsec(&self) -> Result<String> {
        let mut secret = self
            .entry()?
            .get_password()
            .context("loading nsec from OS keychain")?;
        let nsec = secret.trim().to_string();
        secret.zeroize();
        validate_nsec(&nsec)?;
        Ok(nsec)
    }

    pub fn delete_nsec(&self) -> Result<()> {
        match self.entry()?.delete_credential() {
            Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
            Err(error) => Err(error).context("deleting nsec from OS keychain"),
        }
    }

    pub fn nsec_status(&self) -> Result<bool> {
        match self.entry()?.get_password() {
            Ok(mut secret) => {
                let present = validate_nsec(&secret).is_ok();
                secret.zeroize();
                Ok(present)
            }
            Err(KeyringError::NoEntry) => Ok(false),
            Err(error) => Err(error).context("loading nsec from OS keychain"),
        }
    }

    pub fn has_nsec(&self) -> bool {
        self.nsec_status().unwrap_or(false)
    }

    pub fn label(&self) -> String {
        format!("{} / {}", self.service, self.item)
    }
}

pub fn read_nsec_interactive() -> Result<String> {
    let mut nsec = if io::stdin().is_terminal() {
        eprint!("Enter White Noise nsec: ");
        io::stderr().flush().ok();
        let secret = rpassword::read_password().context("reading nsec")?;
        eprintln!();
        secret
    } else {
        let mut buf = String::new();
        io::stdin()
            .lock()
            .read_line(&mut buf)
            .context("reading nsec from stdin")?;
        buf
    };
    nsec = nsec.trim().to_string();
    validate_nsec(&nsec)?;
    Ok(nsec)
}

pub fn validate_nsec(nsec: &str) -> Result<()> {
    let nsec = nsec.trim();
    if nsec.is_empty() {
        bail!("nsec is empty");
    }
    if !nsec.starts_with("nsec1") {
        bail!("expected an nsec1... secret");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_nsec() {
        assert!(validate_nsec("npub1abc").is_err());
    }
}
