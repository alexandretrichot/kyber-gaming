use std::net::IpAddr;

use futures::stream::{self, BoxStream, StreamExt};
use kg_protocol::{AgentToHost, Provision};
use kg_provider::InstanceId;

/// The host's side of the VM agent: bootstraps it on a fresh instance, hands over the provider access,
/// asks for provisioning and streams back everything the agent says.
pub trait Agent: Send + Sync {
    fn provision(&self, ip: IpAddr, instance: InstanceId, request: Provision) -> BoxStream<'static, anyhow::Result<AgentToHost>>;
}

/// Placeholder until the SSH bridge exists (step 4): the machine boots, then the session stops here.
pub struct NotYetBridged;

impl Agent for NotYetBridged {
    fn provision(&self, _: IpAddr, _: InstanceId, _: Provision) -> BoxStream<'static, anyhow::Result<AgentToHost>> {
        stream::once(async { Err(anyhow::anyhow!("the SSH bridge to the agent is not implemented yet")) }).boxed()
    }
}
