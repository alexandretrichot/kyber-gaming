use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use crate::provision::{Ctx, Step};
use kg_provider::{Instance, InstanceId, LaunchSpec, MachineType, Provider, ProviderAccess, ProviderFactory, Result};

#[derive(Default, Clone)]
pub struct NullProvider {
    pub self_destructs: Arc<AtomicUsize>,
}

#[async_trait]
impl Provider for NullProvider {
    fn kind(&self) -> &'static str {
        "null"
    }
    async fn machine_types(&self) -> Result<Vec<MachineType>> {
        Ok(vec![])
    }
    async fn launch(&self, _: &LaunchSpec) -> Result<Instance> {
        unimplemented!("the agent never launches")
    }
    async fn get(&self, _: &InstanceId) -> Result<Option<Instance>> {
        Ok(None)
    }
    async fn list_tagged(&self, _: &str) -> Result<Vec<Instance>> {
        Ok(vec![])
    }
    async fn destroy(&self, _: &InstanceId) -> Result<()> {
        Ok(())
    }
    async fn self_destruct(&self, _: &InstanceId) -> Result<()> {
        self.self_destructs.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn sweep(&self, _: &str) -> Result<usize> {
        Ok(0)
    }
}

pub struct NullFactory;

impl ProviderFactory for NullFactory {
    fn kind(&self) -> &'static str {
        "null"
    }
    fn connect(&self, _: &ProviderAccess) -> Result<Box<dyn Provider>> {
        Ok(Box::new(NullProvider::default()))
    }
}

pub struct FakeStep {
    name: &'static str,
    done: bool,
    fails: bool,
}

impl FakeStep {
    pub fn ok(name: &'static str) -> Box<dyn Step> {
        Box::new(Self { name, done: false, fails: false })
    }
    pub fn done(name: &'static str) -> Box<dyn Step> {
        Box::new(Self { name, done: true, fails: false })
    }
    pub fn failing(name: &'static str) -> Box<dyn Step> {
        Box::new(Self { name, done: false, fails: true })
    }
}

#[async_trait]
impl Step for FakeStep {
    fn name(&self) -> &'static str {
        self.name
    }
    async fn is_done(&self, _: &Ctx) -> anyhow::Result<bool> {
        Ok(self.done)
    }
    async fn run(&self, _: &Ctx) -> anyhow::Result<()> {
        if self.fails {
            anyhow::bail!("boom");
        }
        Ok(())
    }
}
