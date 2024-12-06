// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::common::cmd::{ManifestParams, OemFile};
use crate::common::vars::{IS_USERSPACE_VAR, LOCKED_VAR, MAX_DOWNLOAD_SIZE_VAR, REVISION_VAR};
use crate::file_resolver::FileResolver;
use crate::manifest::{from_in_tree, from_local_product_bundle, from_path, from_sdk};
use crate::util::Event;
use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use chrono::{Duration, Utc};
use errors::ffx_bail;
use ffx_fastboot_interface::fastboot_interface::{FastbootInterface, RebootEvent, UploadProgress};
use futures::prelude::*;
use futures::try_join;
use pbms::is_local_product_bundle;
use sdk::SdkVersion;
use sparse::build_sparse_files;
use std::path::PathBuf;
use tokio::sync::mpsc;
use tokio::sync::mpsc::{Receiver, Sender};

pub const MISSING_CREDENTIALS: &str =
    "The flash manifest is missing the credential files to unlock this device.\n\
     Please unlock the target and try again.";

pub mod cmd;
pub mod crypto;
pub mod fastboot;
pub mod vars;

pub trait Partition {
    fn name(&self) -> &str;
    fn file(&self) -> &str;
    fn variable(&self) -> Option<&str>;
    fn variable_value(&self) -> Option<&str>;
}

pub trait Product<P> {
    fn bootloader_partitions(&self) -> &Vec<P>;
    fn partitions(&self) -> &Vec<P>;
    fn oem_files(&self) -> &Vec<OemFile>;
}

#[async_trait(?Send)]
pub trait Flash {
    async fn flash<F, T>(
        &self,
        messenger: &Sender<Event>,
        file_resolver: &mut F,
        fastboot_interface: &mut T,
        cmd: ManifestParams,
    ) -> Result<()>
    where
        F: FileResolver + Sync,
        T: FastbootInterface;
}

#[async_trait(?Send)]
pub trait Unlock {
    async fn unlock<F, T>(
        &self,
        _messenger: &Sender<Event>,
        _file_resolver: &mut F,
        _fastboot_interface: &mut T,
    ) -> Result<()>
    where
        F: FileResolver + Sync,
        T: FastbootInterface,
    {
        ffx_bail!(
            "This manifest does not support unlocking target devices. \n\
        Please update to a newer version of manifest and try again."
        )
    }
}

#[async_trait(?Send)]
pub trait Boot {
    async fn boot<F, T>(
        &self,
        messenger: Sender<Event>,
        file_resolver: &mut F,
        slot: String,
        fastboot_interface: &mut T,
        cmd: ManifestParams,
    ) -> Result<()>
    where
        F: FileResolver + Sync,
        T: FastbootInterface;
}

pub const MISSING_PRODUCT: &str = "Manifest does not contain product";

const LOCK_COMMAND: &str = "vx-lock";

pub const UNLOCK_ERR: &str = "The product requires the target to be unlocked. \
                                     Please unlock target and try again.";

pub async fn stage_file<F: FileResolver + Sync, T: FastbootInterface>(
    prog_client: Sender<UploadProgress>,
    file_resolver: &mut F,
    resolve: bool,
    file: &str,
    fastboot_interface: &mut T,
) -> Result<()> {
    let file_to_upload = if resolve {
        file_resolver.get_file(file).await.context("reconciling file for upload")?
    } else {
        file.to_string()
    };
    tracing::debug!("Preparing to stage {}", file_to_upload);
    fastboot_interface
        .stage(&file_to_upload, prog_client)
        .await
        .map_err(|e| anyhow!(e))
        .map_err(|e| anyhow!("There was an error staging {}: {:?}", file_to_upload, e))?;
    Ok(())
}

#[tracing::instrument()]
async fn do_flash<F: FastbootInterface>(
    name: &str,
    messenger: &Sender<Event>,
    fastboot_interface: &mut F,
    file_to_upload: &str,
    timeout: Duration,
) -> Result<()> {
    let (prog_client, mut prog_server): (Sender<UploadProgress>, Receiver<UploadProgress>) =
        mpsc::channel(1);
    try_join!(
        fastboot_interface.flash(name, file_to_upload, prog_client, timeout).map_err(|e| anyhow!(
            "There was an error flashing \"{}\" - {}: {:?}",
            name,
            file_to_upload,
            e
        )),
        async {
            loop {
                match prog_server.recv().await {
                    Some(upload) => {
                        messenger.send(Event::Upload(upload)).await?;
                    }
                    None => {
                        messenger
                            .send(Event::FlashPartition { partition_name: name.to_string() })
                            .await?;
                        return Ok(());
                    }
                }
            }
        }
    )?;
    Ok(())
}

