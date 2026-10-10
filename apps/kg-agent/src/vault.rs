use std::sync::Arc;

use kg_provider::{InstanceId, Provider};
use tokio::sync::RwLock;

/// Provider access handed over by the host. Lives in this process's memory only, never on disk.
pub struct Grant {
    pub instance: InstanceId,
    pub provider: Box<dyn Provider>,
}

#[derive(Default)]
pub struct Vault {
    grant: RwLock<Option<Arc<Grant>>>,
}

impl Vault {
    pub async fn store(&self, grant: Grant) {
        *self.grant.write().await = Some(Arc::new(grant));
    }

    pub async fn current(&self) -> Option<Arc<Grant>> {
        self.grant.read().await.clone()
    }
}
