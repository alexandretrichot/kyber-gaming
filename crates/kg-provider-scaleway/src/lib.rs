mod api;

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use kg_provider::{
    Error, Gpu, Instance, InstanceId, InstanceState, LaunchSpec, MachineType, MachineTypeId, Provider,
    ProviderAccess, ProviderFactory, Result,
};
use reqwest::{Method, RequestBuilder, Response};
use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::time::{Instant, sleep};

pub const KIND: &str = "scaleway";
const BASE_URL: &str = "https://api.scaleway.com";
/// Ubuntu 24.04 with the NVIDIA driver, the base system every provider must launch.
const BASE_IMAGE_LABEL: &str = "ubuntu_noble_gpu_os_13_nvidia";
const SSH_KEY_NAME: &str = "kyber-gaming";
/// `terminate` detaches SBS volumes instead of deleting them, so boot disks are marked for [`Provider::sweep`].
const BOOT_DISK_TAG: &str = "kg-boot-disk";
const GB: u64 = 1_000_000_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub access_key: String,
    #[serde(serialize_with = "expose_secret")]
    pub secret_key: SecretString,
    pub project_id: String,
    pub zone: String,
}

fn expose_secret<S: serde::Serializer>(secret: &SecretString, serializer: S) -> std::result::Result<S::Ok, S::Error> {
    serializer.serialize_str(secret.expose_secret())
}

impl Config {
    pub fn to_access(&self) -> ProviderAccess {
        let json = serde_json::to_string(self).expect("config serializes");
        ProviderAccess { kind: KIND.into(), config: SecretString::from(json) }
    }
}

pub struct Factory;

impl ProviderFactory for Factory {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn connect(&self, access: &ProviderAccess) -> Result<Box<dyn Provider>> {
        let config: Config = serde_json::from_str(access.config.expose_secret())
            .map_err(|e| Error::InvalidConfig(e.to_string()))?;
        Ok(Box::new(Scaleway::new(config)))
    }
}

pub struct Scaleway {
    http: reqwest::Client,
    config: Config,
    base_url: String,
    poll_interval: Duration,
    timeout: Duration,
}

impl Scaleway {
    pub fn new(config: Config) -> Self {
        Self {
            http: reqwest::Client::new(),
            config,
            base_url: BASE_URL.into(),
            poll_interval: Duration::from_secs(3),
            timeout: Duration::from_secs(600),
        }
    }

