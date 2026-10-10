use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use tracing::{error, warn};

use crate::vault::Vault;

pub trait Liveness: Send + Sync {
    fn last_seen(&self) -> Option<Instant>;
}

/// Destroys the instance once nobody has been around for `idle_after`.
///
/// Without a grant the agent cannot reach the provider, and the host cleans up the instance itself.
pub struct Watchdog {
    pub idle_after: Duration,
    pub check_every: Duration,
    pub signals: Vec<Arc<dyn Liveness>>,
    pub vault: Arc<Vault>,
    pub started: Instant,
    /// Unmounts extra volumes before the provider detaches them.
    pub release: Box<dyn Fn() -> Result<()> + Send + Sync>,
}

impl Watchdog {
    pub async fn run(self) {
        loop {
            tokio::time::sleep(self.check_every).await;
            if let Err(e) = self.check(Instant::now()).await {
                error!("self destruct failed, retrying: {e:#}");
            }
        }
    }

    /// Returns whether self destruction was triggered.
    pub async fn check(&self, now: Instant) -> Result<bool> {
        let last_activity = self.signals.iter().filter_map(|s| s.last_seen()).max().unwrap_or(self.started);
        if now.duration_since(last_activity) < self.idle_after {
            return Ok(false);
        }
        let Some(grant) = self.vault.current().await else {
            return Ok(false);
        };
        warn!(instance = %grant.instance, idle = ?now.duration_since(last_activity), "idle for too long, self destructing");
        (self.release)()?;
        grant.provider.self_destruct(&grant.instance).await?;
        Ok(true)
    }
}

pub fn unmount_cache() -> Result<()> {
    use std::process::Command;

    use crate::paths::CACHE_MOUNT;

    let mounts = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
    if !mounts.lines().any(|l| l.split_whitespace().nth(1) == Some(CACHE_MOUNT)) {
        return Ok(());
    }
    let status = Command::new("umount").arg(CACHE_MOUNT).status()?;
    anyhow::ensure!(status.success(), "umount {CACHE_MOUNT} failed with {status}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::Ordering;

    use kg_provider::InstanceId;

    use super::*;
    use crate::testing::NullProvider;
    use crate::vault::Grant;

    struct Fixed(Mutex<Option<Instant>>);

    impl Liveness for Fixed {
        fn last_seen(&self) -> Option<Instant> {
            *self.0.lock().unwrap()
        }
    }

    const IDLE: Duration = Duration::from_secs(60);

    async fn watchdog(last_seen: Option<Instant>, started: Instant, granted: Option<NullProvider>) -> Watchdog {
        let vault = Arc::new(Vault::default());
        if let Some(provider) = granted {
            vault.store(Grant { instance: InstanceId("server-1".into()), provider: Box::new(provider) }).await;
        }
        Watchdog {
            idle_after: IDLE,
            check_every: Duration::from_secs(1),
            signals: vec![Arc::new(Fixed(Mutex::new(last_seen)))],
            vault,
            started,
            release: Box::new(|| Ok(())),
        }
    }

    #[tokio::test]
    async fn recent_activity_keeps_the_instance() {
        let now = Instant::now();
        let provider = NullProvider::default();
        let dog = watchdog(Some(now - IDLE / 2), now - IDLE * 10, Some(provider.clone())).await;

        assert!(!dog.check(now).await.unwrap());
        assert_eq!(provider.self_destructs.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn prolonged_silence_destroys_the_instance() {
        let now = Instant::now();
        let provider = NullProvider::default();
        let dog = watchdog(Some(now - IDLE * 2), now - IDLE * 10, Some(provider.clone())).await;

        assert!(dog.check(now).await.unwrap());
        assert_eq!(provider.self_destructs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn host_that_never_came_counts_from_agent_start() {
        let now = Instant::now();
        let provider = NullProvider::default();

        let young = watchdog(None, now - IDLE / 2, Some(provider.clone())).await;
        assert!(!young.check(now).await.unwrap());

        let old = watchdog(None, now - IDLE * 2, Some(provider.clone())).await;
        assert!(old.check(now).await.unwrap());
    }

    #[tokio::test]
    async fn without_grant_nothing_happens() {
        let now = Instant::now();
        let dog = watchdog(Some(now - IDLE * 2), now - IDLE * 10, None).await;

        assert!(!dog.check(now).await.unwrap());
    }

    #[tokio::test]
    async fn failed_release_aborts_before_touching_the_provider() {
        let now = Instant::now();
        let provider = NullProvider::default();
        let mut dog = watchdog(Some(now - IDLE * 2), now - IDLE * 10, Some(provider.clone())).await;
        dog.release = Box::new(|| anyhow::bail!("device busy"));

        assert!(dog.check(now).await.is_err());
        assert_eq!(provider.self_destructs.load(Ordering::SeqCst), 0);
    }
}
