use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::Mutex;
use tracing::info;

use super::{Ctx, Step};

const TAIL_LINES: usize = 30;

type Env = fn(&Ctx) -> Vec<(&'static str, String)>;

/// A bash script embedded in the agent. It is considered done once it succeeded with the exact same
/// script and environment, so an agent upgrade or new parameters run it again.
pub struct ScriptStep {
    name: &'static str,
    script: &'static str,
    env: Env,
}

impl ScriptStep {
    pub fn new(name: &'static str, script: &'static str, env: Env) -> Self {
        Self { name, script, env }
    }

    fn fingerprint(&self, ctx: &Ctx) -> String {
        let mut hash = Sha256::new();
        hash.update(self.script);
        for (key, value) in (self.env)(ctx) {
            hash.update(format!("\0{key}={value}"));
        }
        hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }

    fn marker(&self, ctx: &Ctx) -> PathBuf {
        ctx.state_dir.join("done").join(self.name)
    }
}

#[async_trait]
impl Step for ScriptStep {
    fn name(&self) -> &'static str {
        self.name
    }

    async fn is_done(&self, ctx: &Ctx) -> Result<bool> {
        Ok(tokio::fs::read_to_string(self.marker(ctx)).await.is_ok_and(|m| m == self.fingerprint(ctx)))
    }

    async fn run(&self, ctx: &Ctx) -> Result<()> {
        let mut child = Command::new("bash")
            .arg("-s")
            .envs((self.env)(ctx))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("spawning bash")?;

        let mut stdin = child.stdin.take().expect("piped stdin");
        stdin.write_all(self.script.as_bytes()).await?;
        drop(stdin);

        let tail = Mutex::new(VecDeque::with_capacity(TAIL_LINES));
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        tokio::join!(self.follow(stdout, &tail), self.follow(stderr, &tail));
        let status = child.wait().await?;

        if !status.success() {
            let tail = tail.into_inner().into_iter().collect::<Vec<_>>().join("\n");
            bail!("{} exited with {status}:\n{tail}", self.name);
        }
        let marker = self.marker(ctx);
        tokio::fs::create_dir_all(marker.parent().expect("marker has a parent")).await?;
        tokio::fs::write(&marker, self.fingerprint(ctx)).await?;
        Ok(())
    }
}

impl ScriptStep {
    async fn follow(&self, output: impl AsyncRead + Unpin, tail: &Mutex<VecDeque<String>>) {
        let mut lines = BufReader::new(output).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            info!(step = self.name, "{line}");
            let mut tail = tail.lock().await;
            if tail.len() == TAIL_LINES {
                tail.pop_front();
            }
            tail.push_back(line);
        }
    }
}

#[cfg(test)]
mod tests {
    use kg_protocol::Resolution;

    use super::*;

    fn ctx(dir: &tempfile::TempDir, width: u32) -> Ctx {
        Ctx {
            runtime: String::new(),
            resolution: Resolution { width, height: 1080 },
            kyber_port: 8080,
            kyber_password_sha256: String::new(),
            state_dir: dir.path().into(),
        }
    }

    fn step(script: &'static str) -> ScriptStep {
        ScriptStep::new("test", script, |ctx| vec![("WIDTH", ctx.resolution.width.to_string())])
    }

    #[tokio::test]
    async fn success_marks_done_for_the_same_parameters_only() {
        let dir = tempfile::tempdir().unwrap();
        let step = step(r#"[ "$WIDTH" -gt 0 ]"#);

        assert!(!step.is_done(&ctx(&dir, 1920)).await.unwrap());
        step.run(&ctx(&dir, 1920)).await.unwrap();
        assert!(step.is_done(&ctx(&dir, 1920)).await.unwrap());
        assert!(!step.is_done(&ctx(&dir, 2560)).await.unwrap());
    }

    #[tokio::test]
    async fn failure_reports_the_output_tail_and_marks_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let step = step("echo starting; echo 'disk full' >&2; exit 3");

        let error = step.run(&ctx(&dir, 1920)).await.unwrap_err().to_string();

        assert!(error.contains("disk full") && error.contains("starting"), "{error}");
        assert!(!step.is_done(&ctx(&dir, 1920)).await.unwrap());
    }

    #[tokio::test]
    async fn changed_script_runs_again() {
        let dir = tempfile::tempdir().unwrap();
        step("true").run(&ctx(&dir, 1920)).await.unwrap();

        assert!(!step("true # v2").is_done(&ctx(&dir, 1920)).await.unwrap());
    }
}
