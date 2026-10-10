//! Plays the host side for a while, then goes silent so the agent's watchdog kicks in.
//!
//! ssh -N -L /tmp/kg-host.sock:/run/kg-agent/host.sock root@<ip> &
//! cargo run -p kg-agent --example fake_host -- /tmp/kg-host.sock <instance-id> <seconds>

use std::env;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use kg_protocol::{HostCodec, HostToAgent, VERSION};
use kg_provider::InstanceId;
use kg_provider_scaleway::Config;
use secrecy::SecretString;
use tokio::net::UnixStream;
use tokio_util::codec::Framed;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let [socket, instance, seconds] = env::args().skip(1).collect::<Vec<_>>().try_into().expect("socket instance seconds");
    let access = Config {
        access_key: env::var("SCW_ACCESS_KEY")?,
        secret_key: SecretString::from(env::var("SCW_SECRET_KEY")?),
        project_id: env::var("SCW_DEFAULT_PROJECT_ID")?,
        zone: env::var("SCW_DEFAULT_ZONE").unwrap_or_else(|_| "fr-par-2".into()),
    }
    .to_access();

    let mut agent = Framed::new(UnixStream::connect(&socket).await?, HostCodec::default());
    agent.send(HostToAgent::Hello { version: VERSION, instance: InstanceId(instance), provider: access }).await?;
    println!("agent replied {:?}", agent.next().await);

    for _ in 0..seconds.parse::<u64>()? / 5 {
        tokio::time::sleep(Duration::from_secs(5)).await;
        agent.send(HostToAgent::Heartbeat).await?;
    }
    println!("going silent");
    Ok(())
}
