//! Plays the host side for a while, then goes silent so the agent's watchdog kicks in.
//!
//! ssh -N -L /tmp/kg-host.sock:/run/kg-agent/host.sock root@<ip> &
//! cargo run -p kg-agent --example fake_host -- /tmp/kg-host.sock <instance-id> <seconds> [provision]

use std::env;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use kg_protocol::{HostCodec, HostToAgent, Provision, Resolution, VERSION};
use kg_provider::InstanceId;
use kg_provider_scaleway::Config;
use secrecy::SecretString;
use tokio::net::UnixStream;
use tokio::time::{Instant, interval};
use tokio_util::codec::Framed;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    let [socket, instance, seconds] = &args[..3] else { panic!("socket instance seconds [provision]") };
    let access = Config {
        access_key: env::var("SCW_ACCESS_KEY")?,
        secret_key: SecretString::from(env::var("SCW_SECRET_KEY")?),
        project_id: env::var("SCW_DEFAULT_PROJECT_ID")?,
        zone: env::var("SCW_DEFAULT_ZONE").unwrap_or_else(|_| "fr-par-2".into()),
    }
    .to_access();

    let mut agent = Framed::new(UnixStream::connect(socket).await?, HostCodec::default());
    agent.send(HostToAgent::Hello { version: VERSION, instance: InstanceId(instance.clone()), provider: access }).await?;
    if args.get(3).map(String::as_str) == Some("provision") {
        agent
            .send(HostToAgent::Provision(Provision {
                runtime: env::var("KG_RUNTIME").unwrap_or_default(),
                kyber_password: SecretString::from(env::var("KG_KYBER_PASSWORD").unwrap_or_else(|_| "fake-host".into())),
                resolution: Resolution { width: 1920, height: 1080 },
            }))
            .await?;
    }

    let deadline = Instant::now() + Duration::from_secs(seconds.parse()?);
    let mut heartbeat = interval(Duration::from_secs(5));
    loop {
        tokio::select! {
            () = tokio::time::sleep_until(deadline) => break,
            _ = heartbeat.tick() => agent.send(HostToAgent::Heartbeat).await?,
            message = agent.next() => match message {
                Some(message) => println!("{:?}", message?),
                None => break,
            },
        }
    }
    println!("going silent");
    Ok(())
}
