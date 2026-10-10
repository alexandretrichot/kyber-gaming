mod bridge;
mod install;
mod layers;
mod paths;
mod provision;
#[cfg(test)]
mod testing;
mod vault;
mod watchdog;

use std::fs::Permissions;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use kg_provider::Registry;
use tokio::net::UnixListener;
use tokio::signal::unix::{SignalKind, signal};
use tracing::info;

use crate::bridge::Bridge;
use crate::paths::{HOST_SOCKET, KYBER_PORT, SOCKET_DIR, STATE_DIR};
use crate::provision::Provisioner;
use crate::watchdog::Watchdog;

#[derive(Parser)]
#[command(version, about = "kyber-gaming agent, runs on the gaming VM")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Installs and (re)starts the systemd service running `serve`.
    Install {
        #[arg(long, default_value_t = 900)]
        idle_after_secs: u64,
    },
    Serve {
        #[arg(long, default_value_t = 900)]
        idle_after_secs: u64,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    match Cli::parse().command {
        Command::Install { idle_after_secs } => install::install(Duration::from_secs(idle_after_secs)).await,
        Command::Serve { idle_after_secs } => serve(Duration::from_secs(idle_after_secs)).await,
    }
}

async fn serve(idle_after: Duration) -> Result<()> {
    std::fs::create_dir_all(SOCKET_DIR).with_context(|| format!("creating {SOCKET_DIR}"))?;
    std::fs::set_permissions(SOCKET_DIR, Permissions::from_mode(0o700))?;
    let _ = std::fs::remove_file(HOST_SOCKET);
    let listener = UnixListener::bind(HOST_SOCKET).with_context(|| format!("binding {HOST_SOCKET}"))?;
    std::fs::set_permissions(HOST_SOCKET, Permissions::from_mode(0o600))?;

    let bridge = Arc::new(Bridge {
        vault: Arc::default(),
        registry: Registry::default().with(kg_provider_scaleway::Factory),
        activity: Arc::default(),
        provisioner: Arc::new(Provisioner::new(provision::default_steps(), STATE_DIR.into(), KYBER_PORT)),
    });
    let watchdog = Watchdog {
        idle_after,
        check_every: Duration::from_secs(15),
        signals: vec![bridge.activity.clone()],
        vault: bridge.vault.clone(),
        started: Instant::now(),
        release: Box::new(watchdog::unmount_cache),
    };

    info!(socket = HOST_SOCKET, ?idle_after, "agent ready");
    let mut terminate = signal(SignalKind::terminate())?;
    tokio::select! {
        result = bridge.serve(listener) => result,
        () = watchdog.run() => Ok(()),
        _ = terminate.recv() => Ok(()),
        _ = tokio::signal::ctrl_c() => Ok(()),
    }
}
