use std::time::Duration;

use kg_provider::{Error, InstanceId, InstanceState, LaunchSpec, MachineTypeId, Provider, Registry, VolumeId};
use secrecy::SecretString;
use serde_json::{Value, json};
use wiremock::matchers::{body_partial_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{Config, Factory, Scaleway};

const ZONE: &str = "fr-par-2";
const SERVER: &str = "11111111-1111-1111-1111-111111111111";
const ROOT_VOLUME: &str = "22222222-2222-2222-2222-222222222222";
const KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExample kyber-gaming";

fn config() -> Config {
    Config {
        access_key: "SCWXXXXXXXXXXXXXXXXX".into(),
        secret_key: SecretString::from("secret-token"),
        project_id: "project".into(),
        zone: ZONE.into(),
    }
}

fn provider(server: &MockServer) -> Scaleway {
    Scaleway {
        base_url: server.uri(),
        poll_interval: Duration::from_millis(1),
        timeout: Duration::from_secs(5),
        ..Scaleway::new(config())
    }
}

fn server_json(state: &str) -> Value {
    json!({ "server": {
        "id": SERVER,
        "name": "gaming",
        "state": state,
        "tags": ["kyber-gaming"],
        "allowed_actions": if state == "running" { json!(["poweroff", "reboot"]) } else { json!(["poweron"]) },
        "public_ips": [{ "address": "51.159.1.2", "family": "inet" }],
        "public_ip": null,
        "volumes": { "0": { "id": ROOT_VOLUME, "volume_type": "sbs_volume" } }
    }})
}

fn instance_path(rest: &str) -> String {
    format!("/instance/v1/zones/{ZONE}/{rest}")
}

fn spec() -> LaunchSpec {
    LaunchSpec {
        name: "gaming".into(),
        machine_type: MachineTypeId("L4-1-24G".into()),
        root_disk_gb: 80,
        ssh_public_key: KEY.into(),
        tags: vec!["kyber-gaming".into()],
        cache_volume: Some(VolumeId("cache-volume".into())),
    }
}

#[tokio::test]
async fn launch_registers_key_resolves_image_creates_and_powers_on() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/iam/v1alpha1/ssh-keys"))
        .and(header("X-Auth-Token", "secret-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ssh_keys": [] })))
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/iam/v1alpha1/ssh-keys"))
        .and(body_partial_json(json!({ "public_key": KEY, "project_id": "project" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/marketplace/v2/local-images"))
        .and(query_param("zone", ZONE))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "local_images": [
            { "id": "image-for-cpu", "compatible_commercial_types": ["POP2-2C-8G"] },
            { "id": "image-for-gpu", "compatible_commercial_types": ["L4-1-24G", "L40S-1-48G"] }
        ]})))
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path(instance_path("servers")))
        .and(body_partial_json(json!({
            "commercial_type": "L4-1-24G",
            "image": "image-for-gpu",
            "project": "project",
            "dynamic_ip_required": true,
            "tags": ["kyber-gaming"],
            "volumes": {
                "0": { "size": 80_000_000_000u64, "volume_type": "sbs_volume" },
                "1": { "id": "cache-volume", "volume_type": "sbs_volume" }
            }
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(server_json("stopped")))
        .expect(1)
        .mount(&mock)
        .await;
    Mock::given(method("PATCH"))
        .and(path(format!("/block/v1/zones/{ZONE}/volumes/{ROOT_VOLUME}")))
        .and(body_partial_json(json!({ "tags": ["kyber-gaming", "kg-boot-disk"] })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path(instance_path(&format!("servers/{SERVER}/action"))))
        .and(body_partial_json(json!({ "action": "poweron" })))
        .respond_with(ResponseTemplate::new(202).set_body_json(json!({})))
        .expect(1)
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path(instance_path(&format!("servers/{SERVER}"))))
        .respond_with(ResponseTemplate::new(200).set_body_json(server_json("starting")))
        .mount(&mock)
        .await;

    let instance = provider(&mock).launch(&spec()).await.unwrap();

    assert_eq!(instance.id, InstanceId(SERVER.into()));
    assert_eq!(instance.state, InstanceState::Starting);
    assert_eq!(instance.public_ip, Some("51.159.1.2".parse().unwrap()));
}

#[tokio::test]
async fn launch_skips_key_already_registered_with_another_comment() {
    let mock = MockServer::start().await;
    let stored = KEY.replace("kyber-gaming", "someone@laptop");
    Mock::given(method("GET"))
        .and(path("/iam/v1alpha1/ssh-keys"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ssh_keys": [{ "public_key": stored }] })))
        .mount(&mock)
        .await;
    Mock::given(method("POST")).and(path("/iam/v1alpha1/ssh-keys")).respond_with(ResponseTemplate::new(200)).expect(0).mount(&mock).await;
    Mock::given(method("GET"))
        .and(path("/marketplace/v2/local-images"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "local_images": [] })))
        .mount(&mock)
        .await;

    let error = provider(&mock).launch(&spec()).await.unwrap_err();

    assert!(matches!(error, Error::InvalidConfig(m) if m.contains("L4-1-24G")));
}

#[tokio::test]
async fn out_of_stock_maps_to_no_capacity() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/iam/v1alpha1/ssh-keys"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ssh_keys": [{ "public_key": KEY }] })))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/marketplace/v2/local-images"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "local_images": [
            { "id": "image-for-gpu", "compatible_commercial_types": ["L4-1-24G"] }
        ]})))
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path(instance_path("servers")))
        .respond_with(ResponseTemplate::new(412).set_body_json(json!({ "type": "out_of_stock", "message": "L4-1-24G is out of stock" })))
        .mount(&mock)
        .await;

    let error = provider(&mock).launch(&spec()).await.unwrap_err();

    assert!(matches!(error, Error::NoCapacity(_)), "{error:?}");
}

