//! Manual check against a real Scaleway account, credentials from the usual SCW_* variables.
//!
//! cargo run -p kg-provider-scaleway --example scw -- gpus
//! cargo run -p kg-provider-scaleway --example scw -- list
//! cargo run -p kg-provider-scaleway --example scw -- launch <machine-type> <ssh-public-key-file> [cache-volume-id]
//! cargo run -p kg-provider-scaleway --example scw -- destroy <instance-id>
//! cargo run -p kg-provider-scaleway --example scw -- self-destruct <instance-id>
//! cargo run -p kg-provider-scaleway --example scw -- sweep

use std::env;

use kg_provider::{InstanceId, LaunchSpec, MachineTypeId, Provider, VolumeId};
use kg_provider_scaleway::{Config, Scaleway};
use secrecy::SecretString;

const TAG: &str = "kyber-gaming";

fn var(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let provider = Scaleway::new(Config {
        access_key: var("SCW_ACCESS_KEY"),
        secret_key: SecretString::from(var("SCW_SECRET_KEY")),
        project_id: var("SCW_DEFAULT_PROJECT_ID"),
        zone: env::var("SCW_DEFAULT_ZONE").unwrap_or_else(|_| "fr-par-2".into()),
    });
    let args: Vec<String> = env::args().skip(1).collect();

    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["gpus"] => {
            for t in provider.machine_types().await? {
                if let Some(gpu) = t.gpu {
                    let price = t.hourly_price_eur.map(|p| format!("{p:.2} €/h")).unwrap_or_default();
                    println!("{:<14} {}x {} {}GB  {} vCPU  {} GB  {price}", t.id.0, gpu.count, gpu.model, gpu.vram_gb, t.vcpus, t.ram_gb);
                }
            }
        }
        ["list"] => {
            for i in provider.list_tagged(TAG).await? {
                println!("{}  {:?}  {:?}  {}", i.id, i.state, i.public_ip, i.name);
            }
        }
        ["launch", machine_type, key_file, cache @ ..] => {
            let instance = provider
                .launch(&LaunchSpec {
                    name: "kg-check".into(),
                    machine_type: MachineTypeId(machine_type.to_string()),
                    root_disk_gb: 80,
                    ssh_public_key: std::fs::read_to_string(key_file)?,
                    tags: vec![TAG.into()],
                    cache_volume: cache.first().map(|v| VolumeId(v.to_string())),
                })
                .await?;
            println!("{instance:#?}");
        }
        ["destroy", id] => {
            provider.destroy(&InstanceId(id.to_string())).await?;
            println!("destroyed {id}");
        }
        ["self-destruct", id] => {
            provider.self_destruct(&InstanceId(id.to_string())).await?;
            println!("self destruct requested for {id}");
        }
        ["sweep"] => println!("swept {} leftover(s)", provider.sweep(TAG).await?),
        _ => eprintln!("usage: scw gpus | list | launch <machine-type> <ssh-key.pub> [cache] | destroy <id> | self-destruct <id> | sweep"),
    }
    Ok(())
}
