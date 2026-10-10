mod runtime;
mod script;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use kg_protocol::{AgentToHost, Provision, Resolution, StepState};
use secrecy::ExposeSecret;
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, broadcast};
use tracing::{error, info};

pub use runtime::RuntimeStep;
pub use script::ScriptStep;

use crate::layers::LayerStore;
use crate::paths::{CACHE_MOUNT, RUNTIME_MOUNT, STATE_DIR};

pub struct Ctx {
    pub runtime: String,
    pub resolution: Resolution,
    pub kyber_port: u16,
    pub kyber_password_sha256: String,
    pub state_dir: PathBuf,
}

#[async_trait]
pub trait Step: Send + Sync {
    fn name(&self) -> &'static str;
    async fn is_done(&self, ctx: &Ctx) -> Result<bool>;
    async fn run(&self, ctx: &Ctx) -> Result<()>;
}

pub fn default_steps() -> Vec<Box<dyn Step>> {
    vec![
        Box::new(ScriptStep::new("cache-volume", include_str!("scripts/cache-volume.sh"), |_| vec![])),
        Box::new(ScriptStep::new("system-packages", include_str!("scripts/system-packages.sh"), |_| vec![])),
        Box::new(RuntimeStep {
            store: LayerStore::new(PathBuf::from(CACHE_MOUNT).join("kg/layers")),
            target: PathBuf::from(RUNTIME_MOUNT),
            mounted: PathBuf::from(STATE_DIR).join("runtime.digest"),
        }),
        Box::new(ScriptStep::new("session", include_str!("scripts/session.sh"), |ctx| {
            vec![
                ("SCREEN_WIDTH", ctx.resolution.width.to_string()),
                ("SCREEN_HEIGHT", ctx.resolution.height.to_string()),
            ]
        })),
        Box::new(ScriptStep::new("kyber", include_str!("scripts/kyber.sh"), |ctx| {
            vec![
                ("KYBER_PORT", ctx.kyber_port.to_string()),
                ("KYBER_PASSWORD_SHA256", ctx.kyber_password_sha256.clone()),
            ]
        })),
    ]
}

/// Runs the steps in order and stops at the first failure. Every transition is broadcast to the host.
pub async fn run(steps: &[Box<dyn Step>], ctx: &Ctx, events: &broadcast::Sender<AgentToHost>) -> Result<()> {
    let report = |step: &dyn Step, state: StepState| {
        let _ = events.send(AgentToHost::Progress { step: step.name().into(), state });
    };
    for step in steps {
        if step.is_done(ctx).await? {
            report(step.as_ref(), StepState::Skipped);
            continue;
        }
        info!(step = step.name(), "running");
        report(step.as_ref(), StepState::Running);
        if let Err(e) = step.run(ctx).await {
            error!(step = step.name(), "failed: {e:#}");
            report(step.as_ref(), StepState::Failed { error: format!("{e:#}") });
            return Err(e);
        }
        report(step.as_ref(), StepState::Done);
    }
    Ok(())
}

/// Owns provisioning so that a host reconnecting mid-way gets the progress instead of starting a second run.
pub struct Provisioner {
    pub steps: Vec<Box<dyn Step>>,
    pub state_dir: PathBuf,
    pub kyber_port: u16,
    pub events: broadcast::Sender<AgentToHost>,
    running: Mutex<bool>,
}

impl Provisioner {
    pub fn new(steps: Vec<Box<dyn Step>>, state_dir: PathBuf, kyber_port: u16) -> Self {
        Self { steps, state_dir, kyber_port, events: broadcast::channel(64).0, running: Mutex::new(false) }
    }

    /// Returns false when a run is already in progress, whose events the caller will see anyway.
    pub async fn start(self: &Arc<Self>, request: Provision) -> bool {
        let mut running = self.running.lock().await;
        if *running {
            return false;
        }
        *running = true;

        let ctx = Ctx {
            runtime: request.runtime,
            resolution: request.resolution,
            kyber_port: self.kyber_port,
            kyber_password_sha256: sha256_hex(request.kyber_password.expose_secret()),
            state_dir: self.state_dir.clone(),
        };
        let this = self.clone();
        tokio::spawn(async move {
            if run(&this.steps, &ctx, &this.events).await.is_ok() {
                let _ = this.events.send(AgentToHost::SessionReady { kyber_port: this.kyber_port });
            }
            *this.running.lock().await = false;
        });
        true
    }
}

pub fn sha256_hex(input: &str) -> String {
    Sha256::digest(input.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeStep;

    fn ctx() -> Ctx {
        Ctx {
            runtime: "img".into(),
            resolution: Resolution { width: 1920, height: 1080 },
            kyber_port: 8080,
            kyber_password_sha256: String::new(),
            state_dir: PathBuf::from("/nonexistent"),
        }
    }

    fn drain(rx: &mut broadcast::Receiver<AgentToHost>) -> Vec<(String, StepState)> {
        std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|e| match e {
                AgentToHost::Progress { step, state } => Some((step, state)),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn skips_done_steps_runs_the_rest_in_order() {
        let (tx, mut rx) = broadcast::channel(16);
        let steps: Vec<Box<dyn Step>> = vec![FakeStep::done("a"), FakeStep::ok("b"), FakeStep::ok("c")];

        run(&steps, &ctx(), &tx).await.unwrap();

        let states: Vec<_> = drain(&mut rx).into_iter().map(|(s, st)| format!("{s}:{st:?}")).collect();
        assert_eq!(states, ["a:Skipped", "b:Running", "b:Done", "c:Running", "c:Done"]);
    }

    #[tokio::test]
    async fn stops_at_the_first_failure_and_reports_it() {
        let (tx, mut rx) = broadcast::channel(16);
        let steps: Vec<Box<dyn Step>> = vec![FakeStep::failing("a"), FakeStep::ok("b")];

        assert!(run(&steps, &ctx(), &tx).await.is_err());

        let events = drain(&mut rx);
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[1], (s, StepState::Failed { error }) if s == "a" && error.contains("boom")));
    }

    #[test]
    fn sha256_matches_kyber_expected_format() {
        assert_eq!(sha256_hex("pw"), "30c952fab122c3f9759f02a6d95c3758b246b4fee239957b2d4fee46e26170c4");
    }
}
