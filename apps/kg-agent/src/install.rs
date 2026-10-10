use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::process::Command;

use crate::paths::UNIT_PATH;

pub async fn install(idle_after: Duration) -> Result<()> {
    let exe = std::env::current_exe().context("locating own executable")?;
    let unit = format!(
        "[Unit]\n\
         Description=kyber-gaming agent\n\
         Wants=network-online.target\n\
         After=network-online.target\n\
         \n\
         [Service]\n\
         ExecStart={} serve --idle-after-secs {}\n\
         Restart=always\n\
         RestartSec=2\n\
         \n\
         [Install]\n\
         WantedBy=multi-user.target\n",
        exe.display(),
        idle_after.as_secs(),
    );
    tokio::fs::write(UNIT_PATH, unit).await.with_context(|| format!("writing {UNIT_PATH}"))?;

    systemctl(&["daemon-reload"]).await?;
    systemctl(&["enable", "kg-agent.service"]).await?;
    // restart rather than start: a reinstall must pick up the freshly uploaded binary.
    systemctl(&["restart", "kg-agent.service"]).await
}

async fn systemctl(args: &[&str]) -> Result<()> {
    let status = Command::new("systemctl").args(args).status().await.context("running systemctl")?;
    if !status.success() {
        bail!("systemctl {} failed with {status}", args.join(" "));
    }
    Ok(())
}
