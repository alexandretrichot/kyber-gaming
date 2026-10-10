//! A pretend provider and agent (`KG_DEMO=1`) to work on the interface without spending anything.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::{self, BoxStream, StreamExt};
use kg_protocol::{AgentToHost, Provision, StepState};
use kg_provider::{
    Gpu, Instance, InstanceId, InstanceState, LaunchSpec, MachineType, MachineTypeId, Provider, Result,
};

use crate::agent::Agent;

const STEPS: [&str; 5] = ["cache-volume", "system-packages", "runtime", "session", "kyber"];

pub struct DemoProvider {
    delay: Duration,
    instances: Mutex<Vec<Instance>>,
}

impl DemoProvider {
    pub fn new() -> Self {
        Self { delay: Duration::from_millis(1500), instances: Mutex::default() }
    }

    #[cfg(test)]
    pub fn fast() -> Self {
        Self { delay: Duration::ZERO, instances: Mutex::default() }
    }
}

#[async_trait]
impl Provider for DemoProvider {
    fn kind(&self) -> &'static str {
        "demo"
    }

    async fn machine_types(&self) -> Result<Vec<MachineType>> {
        tokio::time::sleep(self.delay).await;
        let gpu = |model: &str, vram_gb| Some(Gpu { model: model.into(), count: 1, vram_gb });
        Ok(vec![
            MachineType { id: MachineTypeId("DEMO-L4".into()), gpu: gpu("L4", 24), vcpus: 8, ram_gb: 48, hourly_price_eur: Some(0.79) },
            MachineType { id: MachineTypeId("DEMO-L40S".into()), gpu: gpu("L40S", 48), vcpus: 8, ram_gb: 96, hourly_price_eur: Some(1.47) },
            MachineType { id: MachineTypeId("DEMO-CPU".into()), gpu: None, vcpus: 2, ram_gb: 8, hourly_price_eur: Some(0.07) },
        ])
    }

    async fn launch(&self, spec: &LaunchSpec) -> Result<Instance> {
        tokio::time::sleep(self.delay).await;
        let mut instances = self.instances.lock().unwrap();
        let instance = Instance {
            id: InstanceId(format!("demo-{}", instances.len() + 1)),
            name: spec.name.clone(),
            state: InstanceState::Starting,
            public_ip: Some(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10))),
            tags: spec.tags.clone(),
        };
        instances.push(instance.clone());
        Ok(instance)
    }

    async fn get(&self, id: &InstanceId) -> Result<Option<Instance>> {
        tokio::time::sleep(self.delay * 2).await;
        let mut instances = self.instances.lock().unwrap();
        let found = instances.iter_mut().find(|i| &i.id == id);
        Ok(found.map(|i| {
            i.state = InstanceState::Running;
            i.clone()
        }))
    }

    async fn list_tagged(&self, tag: &str) -> Result<Vec<Instance>> {
        let instances = self.instances.lock().unwrap();
        Ok(instances.iter().filter(|i| i.tags.iter().any(|t| t == tag)).cloned().collect())
    }

    async fn destroy(&self, id: &InstanceId) -> Result<()> {
        tokio::time::sleep(self.delay).await;
        self.instances.lock().unwrap().retain(|i| &i.id != id);
        Ok(())
    }

    async fn self_destruct(&self, id: &InstanceId) -> Result<()> {
        self.destroy(id).await
    }

    async fn sweep(&self, _: &str) -> Result<usize> {
        Ok(0)
    }
}

pub struct DemoAgent {
    delay: Duration,
    fails_at: Option<&'static str>,
}

impl DemoAgent {
    pub fn new() -> Self {
        Self { delay: Duration::from_millis(1200), fails_at: None }
    }

    #[cfg(test)]
    pub fn fast() -> Self {
        Self { delay: Duration::ZERO, fails_at: None }
    }

    #[cfg(test)]
    pub fn failing_at(step: &'static str) -> Self {
        Self { delay: Duration::ZERO, fails_at: Some(step) }
    }
}

impl Agent for DemoAgent {
    fn provision(&self, _: IpAddr, _: InstanceId, _: Provision) -> BoxStream<'static, anyhow::Result<AgentToHost>> {
        let delay = self.delay;
        let fails_at = self.fails_at;
        let progress = |step: &str, state| AgentToHost::Progress { step: step.into(), state };

        let mut messages = vec![AgentToHost::Welcome { version: kg_protocol::VERSION }];
        for step in STEPS {
            messages.push(progress(step, StepState::Running));
            if fails_at == Some(step) {
                messages.push(progress(step, StepState::Failed { error: format!("{step}: pretend failure") }));
                break;
            }
            messages.push(progress(step, StepState::Done));
        }
        if fails_at.is_none() {
            messages.push(AgentToHost::SessionReady { kyber_port: 8080 });
        }

        stream::iter(messages)
            .then(move |message| async move {
                if matches!(message, AgentToHost::Progress { state: StepState::Done, .. }) {
                    tokio::time::sleep(delay).await;
                }
                Ok(message)
            })
            .boxed()
    }
}