#[tokio::test]
async fn destroy_powers_off_deletes_server_then_boot_disk() {
    let mock = MockServer::start().await;
    let server = instance_path(&format!("servers/{SERVER}"));
    Mock::given(method("GET"))
        .and(path(server.clone()))
        .respond_with(ResponseTemplate::new(200).set_body_json(server_json("running")))
        .up_to_n_times(2)
        .with_priority(1)
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path(server.clone()))
        .respond_with(ResponseTemplate::new(200).set_body_json(server_json("stopped")))
        .with_priority(2)
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{server}/action")))
        .and(body_partial_json(json!({ "action": "poweroff" })))
        .respond_with(ResponseTemplate::new(202).set_body_json(json!({})))
        .expect(1)
        .mount(&mock)
        .await;
    Mock::given(method("DELETE")).and(path(server)).respond_with(ResponseTemplate::new(204)).expect(1).mount(&mock).await;
    let volume = format!("/block/v1/zones/{ZONE}/volumes/{ROOT_VOLUME}");
    Mock::given(method("GET"))
        .and(path(volume.clone()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "status": "available" })))
        .mount(&mock)
        .await;
    Mock::given(method("DELETE")).and(path(volume)).respond_with(ResponseTemplate::new(204)).expect(1).mount(&mock).await;

    provider(&mock).destroy(&InstanceId(SERVER.into())).await.unwrap();
}

#[tokio::test]
async fn destroy_of_unknown_server_is_a_no_op() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(instance_path(&format!("servers/{SERVER}"))))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({ "type": "unknown_resource", "message": "not found" })))
        .mount(&mock)
        .await;

    provider(&mock).destroy(&InstanceId(SERVER.into())).await.unwrap();
}

#[tokio::test]
async fn list_tagged_filters_on_tag() {
    let mock = MockServer::start().await;
    let server = server_json("running")["server"].clone();
    Mock::given(method("GET"))
        .and(path(instance_path("servers")))
        .and(query_param("tags", "kyber-gaming"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "servers": [server] })))
        .mount(&mock)
        .await;

    let instances = provider(&mock).list_tagged("kyber-gaming").await.unwrap();

    assert_eq!(instances.len(), 1);
    assert_eq!(instances[0].state, InstanceState::Running);
}

