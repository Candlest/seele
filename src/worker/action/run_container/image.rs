use crate::shared::cond_group::CondGroup;
use crate::shared::oci_image::OciImage;
use crate::{conf, shared};
use anyhow::{anyhow, bail, Context};
use futures_util::FutureExt;
use once_cell::sync::Lazy;
use std::path::PathBuf;
use std::time::Duration;
use tokio::fs;
use tokio::process::Command;
use tokio::time::timeout;
use tracing::{debug, instrument};

static PREPARATION_TASKS: Lazy<CondGroup<OciImage, Result<String, String>>> =
    Lazy::new(|| CondGroup::new(|payload: &OciImage| prepare_image_impl(payload.clone()).boxed()));

pub async fn prepare_image(image: &OciImage) -> anyhow::Result<String> {
    PREPARATION_TASKS.run(image).await.map_err(|err| anyhow!(err))
}

#[instrument]
async fn prepare_image_impl(image: OciImage) -> Result<String, String> {
    pull_image(&image).await.map_err(|err| format!("Error pulling the image: {:#}", err))?;

    let path = unpack_image(&image)
        .await
        .map_err(|err| format!("Error unpacking the image: {:#}", err))?;
    Ok(path)
}

#[instrument]
async fn pull_image(image: &OciImage) -> anyhow::Result<()> {
    const PULL_TIMEOUT_SECOND: u64 = 180;

    let path = get_oci_image_path(image);
    // TODO: check the integrity
    if fs::metadata(&path).await.is_ok() {
        debug!(path = %path.display(), "The image directory already presents, skip pulling");
        return Ok(());
    }

    fs::create_dir_all(&path).await.context("Error creating the image directory")?;

    debug!(path = %path.display(), "Pulling the container image using skopeo");
    let output = timeout(
        Duration::from_secs(PULL_TIMEOUT_SECOND + 3),
        Command::new(&conf::CONFIG.skopeo_path)
            .args([
                "copy",
                &format!("docker://{}/{}:{}", image.registry, image.name, image.tag),
                &format!("oci:{}:{}", path.display(), image.tag),
                "--command-timeout",
                &format!("{}s", PULL_TIMEOUT_SECOND),
                "--retry-times",
                "3",
                "--quiet",
            ])
            .output(),
    )
    .await
    .context("Error executing the skopeo process")?
    .context("The skopeo process took too long to finish")?;
    if !output.status.success() {
        bail!(
            "The skopeo process failed with the following output: {}",
            shared::collect_output(&output)
        );
    }

    Ok(())
}

#[instrument]
async fn unpack_image(image: &OciImage) -> anyhow::Result<String> {
    const UNPACK_TIMEOUT_SECOND: u64 = 120;

    let image_path = get_oci_image_path(image);
    let unpacked_path = get_unpacked_image_path(image);
    // TODO: check the integrity
    if fs::metadata(&unpacked_path).await.is_err() {
        debug!(image = %image_path.display(), unpacked = %unpacked_path.display(), "The unpacked image directory does not exist, unpacking the image");

        fs::create_dir_all(&unpacked_path)
            .await
            .context("Error creating the unpacked image directory")?;

        let output = timeout(
            Duration::from_secs(UNPACK_TIMEOUT_SECOND),
            Command::new(&conf::CONFIG.umoci_path)
                .args([
                    "--log",
                    "error",
                    "unpack",
                    "--rootless",
                    "--image",
                    &format!("{}:{}", image_path.display(), image.tag),
                    &format!("{}", unpacked_path.display()),
                ])
                .output(),
        )
        .await
        .context("Error executing the umoci process")?
        .context("The umoci process took too long to finish")?;

        if !output.status.success() {
            bail!(
                "The umoci process failed with the following output: {}",
                shared::collect_output(&output)
            );
        }
    }

    unpacked_path
        .join("rootfs")
        .into_os_string()
        .into_string()
        .map_err(|_| anyhow!("Error resolving the rootfs path"))
}

#[inline]
pub fn get_image_path(image: &OciImage) -> PathBuf {
    conf::PATHS.images.join(&image.registry).join(escape_image_name(&image.name))
}

#[inline]
pub fn get_oci_image_path(image: &OciImage) -> PathBuf {
    get_image_path(image).join("oci")
}

#[inline]
pub fn get_unpacked_image_path(image: &OciImage) -> PathBuf {
    get_image_path(image).join("unpacked")
}

#[inline]
fn escape_image_name(name: &str) -> String {
    // https://docs.docker.com/registry/spec/api/#overview
    name.replace('/', "_")
}