    fn instance_path(&self, rest: &str) -> String {
        format!("/instance/v1/zones/{}/{rest}", self.config.zone)
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.base_url))
            .header("X-Auth-Token", self.config.secret_key.expose_secret())
    }

    async fn send(&self, request: RequestBuilder) -> Result<Response> {
        let response = request.send().await.map_err(transport)?;
        if response.status().is_success() {
            return Ok(response);
        }
        let status = response.status().as_u16();
        let body: Option<api::ApiError> = response.json().await.ok();
        let message = body
            .as_ref()
            .and_then(|b| b.message.clone().or(b.kind.clone()))
            .unwrap_or_else(|| "no error message".into());
        let out_of_stock = body.as_ref().and_then(|b| b.kind.as_deref()) == Some("out_of_stock")
            || message.to_lowercase().contains("out of stock");
        Err(if out_of_stock { Error::NoCapacity(message) } else { Error::Api { status, message } })
    }

    async fn json<T: DeserializeOwned>(&self, request: RequestBuilder) -> Result<T> {
        self.send(request).await?.json().await.map_err(transport)
    }

    async fn optional<T: DeserializeOwned>(&self, request: RequestBuilder) -> Result<Option<T>> {
        match self.json(request).await {
            Err(Error::Api { status: 404, .. }) => Ok(None),
            other => other.map(Some),
        }
    }

    async fn server(&self, id: &str) -> Result<Option<api::Server>> {
        let request = self.request(Method::GET, &self.instance_path(&format!("servers/{id}")));
        Ok(self.optional::<api::ServerEnvelope>(request).await?.map(|e| e.server))
    }

    async fn action(&self, id: &str, action: &str) -> Result<()> {
        let request = self
            .request(Method::POST, &self.instance_path(&format!("servers/{id}/action")))
            .json(&api::ServerAction { action });
        self.send(request).await.map(drop)
    }

    async fn ensure_ssh_key(&self, public_key: &str) -> Result<()> {
        let request = self
            .request(Method::GET, "/iam/v1alpha1/ssh-keys")
            .query(&[("project_id", self.config.project_id.as_str()), ("page_size", "100")]);
        let keys: api::SshKeys = self.json(request).await?;
        if keys.ssh_keys.iter().any(|k| same_key(&k.public_key, public_key)) {
            return Ok(());
        }
        let request = self.request(Method::POST, "/iam/v1alpha1/ssh-keys").json(&api::CreateSshKey {
            name: SSH_KEY_NAME,
            public_key: public_key.trim(),
            project_id: &self.config.project_id,
        });
        self.send(request).await.map(drop)
    }

    async fn base_image(&self, machine_type: &str) -> Result<String> {
        let request = self.request(Method::GET, "/marketplace/v2/local-images").query(&[
            ("image_label", BASE_IMAGE_LABEL),
            ("zone", self.config.zone.as_str()),
            ("type", "instance_sbs"),
        ]);
        let images: api::LocalImages = self.json(request).await?;
        images
            .local_images
            .into_iter()
            .find(|i| i.compatible_commercial_types.iter().any(|t| t == machine_type))
            .map(|i| i.id)
            .ok_or_else(|| Error::InvalidConfig(format!("{BASE_IMAGE_LABEL} is not available for {machine_type} in {}", self.config.zone)))
    }

    async fn tag_boot_disk(&self, volume: &str, tags: &[String]) -> Result<()> {
        let tags = tags.iter().map(String::as_str).chain([BOOT_DISK_TAG]).collect();
        let request = self
            .request(Method::PATCH, &format!("/block/v1/zones/{}/volumes/{volume}", self.config.zone))
            .json(&api::TagVolume { tags });
        self.send(request).await.map(drop)
    }

    async fn wait_stopped(&self, id: &str) -> Result<()> {
        let deadline = Instant::now() + self.timeout;
        loop {
            match self.server(id).await? {
                None => return Ok(()),
                Some(s) if s.state == "stopped" => return Ok(()),
                Some(s) if s.allowed_actions.iter().any(|a| a == "poweroff") && s.state != "stopping" => {
                    self.action(id, "poweroff").await?;
                }
                Some(_) => {}
            }
            if Instant::now() >= deadline {
                return Err(Error::Timeout(format!("server {id} to stop")));
            }
            sleep(self.poll_interval).await;
        }
    }

    async fn delete_volume(&self, id: &str) -> Result<()> {
        let path = format!("/block/v1/zones/{}/volumes/{id}", self.config.zone);
        let deadline = Instant::now() + self.timeout;
        loop {
            match self.optional::<api::BlockVolume>(self.request(Method::GET, &path)).await? {
                None => return Ok(()),
                Some(v) if v.status == "available" => break,
                Some(_) if Instant::now() >= deadline => return Err(Error::Timeout(format!("volume {id} to detach"))),
                Some(_) => sleep(self.poll_interval).await,
            }
        }
        match self.send(self.request(Method::DELETE, &path)).await {
            Err(Error::Api { status: 404, .. }) => Ok(()),
            other => other.map(drop),
        }
    }
}

