//! OCI images as a distribution format only: layers are pulled, extracted once per digest and stacked
//! read-only on `/opt/kg`. Nothing ever runs as a container.
//!
//! Contract for images: built `FROM scratch`, additive (no whiteouts), and every file under `opt/kg/`.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};
use oci_client::client::ClientConfig;
use oci_client::manifest::{
    IMAGE_DOCKER_LAYER_GZIP_MEDIA_TYPE, IMAGE_DOCKER_LAYER_TAR_MEDIA_TYPE, IMAGE_LAYER_GZIP_MEDIA_TYPE,
    IMAGE_LAYER_MEDIA_TYPE, OciDescriptor,
};
use oci_client::secrets::RegistryAuth;
use oci_client::{Client, Reference};
use tracing::info;

const PREFIX: &str = "opt/kg";

pub struct Resolved {
    /// Digest of the image manifest the tag pointed to.
    pub digest: String,
    /// The `opt/kg` directory of each layer, bottom layer first.
    pub layers: Vec<PathBuf>,
}

pub struct LayerStore {
    client: Client,
    root: PathBuf,
}

impl LayerStore {
    pub fn new(root: PathBuf) -> Self {
        Self { client: Client::new(ClientConfig::default()), root }
    }

    pub async fn resolve(&self, image: &str) -> Result<String> {
        let reference: Reference = image.parse().with_context(|| format!("invalid image reference {image}"))?;
        let (_, digest) = self.client.pull_image_manifest(&reference, &RegistryAuth::Anonymous).await?;
        Ok(digest)
    }

    /// Pulls the layers not extracted yet and returns all of them in stacking order.
    pub async fn ensure(&self, image: &str) -> Result<Resolved> {
        let reference: Reference = image.parse().with_context(|| format!("invalid image reference {image}"))?;
        let (manifest, digest) = self.client.pull_image_manifest(&reference, &RegistryAuth::Anonymous).await?;
        tokio::fs::create_dir_all(&self.root).await?;

        let mut layers = Vec::with_capacity(manifest.layers.len());
        for layer in &manifest.layers {
            layers.push(self.layer(&reference, layer).await?.join(PREFIX));
        }
        Ok(Resolved { digest, layers })
    }

    async fn layer(&self, reference: &Reference, layer: &OciDescriptor) -> Result<PathBuf> {
        let name = layer.digest.replace(':', "-");
        let dir = self.root.join(&name);
        if tokio::fs::try_exists(&dir).await? {
            return Ok(dir);
        }

        let partial = self.root.join(format!(".partial-{name}"));
        let _ = tokio::fs::remove_dir_all(&partial).await;
        tokio::fs::create_dir_all(&partial).await?;
        let blob = partial.join("blob");
        info!(digest = %layer.digest, size = layer.size, "pulling layer");
        let file = tokio::fs::File::create(&blob).await?;
        self.client.pull_blob(reference, layer, file).await.with_context(|| format!("pulling {}", layer.digest))?;

        let tree = partial.join("tree");
        let media_type = layer.media_type.clone();
        let (blob_path, tree_path) = (blob.clone(), tree.clone());
        tokio::task::spawn_blocking(move || extract(&blob_path, &media_type, &tree_path)).await??;
        tokio::fs::remove_file(&blob).await?;
        tokio::fs::rename(&tree, &dir).await?;
        tokio::fs::remove_dir_all(&partial).await?;
        Ok(dir)
    }
}

fn extract(blob: &Path, media_type: &str, into: &Path) -> Result<()> {
    let file = BufReader::new(File::open(blob)?);
    let reader: Box<dyn Read> = match media_type {
        IMAGE_LAYER_GZIP_MEDIA_TYPE | IMAGE_DOCKER_LAYER_GZIP_MEDIA_TYPE => Box::new(flate2::read::GzDecoder::new(file)),
        IMAGE_LAYER_MEDIA_TYPE | IMAGE_DOCKER_LAYER_TAR_MEDIA_TYPE => Box::new(file),
        other => bail!("unsupported layer media type {other}"),
    };
    std::fs::create_dir_all(into)?;
    let mut archive = tar::Archive::new(reader);
    archive.set_preserve_permissions(true);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        check_entry(&path)?;
        ensure!(entry.unpack_in(into)?, "{} escapes the layer", path.display());
    }
    Ok(())
}

fn check_entry(path: &Path) -> Result<()> {
    let parts: Vec<_> = path.components().filter(|c| !matches!(c, Component::CurDir)).collect();
    if parts.iter().any(|c| c.as_os_str().to_string_lossy().starts_with(".wh.")) {
        bail!("{} is a whiteout, runtime layers must be additive", path.display());
    }
    let inside = match parts.as_slice() {
        [Component::Normal(opt)] => *opt == "opt",
        [Component::Normal(opt), Component::Normal(kg), ..] => *opt == "opt" && *kg == "kg",
        _ => false,
    };
    ensure!(inside, "{} is outside /{PREFIX}", path.display());
    Ok(())
}

