use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use futures::{SinkExt, StreamExt};
use kg_protocol::{AgentCodec, AgentToHost, HostToAgent, VERSION};
use kg_provider::Registry;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::UnixListener;
use tokio::sync::broadcast::error::RecvError;
use tokio::time::timeout;
use tokio_util::codec::Framed;
use tracing::{info, warn};

use crate::provision::Provisioner;
use crate::vault::{Grant, Vault};
use crate::watchdog::Liveness;

const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

/// Last time the host said anything, on any connection.
#[derive(Default)]
pub struct Activity(Mutex<Option<Instant>>);

impl Activity {
    pub fn touch(&self) {
        *self.0.lock().unwrap() = Some(Instant::now());
    }
}

impl Liveness for Activity {
    fn last_seen(&self) -> Option<Instant> {
        *self.0.lock().unwrap()
    }
}

pub struct Bridge {
    pub vault: Arc<Vault>,
    pub registry: Registry,
    pub activity: Arc<Activity>,
    pub provisioner: Arc<Provisioner>,
}

impl Bridge {
    pub async fn serve(self: Arc<Self>, listener: UnixListener) -> Result<()> {
        loop {
            let (stream, _) = listener.accept().await.context("accepting host connection")?;
            let bridge = self.clone();
            tokio::spawn(async move {
                if let Err(e) = bridge.session(stream).await {
                    warn!("host session ended: {e:#}");
                }
            });
        }
    }

    pub async fn session<S: AsyncRead + AsyncWrite + Unpin>(&self, stream: S) -> Result<()> {
        let mut framed = Framed::new(stream, AgentCodec::default());
        let mut events = self.provisioner.events.subscribe();

        let first = timeout(HELLO_TIMEOUT, framed.next())
            .await
            .map_err(|_| anyhow!("no hello within {HELLO_TIMEOUT:?}"))?
            .ok_or_else(|| anyhow!("host closed before hello"))??;
        let HostToAgent::Hello { .. } = first else {
            return reject(&mut framed, "first message must be hello".into()).await;
        };
        self.handle(first, &mut framed).await?;

        loop {
            tokio::select! {
                message = framed.next() => match message {
                    Some(message) => self.handle(message?, &mut framed).await?,
                    None => break,
                },
                event = events.recv() => match event {
                    Ok(event) => framed.send(event).await?,
                    Err(RecvError::Lagged(missed)) => warn!(missed, "host too slow, dropped progress events"),
                    Err(RecvError::Closed) => break,
                },
            }
        }
        info!("host disconnected");
        Ok(())
    }

    async fn handle<S: AsyncRead + AsyncWrite + Unpin>(
        &self,
        message: HostToAgent,
        framed: &mut Framed<S, AgentCodec>,
    ) -> Result<()> {
        self.activity.touch();
        match message {
            HostToAgent::Hello { version, instance, provider } => {
                if version != VERSION {
                    return reject(framed, format!("protocol version {version} is not supported, agent speaks {VERSION}")).await;
                }
                let provider = match self.registry.connect(&provider) {
                    Ok(provider) => provider,
                    Err(e) => return reject(framed, e.to_string()).await,
                };
                info!(%instance, provider = provider.kind(), "host connected");
                self.vault.store(Grant { instance, provider }).await;
                framed.send(AgentToHost::Welcome { version: VERSION }).await?;
            }
            HostToAgent::Heartbeat => {}
            HostToAgent::Provision(request) => {
                if !self.provisioner.start(request).await {
                    info!("provisioning already running, host will get its progress");
                }
            }
        }
        Ok(())
    }
}

async fn reject<S: AsyncRead + AsyncWrite + Unpin>(framed: &mut Framed<S, AgentCodec>, reason: String) -> Result<()> {
    framed.send(AgentToHost::Rejected { reason: reason.clone() }).await?;
    Err(anyhow!(reason))
}

#[cfg(test)]
mod tests {
    use futures::{SinkExt, StreamExt};
    use kg_protocol::{AgentToHost, HostCodec, HostToAgent, Provision, Resolution, StepState, VERSION};
    use kg_provider::{InstanceId, ProviderAccess, Registry};
    use secrecy::SecretString;
    use tokio_util::codec::Framed;