#[async_trait]
impl Provider for Scaleway {
    fn kind(&self) -> &'static str {
        KIND
    }

    async fn machine_types(&self) -> Result<Vec<MachineType>> {
        let mut types = Vec::new();
        for page in 1.. {
            let request = self
                .request(Method::GET, &self.instance_path("products/servers"))
                .query(&[("per_page", "100"), ("page", &page.to_string())]);
            let products: api::Products = self.json(request).await?;
            if products.servers.is_empty() {
                break;
            }
            types.extend(products.servers.into_iter().filter(|(_, p)| !p.end_of_service).map(machine_type));
        }
        types.sort_by(|a, b| a.id.0.cmp(&b.id.0));
        Ok(types)
    }

    async fn launch(&self, spec: &LaunchSpec) -> Result<Instance> {
        self.ensure_ssh_key(&spec.ssh_public_key).await?;
        let image = self.base_image(&spec.machine_type.0).await?;

        let mut volumes = HashMap::from([(
            "0",
            api::VolumeTemplate { id: None, size: Some(u64::from(spec.root_disk_gb) * GB), volume_type: "sbs_volume" },
        )]);
        if let Some(cache) = &spec.cache_volume {
            volumes.insert("1", api::VolumeTemplate { id: Some(&cache.0), size: None, volume_type: "sbs_volume" });
        }
        let request = self.request(Method::POST, &self.instance_path("servers")).json(&api::CreateServer {
            name: &spec.name,
            commercial_type: &spec.machine_type.0,
            image: &image,
            project: &self.config.project_id,
            tags: &spec.tags,
            dynamic_ip_required: true,
            volumes,
        });
        let created: api::ServerEnvelope = self.json(request).await?;
        let id = created.server.id;
        let boot_disk = created.server.volumes.get("0").map(|v| v.id.clone());

        let ready = async {
            if let Some(disk) = &boot_disk {
                self.tag_boot_disk(disk, &spec.tags).await?;
            }
            self.action(&id, "poweron").await
        };
        if let Err(e) = ready.await {
            let _ = self.destroy(&InstanceId(id)).await;
            return Err(e);
        }
        self.get(&InstanceId(id.clone()))
            .await?
            .ok_or_else(|| Error::Api { status: 404, message: format!("server {id} vanished after power on") })
    }

    async fn get(&self, id: &InstanceId) -> Result<Option<Instance>> {
        Ok(self.server(&id.0).await?.map(instance))
    }

    async fn list_tagged(&self, tag: &str) -> Result<Vec<Instance>> {
        let mut instances = Vec::new();
        for page in 1.. {
            let request = self
                .request(Method::GET, &self.instance_path("servers"))
                .query(&[("tags", tag), ("per_page", "100"), ("page", &page.to_string())]);
            let list: api::ServerList = self.json(request).await?;
            let count = list.servers.len();
            instances.extend(list.servers.into_iter().map(instance));
            if count < 100 {
                break;
            }
        }
        Ok(instances)
    }

    async fn destroy(&self, id: &InstanceId) -> Result<()> {
        let Some(server) = self.server(&id.0).await? else {
            return Ok(());
        };
        let root_volume = server
            .volumes
            .get("0")
            .filter(|v| v.volume_type == "sbs_volume")
            .map(|v| v.id.clone());

        self.wait_stopped(&id.0).await?;
        match self.send(self.request(Method::DELETE, &self.instance_path(&format!("servers/{}", id.0)))).await {
            Err(Error::Api { status: 404, .. }) | Ok(_) => {}
            Err(e) => return Err(e),
        }
        // Deleting a server only detaches its SBS volumes, the boot disk has to go explicitly.
        match root_volume {
            Some(volume) => self.delete_volume(&volume).await,
            None => Ok(()),
        }
    }

    async fn self_destruct(&self, id: &InstanceId) -> Result<()> {
        let Some(server) = self.server(&id.0).await? else {
            return Ok(());
        };
        // `terminate` deletes every attached volume, so the cache has to leave first.
        for (slot, volume) in &server.volumes {
            if slot != "0" {
                let request = self
                    .request(Method::POST, &self.instance_path(&format!("servers/{}/detach-volume", id.0)))
                    .json(&api::DetachVolume { volume_id: &volume.id });
                self.send(request).await?;
            }
        }
        self.action(&id.0, "terminate").await
    }

    async fn sweep(&self, tag: &str) -> Result<usize> {
        let path = format!("/block/v1/zones/{}/volumes", self.config.zone);
        let mut swept = 0;
        for page in 1.. {
            let request = self.request(Method::GET, &path).query(&[
                ("project_id", self.config.project_id.as_str()),
                ("tags", tag),
                ("page_size", "100"),
                ("page", &page.to_string()),
            ]);
            let list: api::BlockVolumes = self.json(request).await?;
            let count = list.volumes.len();
            for volume in list.volumes {
                let leftover = volume.status == "available"
                    && volume.tags.iter().any(|t| t == tag)
                    && volume.tags.iter().any(|t| t == BOOT_DISK_TAG);
                if leftover {
                    self.delete_volume(&volume.id).await?;
                    swept += 1;
                }
            }
            if count < 100 {
                break;
            }
        }
        Ok(swept)
    }
}

fn transport(e: reqwest::Error) -> Error {
    Error::Transport(Box::new(e))
}

/// Scaleway may drop the comment of a stored key, so only the type and the key material are compared.
fn same_key(a: &str, b: &str) -> bool {
    a.split_whitespace().take(2).eq(b.split_whitespace().take(2))
}

fn instance(server: api::Server) -> Instance {
    let public_ip = server
        .public_ips
        .iter()
        .find(|ip| ip.family == "inet")
        .or(server.public_ips.first())
        .or(server.public_ip.as_ref())
        .map(|ip| ip.address);
    Instance {
        id: InstanceId(server.id),
        name: server.name,
        state: match server.state.as_str() {
            "starting" => InstanceState::Starting,
            "running" => InstanceState::Running,
            "stopping" => InstanceState::Stopping,
            "stopped" | "stopped in place" => InstanceState::Stopped,
            _ => InstanceState::Unknown,
        },
        public_ip,
        tags: server.tags,
    }
}

fn machine_type((id, product): (String, api::Product)) -> MachineType {
    MachineType {
        id: MachineTypeId(id),
        gpu: product.gpu_info.filter(|_| product.gpu > 0).map(|info| Gpu {
            model: info.gpu_name,
            count: product.gpu,
            vram_gb: (info.gpu_memory / (1 << 30)) as u32,
        }),
        vcpus: product.ncpus,
        ram_gb: (product.ram / (1 << 30)) as u32,
        hourly_price_eur: product.hourly_price,
    }
}

#[cfg(test)]
mod tests;
