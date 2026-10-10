use std::path::PathBuf;

use anyhow::Result;
use async_trait::async_trait;

use super::{Ctx, Step};
use crate::layers::{self, LayerStore};

/// Mounts the runtime image (Kyber, in-VM apps) read-only on `target`.
pub struct RuntimeStep {
    pub store: LayerStore,
    pub target: PathBuf,
    /// Records which manifest is mounted, so a moved tag remounts and an unchanged one is skipped.
    pub mounted: PathBuf,
}

#[async_trait]
impl Step for RuntimeStep {
    fn name(&self) -> &'static str {
        "runtime"
    }

    async fn is_done(&self, ctx: &Ctx) -> Result<bool> {
        if !layers::is_mountpoint(&self.target) {
            return Ok(false);
        }
        let Ok(mounted) = tokio::fs::read_to_string(&self.mounted).await else {
            return Ok(false);
        };
        Ok(mounted == self.store.resolve(&ctx.runtime).await?)
    }

    async fn run(&self, ctx: &Ctx) -> Result<()> {
        let resolved = self.store.ensure(&ctx.runtime).await?;
        layers::mount(&self.target, &resolved.layers)?;
        tokio::fs::create_dir_all(self.mounted.parent().expect("marker has a parent")).await?;
        tokio::fs::write(&self.mounted, resolved.digest).await?;
        Ok(())
    }
}
