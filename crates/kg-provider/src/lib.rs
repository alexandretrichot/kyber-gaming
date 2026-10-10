//! Cloud provider abstraction.
//!
//! Every implementation launches the same base system: Ubuntu 24.04 with the NVIDIA driver installed and
//! the [`LaunchSpec::ssh_public_key`] authorized for `root`. Everything above that is set up over SSH by
//! the host, so providers never need cloud-init or user data.

use std::fmt;
use std::net::IpAddr;

use async_trait::async_trait;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InstanceId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VolumeId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MachineTypeId(pub String);

impl fmt::Display for InstanceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstanceState {
    Starting,
    Running,
    Stopping,
    Stopped,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Instance {
    pub id: InstanceId,
    pub name: String,
    pub state: InstanceState,
    pub public_ip: Option<IpAddr>,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Gpu {
    pub model: String,
    pub count: u32,
    pub vram_gb: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MachineType {
    pub id: MachineTypeId,
    pub gpu: Option<Gpu>,
    pub vcpus: u32,
    pub ram_gb: u32,
    pub hourly_price_eur: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub name: String,
    pub machine_type: MachineTypeId,
    pub root_disk_gb: u32,
    pub ssh_public_key: String,
    /// Instances are found again by tag, so the host can clean up the ones it lost track of.
    pub tags: Vec<String>,
    pub cache_volume: Option<VolumeId>,
}

#[async_trait]
pub trait Provider: Send + Sync {
    fn kind(&self) -> &'static str;

    async fn machine_types(&self) -> Result<Vec<MachineType>>;

    /// Creates and powers on the instance. Returns once the provider accepted it, not once SSH is up.
    async fn launch(&self, spec: &LaunchSpec) -> Result<Instance>;

    async fn get(&self, id: &InstanceId) -> Result<Option<Instance>>;

    async fn list_tagged(&self, tag: &str) -> Result<Vec<Instance>>;

    /// Deletes the instance and its boot disk. Other attached volumes, such as the cache, are kept.
    async fn destroy(&self, id: &InstanceId) -> Result<()>;

    /// Same outcome as [`Provider::destroy`], but called from the instance itself, so it may never return.
    /// The caller unmounts every extra volume first.
    async fn self_destruct(&self, id: &InstanceId) -> Result<()>;

    /// Deletes what self destructed instances may leave behind (boot disks on some providers).
    /// Never touches volumes it did not create, such as the cache. Returns how many resources went away.
    async fn sweep(&self, tag: &str) -> Result<usize>;
}

/// What the host hands over so another process (the VM agent) can drive the same provider account.
///
/// `config` is provider-specific and opaque to everything but that provider's [`ProviderFactory`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderAccess {
    pub kind: String,
    #[serde(serialize_with = "expose_secret")]
    pub config: SecretString,
}

fn expose_secret<S: serde::Serializer>(secret: &SecretString, serializer: S) -> std::result::Result<S::Ok, S::Error> {
    use secrecy::ExposeSecret;
    serializer.serialize_str(secret.expose_secret())
}

pub trait ProviderFactory: Send + Sync {
    fn kind(&self) -> &'static str;
    fn connect(&self, access: &ProviderAccess) -> Result<Box<dyn Provider>>;
}

#[derive(Default)]
pub struct Registry {
    factories: Vec<Box<dyn ProviderFactory>>,
}

impl Registry {
    pub fn with(mut self, factory: impl ProviderFactory + 'static) -> Self {
        self.factories.push(Box::new(factory));
        self
    }

    pub fn connect(&self, access: &ProviderAccess) -> Result<Box<dyn Provider>> {
        self.factories
            .iter()
            .find(|f| f.kind() == access.kind)
            .ok_or_else(|| Error::UnknownProvider(access.kind.clone()))?
            .connect(access)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unknown provider {0:?}")]
    UnknownProvider(String),
    #[error("invalid provider configuration: {0}")]
    InvalidConfig(String),
    #[error("provider API returned {status}: {message}")]
    Api { status: u16, message: String },
    #[error("no capacity: {0}")]
    NoCapacity(String),
    #[error("timed out waiting for {0}")]
    Timeout(String),
    #[error(transparent)]
    Transport(Box<dyn std::error::Error + Send + Sync>),
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    struct Dummy;

    impl ProviderFactory for Dummy {
        fn kind(&self) -> &'static str {
            "dummy"
        }
        fn connect(&self, _: &ProviderAccess) -> Result<Box<dyn Provider>> {
            Err(Error::InvalidConfig("dummy".into()))
        }
    }

    fn access(kind: &str) -> ProviderAccess {
        ProviderAccess { kind: kind.into(), config: SecretString::from("{\"secret\":\"hunter2\"}") }
    }

    #[test]
    fn registry_dispatches_on_kind() {
        let registry = Registry::default().with(Dummy);
        assert!(matches!(registry.connect(&access("dummy")), Err(Error::InvalidConfig(_))));
        assert!(matches!(registry.connect(&access("other")), Err(Error::UnknownProvider(k)) if k == "other"));
    }

    #[test]
    fn access_redacts_in_debug_but_survives_serde() {
        let original = access("dummy");
        assert!(!format!("{original:?}").contains("hunter2"));

        let wire = serde_json::to_string(&original).unwrap();
        let back: ProviderAccess = serde_json::from_str(&wire).unwrap();
        assert_eq!(back.config.expose_secret(), original.config.expose_secret());
    }
}
