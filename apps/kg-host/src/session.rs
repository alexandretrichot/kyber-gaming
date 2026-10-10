use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use futures::stream::{BoxStream, StreamExt};
use futures::{SinkExt, channel::mpsc};
use kg_protocol::{AgentToHost, Provision, StepState};
use kg_provider::{InstanceId, InstanceState, LaunchSpec, MachineTypeId, Provider};

use crate::agent::Agent;

/// Every instance the host creates carries this tag, which is how leftovers are found again.
pub const TAG: &str = "kyber-gaming";
const BOOT_TIMEOUT: Duration = Duration::from_secs(600);

pub struct Plan {
    pub machine_type: MachineTypeId,
    pub ssh_public_key: String,
    pub provision: Provision,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SessionEvent {
    Cleaned { leftovers: usize },
    Launched { instance: InstanceId },
    Booted { ip: IpAddr },
    Step { name: String, state: StepState },
    Ready { kyber: SocketAddr },
    Failed(String),
}

pub fn start(provider: Arc<dyn Provider>, agent: Arc<dyn Agent>, plan: Plan, poll: Duration) -> BoxStream<'static, SessionEvent> {
    let (mut tx, rx) = mpsc::channel(32);
    tokio::spawn(async move {
        if let Err(e) = run(provider.as_ref(), agent.as_ref(), plan, poll, &mut tx).await {
            let _ = tx.send(SessionEvent::Failed(format!("{e:#}"))).await;
        }
    });
    rx.boxed()
}

async fn run(
    provider: &dyn Provider,
    agent: &dyn Agent,
    plan: Plan,
    poll: Duration,
    tx: &mut mpsc::Sender<SessionEvent>,
) -> anyhow::Result<()> {
    let leftovers = reconcile(provider).await?;
    tx.send(SessionEvent::Cleaned { leftovers }).await?;

    let instance = provider
        .launch(&LaunchSpec {
            name: "kyber-gaming".into(),
            machine_type: plan.machine_type,
            root_disk_gb: 80,
            ssh_public_key: plan.ssh_public_key,
            tags: vec![TAG.into()],
            cache_volume: None,
        })
        .await?;
    tx.send(SessionEvent::Launched { instance: instance.id.clone() }).await?;

    let ip = wait_running(provider, &instance.id, poll).await?;
    tx.send(SessionEvent::Booted { ip }).await?;

    let mut messages = agent.provision(ip, instance.id.clone(), plan.provision);
    while let Some(message) = messages.next().await {
        match message? {
            AgentToHost::Progress { step, state } => {
                let failed = matches!(state, StepState::Failed { .. });
                tx.send(SessionEvent::Step { name: step.clone(), state }).await?;
                if failed {
                    anyhow::bail!("{step} failed on the machine");
                }
            }
            AgentToHost::SessionReady { kyber_port } => {
                tx.send(SessionEvent::Ready { kyber: SocketAddr::new(ip, kyber_port) }).await?;
                return Ok(());
            }
            AgentToHost::Rejected { reason } => anyhow::bail!("agent refused the session: {reason}"),
            AgentToHost::Welcome { .. } | AgentToHost::SaveToHost { .. } => {}
        }
    }
    anyhow::bail!("lost the agent before the session was ready")
}

async fn wait_running(provider: &dyn Provider, id: &InstanceId, poll: Duration) -> anyhow::Result<IpAddr> {
    let deadline = tokio::time::Instant::now() + BOOT_TIMEOUT;
    loop {
        if let Some(instance) = provider.get(id).await? {
            if let (InstanceState::Running, Some(ip)) = (instance.state, instance.public_ip) {
                return Ok(ip);
            }
        } else {
            anyhow::bail!("instance {id} disappeared while booting");
        }
        anyhow::ensure!(tokio::time::Instant::now() < deadline, "instance {id} did not boot within {BOOT_TIMEOUT:?}");
        tokio::time::sleep(poll).await;
    }
}

/// Destroys tagged instances nobody is playing on and sweeps what self destructed ones left.
/// Only safe when this host has no session running.
pub async fn reconcile(provider: &dyn Provider) -> kg_provider::Result<usize> {
    let mut cleaned = 0;
    for orphan in provider.list_tagged(TAG).await? {
        provider.destroy(&orphan.id).await?;
        cleaned += 1;
    }
    Ok(cleaned + provider.sweep(TAG).await?)
}

pub async fn stop(provider: Arc<dyn Provider>, instance: InstanceId) -> kg_provider::Result<()> {
    provider.destroy(&instance).await?;
    provider.sweep(TAG).await.map(drop)
}

#[cfg(test)]
mod tests {
    use secrecy::SecretString;

    use super::*;
    use crate::demo::{DemoAgent, DemoProvider};
    use kg_protocol::Resolution;

    fn plan() -> Plan {
        Plan {
            machine_type: MachineTypeId("DEMO-L4".into()),
            ssh_public_key: "ssh-ed25519 AAAA test".into(),
            provision: Provision {
                runtime: "img".into(),
                kyber_password: SecretString::from("pw"),
                resolution: Resolution { width: 1920, height: 1080 },
            },
        }
    }

    #[tokio::test]
    async fn happy_path_reaches_ready_with_the_instance_ip() {
        let provider = Arc::new(DemoProvider::fast());
        let events: Vec<_> = start(provider.clone(), Arc::new(DemoAgent::fast()), plan(), Duration::from_millis(1))
            .collect()
            .await;

        assert_eq!(events.first(), Some(&SessionEvent::Cleaned { leftovers: 0 }));
        assert!(matches!(events.last(), Some(SessionEvent::Ready { kyber }) if kyber.port() == 8080));
        assert!(events.iter().any(|e| matches!(e, SessionEvent::Step { state: StepState::Done, .. })));
    }

    #[tokio::test]
    async fn failing_step_ends_the_session_with_an_error() {
        let events: Vec<_> = start(
            Arc::new(DemoProvider::fast()),
            Arc::new(DemoAgent::failing_at("session")),
            plan(),
            Duration::from_millis(1),
        )
        .collect()
        .await;

        assert!(matches!(events.last(), Some(SessionEvent::Failed(e)) if e.contains("session")));
    }

    #[tokio::test]
    async fn reconcile_destroys_orphans_before_launching() {
        let provider = Arc::new(DemoProvider::fast());
        provider.launch(&LaunchSpec {
            name: "orphan".into(),
            machine_type: MachineTypeId("DEMO-L4".into()),
            root_disk_gb: 80,
            ssh_public_key: String::new(),
            tags: vec![TAG.into()],
            cache_volume: None,
        })
        .await
        .unwrap();

        assert_eq!(reconcile(provider.as_ref()).await.unwrap(), 1);
        assert!(provider.list_tagged(TAG).await.unwrap().is_empty());
    }
}