/// Stacks the layers read-only on `target`, replacing whatever was mounted there.
pub fn mount(target: &Path, layers: &[PathBuf]) -> Result<()> {
    ensure!(!layers.is_empty(), "the runtime image has no layer");
    std::fs::create_dir_all(target)?;
    if is_mountpoint(target) {
        // Lazy: processes still using the old runtime keep it until they are restarted.
        run(Command::new("umount").arg("--lazy").arg(target))?;
    }
    match layers {
        [single] => {
            run(Command::new("mount").arg("--bind").arg(single).arg(target))?;
            run(Command::new("mount").args(["-o", "remount,ro,bind"]).arg(target))
        }
        _ => {
            let lowerdir = layers.iter().rev().map(|l| l.display().to_string()).collect::<Vec<_>>().join(":");
            run(Command::new("mount").args(["-t", "overlay", "overlay", "-o"]).arg(format!("lowerdir={lowerdir}")).arg(target))
        }
    }
}

pub fn is_mountpoint(path: &Path) -> bool {
    let mounts = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
    let path = path.to_string_lossy();
    mounts.lines().any(|l| l.split_whitespace().nth(1) == Some(&path))
}

fn run(command: &mut Command) -> Result<()> {
    let output = command.output().with_context(|| format!("running {command:?}"))?;
    ensure!(output.status.success(), "{command:?} failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    Ok(())
}

#[cfg(test)]
impl LayerStore {
    fn insecure(root: PathBuf) -> Self {
        let config = ClientConfig { protocol: oci_client::client::ClientProtocol::Http, ..Default::default() };
        Self { client: Client::new(config), root }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use flate2::Compression;
    use flate2::write::GzEncoder;
    use serde_json::json;
    use sha2::{Digest, Sha256};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn layer(files: &[(&str, &str)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
        for (path, content) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, path, content.as_bytes()).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    fn digest(bytes: &[u8]) -> String {
        format!("sha256:{}", Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect::<String>())
    }

    /// Serves `test/runtime:latest` with the given layers, each blob expected `pulls` times.
    async fn registry(layers: &[Vec<u8>], pulls: u64) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/v2/")).respond_with(ResponseTemplate::new(200)).mount(&server).await;
        let config = b"{}".to_vec();
        let descriptors: Vec<_> = layers
            .iter()
            .map(|l| json!({ "mediaType": IMAGE_LAYER_GZIP_MEDIA_TYPE, "digest": digest(l), "size": l.len() }))
            .collect();
        let manifest = json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "config": { "mediaType": "application/vnd.oci.image.config.v1+json", "digest": digest(&config), "size": config.len() },
            "layers": descriptors,
        })
        .to_string();
        Mock::given(method("GET"))
            .and(path("/v2/test/runtime/manifests/latest"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Type", "application/vnd.oci.image.manifest.v1+json")
                    .insert_header("Docker-Content-Digest", digest(manifest.as_bytes()))
                    .set_body_string(manifest.clone()),
            )
            .mount(&server)
            .await;
        for l in layers {
            Mock::given(method("GET"))
                .and(path(format!("/v2/test/runtime/blobs/{}", digest(l))))
                .respond_with(ResponseTemplate::new(200).set_body_bytes(l.clone()))
                .expect(pulls)
                .mount(&server)
                .await;
        }
        server
    }

    fn image(server: &MockServer) -> String {
        format!("{}/test/runtime:latest", server.address())
    }

    #[tokio::test]
    async fn layers_are_extracted_once_and_returned_bottom_first() {
        let base = layer(&[("opt/kg/kyber/bin/kycontroller", "#!/bin/sh\n")]);
        let top = layer(&[("./opt/kg/dock/kg-dock", "dock")]);
        let server = registry(&[base, top], 1).await;
        let dir = tempfile::tempdir().unwrap();
        let store = LayerStore::insecure(dir.path().join("layers"));

        let first = store.ensure(&image(&server)).await.unwrap();
        let again = store.ensure(&image(&server)).await.unwrap();

        assert_eq!(first.layers, again.layers);
        assert!(first.layers[0].join("kyber/bin/kycontroller").is_file());
        assert!(first.layers[1].join("dock/kg-dock").is_file());
        assert!(first.digest.starts_with("sha256:"));
        assert!(std::fs::read_dir(dir.path().join("layers")).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().starts_with(".partial")));
    }

    #[test]
    fn files_outside_opt_kg_are_refused() {
        assert!(check_entry(Path::new("opt/kg/kyber/bin/x")).is_ok());
        assert!(check_entry(Path::new("./opt/")).is_ok());
        assert!(check_entry(Path::new("etc/passwd")).is_err());
        assert!(check_entry(Path::new("opt/other/x")).is_err());
        assert!(check_entry(Path::new("opt/kg/.wh.kyber")).is_err());
    }

    #[test]
    fn a_layer_writing_elsewhere_fails_to_extract() {
        let dir = tempfile::tempdir().unwrap();
        let blob = dir.path().join("blob");
        std::fs::File::create(&blob).unwrap().write_all(&layer(&[("etc/cron.d/evil", "* * * * * root sh")])).unwrap();

        let error = extract(&blob, IMAGE_LAYER_GZIP_MEDIA_TYPE, &dir.path().join("tree")).unwrap_err();

        assert!(error.to_string().contains("outside"), "{error}");
        assert!(!dir.path().join("tree/etc").exists());
    }
}
