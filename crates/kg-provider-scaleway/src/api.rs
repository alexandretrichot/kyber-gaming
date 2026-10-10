use std::collections::HashMap;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct ApiError {
    pub message: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ServerEnvelope {
    pub server: Server,
}

#[derive(Debug, Deserialize)]
pub struct ServerList {
    pub servers: Vec<Server>,
}

#[derive(Debug, Deserialize)]
pub struct Server {
    pub id: String,
    pub name: String,
    pub state: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub allowed_actions: Vec<String>,
    #[serde(default)]
    pub public_ips: Vec<ServerIp>,
    pub public_ip: Option<ServerIp>,
    #[serde(default)]
    pub volumes: HashMap<String, ServerVolume>,
}

#[derive(Debug, Deserialize)]
pub struct ServerIp {
    pub address: IpAddr,
    #[serde(default)]
    pub family: String,
}

#[derive(Debug, Deserialize)]
pub struct ServerVolume {
    pub id: String,
    #[serde(default)]
    pub volume_type: String,
}

#[derive(Debug, Serialize)]
pub struct CreateServer<'a> {
    pub name: &'a str,
    pub commercial_type: &'a str,
    pub image: &'a str,
    pub project: &'a str,
    pub tags: &'a [String],
    pub dynamic_ip_required: bool,
    pub volumes: HashMap<&'static str, VolumeTemplate<'a>>,
}

#[derive(Debug, Serialize)]
pub struct VolumeTemplate<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    pub volume_type: &'static str,
}

#[derive(Debug, Serialize)]
pub struct ServerAction<'a> {
    pub action: &'a str,
}

#[derive(Debug, Deserialize)]
pub struct LocalImages {
    pub local_images: Vec<LocalImage>,
}

#[derive(Debug, Deserialize)]
pub struct LocalImage {
    pub id: String,
    #[serde(default)]
    pub compatible_commercial_types: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Products {
    pub servers: HashMap<String, Product>,
}

#[derive(Debug, Deserialize)]
pub struct Product {
    pub ncpus: u32,
    pub ram: u64,
    #[serde(default)]
    pub gpu: u32,
    pub gpu_info: Option<GpuInfo>,
    pub hourly_price: Option<f64>,
    #[serde(default)]
    pub end_of_service: bool,
}

#[derive(Debug, Deserialize)]
pub struct GpuInfo {
    pub gpu_name: String,
    pub gpu_memory: u64,
}

#[derive(Debug, Deserialize)]
pub struct SshKeys {
    pub ssh_keys: Vec<SshKey>,
}

#[derive(Debug, Deserialize)]
pub struct SshKey {
    pub public_key: String,
}

#[derive(Debug, Serialize)]
pub struct CreateSshKey<'a> {
    pub name: &'a str,
    pub public_key: &'a str,
    pub project_id: &'a str,
}

#[derive(Debug, Deserialize)]
pub struct BlockVolume {
    pub status: String,
}

#[derive(Debug, Deserialize)]
pub struct BlockVolumes {
    pub volumes: Vec<BlockVolumeSummary>,
}

#[derive(Debug, Deserialize)]
pub struct BlockVolumeSummary {
    pub id: String,
    pub status: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct TagVolume<'a> {
    pub tags: Vec<&'a str>,
}

#[derive(Debug, Serialize)]
pub struct DetachVolume<'a> {
    pub volume_id: &'a str,
}