#[tokio::test]
async fn machine_types_expose_gpus_and_skip_end_of_service() {
    let mock = MockServer::start().await;
    let products = instance_path("products/servers");
    Mock::given(method("GET"))
        .and(path(products.clone()))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "servers": {
            "L4-1-24G": { "ncpus": 8, "ram": 51539607552u64, "gpu": 1,
                          "gpu_info": { "gpu_name": "L4", "gpu_memory": 25769803776u64 }, "hourly_price": 0.7875 },
            "OLD-1": { "ncpus": 2, "ram": 2147483648u64, "gpu": 0, "end_of_service": true },
            "POP2-2C-8G": { "ncpus": 2, "ram": 8589934592u64, "gpu": 0, "gpu_info": null, "hourly_price": 0.07 }
        }})))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path(products))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "servers": {} })))
        .mount(&mock)
        .await;

    let types = provider(&mock).machine_types().await.unwrap();

    assert_eq!(types.iter().map(|t| t.id.0.as_str()).collect::<Vec<_>>(), ["L4-1-24G", "POP2-2C-8G"]);
    let gpu = types[0].gpu.as_ref().unwrap();
    assert_eq!((gpu.model.as_str(), gpu.count, gpu.vram_gb), ("L4", 1, 24));
    assert_eq!(types[0].ram_gb, 48);
    assert!(types[1].gpu.is_none());
}

#[test]
fn access_roundtrips_through_the_registry() {
    let access = config().to_access();
    assert_eq!(access.kind, "scaleway");
    assert!(!format!("{access:?}").contains("secret-token"));

    let provider = Registry::default().with(Factory).connect(&access).unwrap();
    assert_eq!(provider.kind(), "scaleway");
}

#[tokio::test]
async fn self_destruct_detaches_extra_volumes_then_terminates() {
    let mock = MockServer::start().await;
    let server = instance_path(&format!("servers/{SERVER}"));
    let mut body = server_json("running");
    body["server"]["volumes"]["1"] = json!({ "id": "cache-volume", "volume_type": "sbs_volume" });
    Mock::given(method("GET")).and(path(server.clone())).respond_with(ResponseTemplate::new(200).set_body_json(body)).mount(&mock).await;
    Mock::given(method("POST"))
        .and(path(format!("{server}/detach-volume")))
        .and(body_partial_json(json!({ "volume_id": "cache-volume" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{server}/action")))
        .and(body_partial_json(json!({ "action": "terminate" })))
        .respond_with(ResponseTemplate::new(202).set_body_json(json!({})))
        .expect(1)
        .mount(&mock)
        .await;

    provider(&mock).self_destruct(&InstanceId(SERVER.into())).await.unwrap();
}

#[tokio::test]
async fn sweep_deletes_detached_boot_disks_only() {
    let mock = MockServer::start().await;
    let volumes = format!("/block/v1/zones/{ZONE}/volumes");
    Mock::given(method("GET"))
        .and(path(volumes.clone()))
        .and(query_param("tags", "kyber-gaming"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "volumes": [
            { "id": "leftover", "status": "available", "tags": ["kyber-gaming", "kg-boot-disk"] },
            { "id": "running-boot", "status": "in_use", "tags": ["kyber-gaming", "kg-boot-disk"] },
            { "id": "cache", "status": "available", "tags": ["kyber-gaming"] }
        ]})))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{volumes}/leftover")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "status": "available" })))
        .mount(&mock)
        .await;
    Mock::given(method("DELETE")).and(path(format!("{volumes}/leftover"))).respond_with(ResponseTemplate::new(204)).expect(1).mount(&mock).await;

    assert_eq!(provider(&mock).sweep("kyber-gaming").await.unwrap(), 1);
}