#[tracing::instrument()]
async fn flash_partition_sparse<F: FastbootInterface>(
    name: &str,
    messenger: &Sender<Event>,
    file_to_upload: &str,
    fastboot_interface: &mut F,
    max_download_size: u64,
    timeout: Duration,
) -> Result<()> {
    tracing::debug!("Preparing to flash {} in sparse mode", file_to_upload);

    let sparse_files = build_sparse_files(
        name,
        file_to_upload,
        std::env::temp_dir().as_path(),
        max_download_size,
    )?;
    for tmp_file_path in sparse_files {
        let tmp_file_name = tmp_file_path.to_str().unwrap();
        do_flash(name, messenger, fastboot_interface, tmp_file_name, timeout).await?;
    }

    Ok(())
}

#[tracing::instrument(skip(file_resolver))]
pub async fn flash_partition<F: FileResolver + Sync, T: FastbootInterface>(
    messenger: &Sender<Event>,
    file_resolver: &mut F,
    name: &str,
    file: &str,
    fastboot_interface: &mut T,
    min_timeout_secs: u64,
    flash_timeout_rate_mb_per_second: u64,
) -> Result<()> {
    let file_to_upload =
        file_resolver.get_file(file).await.context("reconciling file for upload")?;
    tracing::debug!("Preparing to upload {}", file_to_upload);

    // If the given file to flash is bigger than what the device can download
    // at once, we need to make a sparse image out of the given file
    let file_handle = async_fs::File::open(&file_to_upload)
        .await
        .map_err(|e| anyhow!("Got error trying to open file \"{}\": {}", file_to_upload, e))?;
    let file_size = file_handle
        .metadata()
        .await
        .map_err(|e| {
            anyhow!("Got error retrieving metadata for file \"{}\": {}", file_to_upload, e)
        })?
        .len();

    // Calculate the flashing timeout
    let min_timeout = min_timeout_secs;
    let timeout_rate = flash_timeout_rate_mb_per_second;
    let megabytes = (file_size / 1000000) as u64;
    let mut timeout = megabytes / timeout_rate;
    timeout = std::cmp::max(timeout, min_timeout);
    let timeout = Duration::seconds(timeout as i64);
    tracing::debug!("Estimated timeout: {}s for {}MB", timeout, megabytes);

    let max_download_size_var = fastboot_interface
        .get_var(MAX_DOWNLOAD_SIZE_VAR)
        .await
        .map_err(|e| anyhow!("Communication error with the device: {:?}", e))?;

    tracing::trace!("Got max download size from device: {}", max_download_size_var);
    let trimmed_max_download_size_var = max_download_size_var.trim_start_matches("0x");

    let max_download_size: u64 = u64::from_str_radix(trimmed_max_download_size_var, 16)
        .expect("Fastboot max download size var was not a valid u32");

    tracing::trace!("Device Max Download Size: {}", max_download_size);
    tracing::trace!("File size: {}", file_size);

    let start_time = Utc::now();

    if u64::from(max_download_size) < file_size {
        flash_partition_sparse(
            name,
            messenger,
            &file_to_upload,
            fastboot_interface,
            max_download_size,
            timeout,
        )
        .await?;
    } else {
        do_flash(name, messenger, fastboot_interface, &file_to_upload, timeout).await?;
    }
    messenger
        .send(Event::FlashPartitionFinished {
            partition_name: name.to_string(),
            duration: Utc::now().signed_duration_since(start_time),
        })
        .await?;
    Ok(())
}

pub async fn verify_hardware(
    revision: &String,
    fastboot_interface: &mut impl FastbootInterface,
) -> Result<()> {
    let rev = fastboot_interface
        .get_var(REVISION_VAR)
        .await
        .map_err(|e| anyhow!("Communication error with the device: {:?}", e))?;
    if let Some(r) = rev.split("-").next() {
        if r != *revision && rev != *revision {
            ffx_bail!(
                "Hardware mismatch! Trying to flash images built for {} but have {}",
                revision,
                r
            );
        }
    } else {
        ffx_bail!("Could not verify hardware revision of target device");
    }
    Ok(())
}

pub async fn verify_variable_value(
    var: &str,
    value: &str,
    fastboot_interface: &mut impl FastbootInterface,
) -> Result<bool> {
    tracing::debug!("Verifying value for variable {} equals {}", var, value);
    fastboot_interface
        .get_var(var)
        .await
        .map_err(|e| anyhow!("Communication error with the device: {:?}", e))
        .map(|res| res == value)
}

