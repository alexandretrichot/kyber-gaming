use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use futures::{SinkExt, StreamExt};
use kg_protocol::{AgentCodec, AgentToHost, HostToAgent, VERSION};
use kg_provider::Registry;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::UnixListener;
use tokio::time::timeout;
use tokio_util::codec::Framed;
use tracing::{info, warn};

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

        let first = timeout(HELLO_TIMEOUT, framed.next())
            .await
            .map_err(|_| anyhow!("no hello within {HELLO_TIMEOUT:?}"))?
            .ok_or_else(|| anyhow!("host closed before hello"))??;
        if !matches!(first, HostToAgent::Hello { .. }) {
            let reason = "first message must be hello".to_string();
            framed.send(AgentToHost::Rejected { reason: reason.clone() }).await?;
            return Err(anyhow!(reason));
        }
        self.handle(first, &mut framed).await?;

        while let Some(message) = framed.next().await {
            self.handle(message?, &mut framed).await?;
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
                    let reason = format!("protocol version {version} is not supported, agent speaks {VERSION}");
                    framed.send(AgentToHost::Rejected { reason: reason.clone() }).await?;
                    return Err(anyhow!(reason));
                }
                let provider = match self.registry.connect(&provider) {
                    Ok(provider) => provider,
                    Err(e) => {
                        framed.send(AgentToHost::Rejected { reason: e.to_string() }).await?;
                        return Err(e.into());
                    }
                };
                info!(%instance, provider = provider.kind(), "host connected");
                self.vault.store(Grant { instance, provider }).await;
                framed.send(AgentToHost::Welcome { version: VERSION }).await?;
            }
            HostToAgent::Heartbeat => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use futures::{SinkExt, StreamExt};
    use kg_protocol::{AgentToHost, HostCodec, HostToAgent, VERSION};
    use kg_provider::{InstanceId, ProviderAccess, Registry};
    use secrecy::SecretString;
    use tokio_util::codec::Framed;

    use super::*;
    use crate::testing::NullFactory;

    fn bridge() -> Bridge {
        Bridge {
            vault: Arc::default(),
            registry: Registry::default().with(NullFactory),
            activity: Arc::default(),
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
        let bridge = bridge();
        let (reply, session) = exchange(&bridge, hello(VERSION, "null")).await;

        assert_eq!(reply, Some(AgentToHost::Welcome { version: VERSION }));
        session.unwrap();
        assert_eq!(bridge.vault.current().await.unwrap().instance, InstanceId("server-1".into()));
        assert!(bridge.activity.last_seen().is_some());
    }

    #[tokio::test]
    async fn other_protocol_version_is_rejected_and_grants_nothing() {
        let bridge = bridge();
        let (reply, session) = exchange(&bridge, hello(VERSION + 1, "null")).await;

        assert!(matches!(reply, Some(AgentToHost::Rejected { .. })));
        assert!(session.is_err());
        assert!(bridge.vault.current().await.is_none());
    }

    #[tokio::test]
    async fn unknown_provider_is_rejected() {
        let bridge = bridge();
        let (reply, _) = exchange(&bridge, hello(VERSION, "nope")).await;

        assert!(matches!(reply, Some(AgentToHost::Rejected { reason }) if reason.contains("nope")));
        assert!(bridge.vault.current().await.is_none());
    }

    #[tokio::test]
    async fn heartbeat_before_hello_is_refused() {
        let bridge = bridge();
        let (reply, session) = exchange(&bridge, HostToAgent::Heartbeat).await;

        assert!(matches!(reply, Some(AgentToHost::Rejected { .. })));
        assert!(session.is_err());
        assert!(bridge.vault.current().await.is_none());
    }
}