    use super::*;
    use crate::testing::{FakeStep, NullFactory};

    fn bridge(steps: Vec<Box<dyn crate::provision::Step>>) -> Bridge {
        Bridge {
            vault: Arc::default(),
            registry: Registry::default().with(NullFactory),
            activity: Arc::default(),
            provisioner: Arc::new(Provisioner::new(steps, "/nonexistent".into(), 8080)),
        }
    }

    fn hello(version: u32, kind: &str) -> HostToAgent {
        HostToAgent::Hello {
            version,
            instance: InstanceId("server-1".into()),
            provider: ProviderAccess { kind: kind.into(), config: SecretString::from("{}") },
        }
    }

    async fn exchange(bridge: &Bridge, first: HostToAgent) -> (Option<AgentToHost>, Result<()>) {
        let (host_end, agent_end) = tokio::io::duplex(4096);
        let mut host = Framed::new(host_end, HostCodec::default());
        host.send(first).await.unwrap();
        let (session, reply) = tokio::join!(bridge.session(agent_end), async {
            let reply = host.next().await.map(Result::unwrap);
            drop(host);
            reply
        });
        (reply, session)
    }

    #[tokio::test]
    async fn hello_stores_the_grant_and_is_welcomed() {
        let bridge = bridge(vec![]);
        let (reply, session) = exchange(&bridge, hello(VERSION, "null")).await;

        assert_eq!(reply, Some(AgentToHost::Welcome { version: VERSION }));
        session.unwrap();
        assert_eq!(bridge.vault.current().await.unwrap().instance, InstanceId("server-1".into()));
        assert!(bridge.activity.last_seen().is_some());
    }

    #[tokio::test]
    async fn other_protocol_version_is_rejected_and_grants_nothing() {
        let bridge = bridge(vec![]);
        let (reply, session) = exchange(&bridge, hello(VERSION + 1, "null")).await;

        assert!(matches!(reply, Some(AgentToHost::Rejected { .. })));
        assert!(session.is_err());
        assert!(bridge.vault.current().await.is_none());
    }

    #[tokio::test]
    async fn unknown_provider_is_rejected() {
        let bridge = bridge(vec![]);
        let (reply, _) = exchange(&bridge, hello(VERSION, "nope")).await;

        assert!(matches!(reply, Some(AgentToHost::Rejected { reason }) if reason.contains("nope")));
        assert!(bridge.vault.current().await.is_none());
    }

    #[tokio::test]
    async fn heartbeat_before_hello_is_refused() {
        let bridge = bridge(vec![]);
        let (reply, session) = exchange(&bridge, HostToAgent::Heartbeat).await;

        assert!(matches!(reply, Some(AgentToHost::Rejected { .. })));
        assert!(session.is_err());
        assert!(bridge.vault.current().await.is_none());
    }

    #[tokio::test]
    async fn provision_streams_progress_then_session_ready() {
        let bridge = bridge(vec![FakeStep::done("packages"), FakeStep::ok("kyber")]);
        let (host_end, agent_end) = tokio::io::duplex(4096);
        let mut host = Framed::new(host_end, HostCodec::default());

        let host_side = async {
            host.send(hello(VERSION, "null")).await.unwrap();
            host.send(HostToAgent::Provision(Provision {
                runtime: "img".into(),
                kyber_password: SecretString::from("pw"),
                resolution: Resolution { width: 1920, height: 1080 },
            }))
            .await
            .unwrap();
            let mut received = vec![];
            while let Some(Ok(message)) = host.next().await {
                let ready = matches!(message, AgentToHost::SessionReady { .. });
                received.push(message);
                if ready {
                    break;
                }
            }
            drop(host);
            received
        };
        let (session, received) = tokio::join!(bridge.session(agent_end), host_side);

        session.unwrap();
        let progress = |step: &str, state| AgentToHost::Progress { step: step.into(), state };
        assert_eq!(
            received,
            [
                AgentToHost::Welcome { version: VERSION },
                progress("packages", StepState::Skipped),
                progress("kyber", StepState::Running),
                progress("kyber", StepState::Done),
                AgentToHost::SessionReady { kyber_port: 8080 },
            ]
        );
    }
}