#[tracing::instrument(skip(messenger))]
pub async fn reboot_bootloader<F: FastbootInterface>(
    messenger: &Sender<Event>,
    fastboot_interface: &mut F,
) -> Result<()> {
    messenger.send(Event::RebootStarted).await?;
    let (reboot_client, mut reboot_server): (Sender<RebootEvent>, Receiver<RebootEvent>) =
        mpsc::channel(1);
    let start_time = Utc::now();
    try_join!(
        fastboot_interface.reboot_bootloader(reboot_client).map_err(|e| anyhow!(e)),
        async move {
            match reboot_server.recv().await {
                Some(RebootEvent::OnReboot) => {
                    return Ok(());
                }
                None => {
                    bail!("Did not receive reboot signal");
                }
            };
        }
    )?;

    let d = Utc::now().signed_duration_since(start_time);
    tracing::debug!("Reboot duration: {:.2}s", (d.num_milliseconds() / 1000));
    messenger.send(Event::Rebooted(d)).await?;
    Ok(())
}

pub async fn stage_oem_files<F: FileResolver + Sync, T: FastbootInterface>(
    messenger: &Sender<Event>,
    file_resolver: &mut F,
    resolve: bool,
    oem_files: &Vec<OemFile>,
    fastboot_interface: &mut T,
) -> Result<()> {
    for oem_file in oem_files {
        let (prog_client, mut prog_server) = mpsc::channel(1);
        try_join!(
            stage_file(prog_client, file_resolver, resolve, oem_file.file(), fastboot_interface),
            async {
                loop {
                    match prog_server.recv().await {
                        Some(e) => {
                            messenger.send(Event::Upload(e)).await?;
                        }
                        None => {
                            return Ok(());
                        }
                    }
                }
            }
        )?;

        messenger.send(Event::Oem { oem_command: oem_file.command().to_string() }).await?;
        fastboot_interface.oem(oem_file.command()).await.map_err(|_| {
            anyhow!("There was an error sending oem command \"{}\"", oem_file.command())
        })?;
    }
    Ok(())
}

pub async fn set_slot_a_active(fastboot_interface: &mut impl FastbootInterface) -> Result<()> {
    if fastboot_interface.erase("misc").await.is_err() {
        tracing::debug!("Could not erase misc partition");
    }
    fastboot_interface.set_active("a").await.map_err(|_| anyhow!("Could not set active slot"))
}

#[tracing::instrument(skip(file_resolver, partitions))]
pub async fn flash_partitions<F: FileResolver + Sync, P: Partition, T: FastbootInterface>(
    messenger: &Sender<Event>,
    file_resolver: &mut F,
    partitions: &Vec<P>,
    fastboot_interface: &mut T,
    flash_timeout_rate_mb_per_second: u64,
    min_timeout_secs: u64,
) -> Result<()> {
    for partition in partitions {
        match (partition.variable(), partition.variable_value()) {
            (Some(var), Some(value)) => {
                if verify_variable_value(var, value, fastboot_interface).await? {
                    flash_partition(
                        messenger,
                        file_resolver,
                        partition.name(),
                        partition.file(),
                        fastboot_interface,
                        flash_timeout_rate_mb_per_second,
                        min_timeout_secs,
                    )
                    .await?;
                }
            }
            _ => {
                flash_partition(
                    messenger,
                    file_resolver,
                    partition.name(),
                    partition.file(),
                    fastboot_interface,
                    flash_timeout_rate_mb_per_second,
                    min_timeout_secs,
                )
                .await?
            }
        }
    }
    Ok(())
}

#[tracing::instrument(skip(file_resolver, product, cmd))]
pub async fn flash<F, Part, P, T>(
    messenger: &Sender<Event>,
    file_resolver: &mut F,
    product: &P,
    fastboot_interface: &mut T,
    cmd: ManifestParams,
) -> Result<()>
where
    F: FileResolver + Sync,
    Part: Partition,
    P: Product<Part>,
    T: FastbootInterface,
{
    flash_bootloader(messenger, file_resolver, product, fastboot_interface, &cmd).await?;
    flash_product(messenger, file_resolver, product, fastboot_interface, &cmd).await
}

pub async fn is_userspace_fastboot(
    fastboot_interface: &mut impl FastbootInterface,
) -> Result<bool> {
    match fastboot_interface.get_var(IS_USERSPACE_VAR).await {
        Ok(rev) => Ok(rev == "yes"),
        _ => Ok(false),
    }
}

