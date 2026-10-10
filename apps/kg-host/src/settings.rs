use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use directories::ProjectDirs;
use kg_protocol::Resolution;
use kg_provider::{MachineTypeId, ProviderAccess};
use rand_core::{OsRng, RngCore};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use ssh_key::{Algorithm, LineEnding, PrivateKey};

const KEYRING_SERVICE: &str = "kyber-gaming";
const KEYRING_USER: &str = "provider";

/// Non-secret preferences. Provider credentials live in the OS keychain, never in this file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub machine_type: Option<MachineTypeId>,
    pub resolution: Resolution,
    pub runtime: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            machine_type: None,
            resolution: Resolution { width: 1920, height: 1080 },
            runtime: "ghcr.io/alexandretrichot/kg-runtime:latest".into(),
        }
    }
}

fn dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("gg", "kyber-gaming", "kyber-gaming").context("no home directory")
}

impl Settings {
    fn path() -> Result<PathBuf> {
        Ok(dirs()?.config_dir().join("settings.toml"))
    }

    pub fn load() -> Result<Self> {
        match fs::read_to_string(Self::path()?) {
            Ok(text) => toml::from_str(&text).context("reading settings.toml"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        fs::create_dir_all(path.parent().expect("settings path has a parent"))?;
        fs::write(path, toml::to_string_pretty(self)?)?;
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct StoredAccess {
    kind: String,
    config: String,
}

pub fn load_access() -> Result<Option<ProviderAccess>> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)?;
    match entry.get_password() {
        Ok(json) => {
            let stored: StoredAccess = serde_json::from_str(&json).context("reading stored provider access")?;
            Ok(Some(ProviderAccess { kind: stored.kind, config: SecretString::from(stored.config) }))
        }
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn save_access(access: &ProviderAccess) -> Result<()> {
    let stored = StoredAccess { kind: access.kind.clone(), config: access.config.expose_secret().to_string() };
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)?.set_password(&serde_json::to_string(&stored)?)?;
    Ok(())
}

/// The key the host uses to reach its instances, created on first use. Returns the OpenSSH public key.
pub fn ssh_public_key() -> Result<String> {
    let path = dirs()?.data_dir().join("id_ed25519");
    let key = match fs::read_to_string(&path) {
        Ok(pem) => PrivateKey::from_openssh(pem).context("reading the host SSH key")?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let key = PrivateKey::random(&mut OsRng, Algorithm::Ed25519)?;
            fs::create_dir_all(path.parent().expect("key path has a parent"))?;
            fs::write(&path, key.to_openssh(LineEnding::LF)?.as_bytes())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
            }
            key
        }
        Err(e) => return Err(e.into()),
    };
    Ok(key.public_key().to_openssh()?)
}

/// Fresh for every session: the host hands it to its own Kyber client, nobody types it.
pub fn session_password() -> SecretString {
    const ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    SecretString::from(bytes.iter().map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char).collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_roundtrip_through_toml() {
        let settings = Settings { machine_type: Some(MachineTypeId("L4-1-24G".into())), ..Settings::default() };
        let text = toml::to_string_pretty(&settings).unwrap();
        assert_eq!(toml::from_str::<Settings>(&text).unwrap(), settings);
    }

    #[test]
    fn session_passwords_are_long_and_distinct() {
        let (a, b) = (session_password(), session_password());
        assert_eq!(a.expose_secret().len(), 32);
        assert_ne!(a.expose_secret(), b.expose_secret());
    }
}