#[tracing::instrument(skip(file_resolver, cmd, product))]
pub async fn flash_bootloader<F, Part, P, T>(
    messenger: &Sender<Event>,
    file_resolver: &mut F,
    product: &P,
    fastboot_interface: &mut T,
    cmd: &ManifestParams,
) -> Result<()>
where
    F: FileResolver + Sync,
    Part: Partition,
    P: Product<Part>,
    T: FastbootInterface,
{
    flash_partitions(
        messenger,
        file_resolver,
        product.bootloader_partitions(),
        fastboot_interface,
        cmd.flash_min_timeout_seconds,
        cmd.flash_timeout_rate_mb_per_second,
    )
    .await?;
    if product.bootloader_partitions().len() > 0
        && !cmd.no_bootloader_reboot
        && !is_userspace_fastboot(fastboot_interface).await?
    {
        set_slot_a_active(fastboot_interface).await?;
        reboot_bootloader(messenger, fastboot_interface).await?;
    }
    Ok(())
}

#[tracing::instrument(skip(file_resolver, cmd, product))]
pub async fn flash_product<F, Part, P, T>(
    messenger: &Sender<Event>,
    file_resolver: &mut F,
    product: &P,
    fastboot_interface: &mut T,
    cmd: &ManifestParams,
) -> Result<()>
where
    F: FileResolver + Sync,
    Part: Partition,
    P: Product<Part>,
    T: FastbootInterface,
{
    flash_partitions(
        &messenger,
        file_resolver,
        product.partitions(),
        fastboot_interface,
        cmd.flash_min_timeout_seconds,
        cmd.flash_timeout_rate_mb_per_second,
    )
    .await?;
    if !cmd.no_bootloader_reboot && is_userspace_fastboot(fastboot_interface).await? {
        reboot_bootloader(messenger, fastboot_interface).await?;
    }
    stage_oem_files(messenger, file_resolver, false, &cmd.oem_stage, fastboot_interface).await?;
    stage_oem_files(messenger, file_resolver, true, product.oem_files(), fastboot_interface).await
}

#[tracing::instrument(skip(file_resolver, cmd, product))]
pub async fn flash_and_reboot<F, Part, P, T>(
    messenger: &Sender<Event>,
    file_resolver: &mut F,
    product: &P,
    fastboot_interface: &mut T,
    cmd: ManifestParams,
) -> Result<()>
where
    F: FileResolver + Sync,
    Part: Partition,
    P: Product<Part>,
    T: FastbootInterface,
{
    flash(messenger, file_resolver, product, fastboot_interface, cmd).await?;
    finish(fastboot_interface).await
}

pub async fn finish<F: FastbootInterface>(fastboot_interface: &mut F) -> Result<()> {
    set_slot_a_active(fastboot_interface).await?;
    fastboot_interface.continue_boot().await.map_err(|_| anyhow!("Could not reboot device"))?;
    Ok(())
}

pub async fn is_locked(fastboot_interface: &mut impl FastbootInterface) -> Result<bool> {
    verify_variable_value(LOCKED_VAR, "no", fastboot_interface).await.map(|l| !l)
}

pub async fn lock_device(fastboot_interface: &mut impl FastbootInterface) -> Result<()> {
    fastboot_interface.oem(LOCK_COMMAND).await.map_err(|_| anyhow!("Could not lock device"))
}

pub async fn from_manifest<C, F>(
    messenger: Sender<Event>,
    input: C,
    fastboot_interface: &mut F,
) -> Result<()>
where
    C: Into<ManifestParams>,
    F: FastbootInterface,
{
    let cmd: ManifestParams = input.into();
    match &cmd.manifest {
        Some(manifest) => {
            if !manifest.is_file() {
                ffx_bail!("Manifest \"{}\" is not a file.", manifest.display());
            }
            from_path(&messenger, manifest.to_path_buf(), fastboot_interface, cmd).await
        }
        None => {
            if let Some(path) = cmd.product_bundle.as_ref().filter(|s| is_local_product_bundle(s)) {
                from_local_product_bundle(
                    &messenger,
                    PathBuf::from(&*path),
                    fastboot_interface,
                    cmd,
                )
                .await
            } else {
                let sdk = ffx_config::global_env_context()
                    .context("loading global environment context")?
                    .get_sdk()
                    .await?;
                let mut path = sdk.get_path_prefix().to_path_buf();
                path.push("flash.json"); // Not actually used, placeholder value needed.
                match sdk.get_version() {
                    SdkVersion::InTree => from_in_tree(&messenger, fastboot_interface, cmd).await,
                    SdkVersion::Version(_) => from_sdk(&messenger, fastboot_interface, cmd).await,
                    _ => ffx_bail!("Unknown SDK type"),
                }
            }
        }
    }
}
