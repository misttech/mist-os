// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::common::cmd::{ManifestParams, OemFile};
use crate::common::vars::{IS_USERSPACE_VAR, LOCKED_VAR, MAX_DOWNLOAD_SIZE_VAR, REVISION_VAR};
use crate::file_resolver::FileResolver;
use crate::manifest::{from_in_tree, from_local_product_bundle, from_path, from_sdk};
use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use errors::ffx_bail;
use ffx_fastboot_interface::fastboot_interface::{FastbootInterface, RebootEvent, UploadProgress};
use futures::prelude::*;
use futures::try_join;
use pbms::is_local_product_bundle;
use sdk::SdkVersion;
use sparse::build_sparse_files;
use std::io::Write;
use std::path::PathBuf;
use termion::{color, style};
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
    async fn flash<W, F, T>(
        &self,
        writer: &mut W,
        file_resolver: &mut F,
        fastboot_interface: &mut T,
        cmd: ManifestParams,
    ) -> Result<()>
    where
        W: Write,
        F: FileResolver + Sync,
        T: FastbootInterface;
}

#[async_trait(?Send)]
pub trait Unlock {
    async fn unlock<W, F, T>(
        &self,
        _writer: &mut W,
        _file_resolver: &mut F,
        _fastboot_interface: &mut T,
    ) -> Result<()>
    where
        W: Write,
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
    async fn boot<W, F, T>(
        &self,
        writer: &mut W,
        file_resolver: &mut F,
        slot: String,
        fastboot_interface: &mut T,
        cmd: ManifestParams,
    ) -> Result<()>
    where
        W: Write,
        F: FileResolver + Sync,
        T: FastbootInterface;
}

pub const MISSING_PRODUCT: &str = "Manifest does not contain product";

const LARGE_FILE: &str = "large file, please wait... ";
const LOCK_COMMAND: &str = "vx-lock";

pub const UNLOCK_ERR: &str = "The product requires the target to be unlocked. \
                                     Please unlock target and try again.";

pub fn done_time<W: Write>(writer: &mut W, duration: Duration) -> std::io::Result<()> {
    writeln!(
        writer,
        "{}Done{} [{}{:.2}s{}]",
        color::Fg(color::Green),
        style::Reset,
        color::Fg(color::Blue),
        (duration.num_milliseconds() as f32) / (1000 as f32),
        style::Reset
    )?;
    writer.flush()
}

async fn handle_upload_progress_for_upload<W: Write>(
    writer: &mut W,
    mut prog_server: Receiver<UploadProgress>,
    mut on_large: impl FnMut() -> (),
    mut on_finished: impl FnMut(&mut W) -> Result<()>,
) -> Result<Option<DateTime<Utc>>> {
    let mut start_time: Option<DateTime<Utc>> = None;
    let mut finish_time: Option<DateTime<Utc>> = None;
    loop {
        match prog_server.recv().await {
            Some(UploadProgress::OnStarted { size, .. }) => {
                start_time.replace(Utc::now());
                tracing::debug!("Upload started: {}", size);
                write!(writer, "Uploading... ")?;
                if size > (1 << 24) {
                    on_large();
                    write!(writer, "{}", LARGE_FILE)?;
                }
                writer.flush()?;
            }
            Some(UploadProgress::OnFinished { .. }) => {
                if let Some(st) = start_time {
                    let d = Utc::now().signed_duration_since(st);
                    tracing::debug!("Upload duration: {:.2}s", (d.num_milliseconds() / 1000));
                    done_time(writer, d)?;
                } else {
                    // Write done without the time .
                    writeln!(writer, "{}Done{}", color::Fg(color::Green), style::Reset)?;
                    writer.flush()?;
                }
                on_finished(writer)?;
                finish_time.replace(Utc::now());
                tracing::debug!("Upload finished");
            }
            Some(UploadProgress::OnError { error, .. }) => {
                tracing::error!("{}", error);
                ffx_bail!("{}", error)
            }
            Some(UploadProgress::OnProgress { bytes_written, .. }) => {
                tracing::trace!("Upload progress: {}", bytes_written);
            }
            None => return Ok(finish_time),
        }
    }
}

async fn handle_upload_progress_for_staging<W: Write>(
    writer: &mut W,
    prog_server: Receiver<UploadProgress>,
) -> Result<Option<DateTime<Utc>>> {
    handle_upload_progress_for_upload(writer, prog_server, move || {}, move |_writer| Ok(())).await
}

async fn handle_upload_progress_for_flashing<W: Write>(
    name: &str,
    writer: &mut W,
    prog_server: Receiver<UploadProgress>,
) -> Result<Option<DateTime<Utc>>> {
    // Using a boolean results in a warning that the variable is never read.
    let mut is_large: Option<()> = None;
    handle_upload_progress_for_upload(
        writer,
        prog_server,
        move || {
            is_large.replace(());
        },
        move |writer| {
            write!(writer, "Partitioning {}... ", name)?;
            if is_large.is_some() {
                write!(writer, "{}", LARGE_FILE)?;
            }
            writer.flush()?;
            Ok(())
        },
    )
    .await
}

pub async fn stage_file<W: Write, F: FileResolver + Sync, T: FastbootInterface>(
    writer: &mut W,
    file_resolver: &mut F,
    resolve: bool,
    file: &str,
    fastboot_interface: &mut T,
) -> Result<()> {
    let (prog_client, prog_server): (Sender<UploadProgress>, Receiver<UploadProgress>) =
        mpsc::channel(1);
    let file_to_upload = if resolve {
        file_resolver.get_file(writer, file).await.context("reconciling file for upload")?
    } else {
        file.to_string()
    };
    tracing::debug!("Preparing to stage {}", file_to_upload);
    try_join!(
        fastboot_interface.stage(&file_to_upload, prog_client).map_err(|e| anyhow!(e)),
        handle_upload_progress_for_staging(writer, prog_server),
    )
    .map_err(|e| anyhow!("There was an error staging {}: {:?}", file_to_upload, e))?;
    Ok(())
}

#[tracing::instrument(skip(writer))]
async fn do_flash<W: Write, F: FastbootInterface>(
    writer: &mut W,
    name: &str,
    fastboot_interface: &mut F,
    file_to_upload: &str,
    timeout: Duration,
) -> Result<()> {
    let (prog_client, prog_server): (Sender<UploadProgress>, Receiver<UploadProgress>) =
        mpsc::channel(1);
    try_join!(
        fastboot_interface.flash(name, file_to_upload, prog_client, timeout).map_err(|e| anyhow!(
            "There was an error flashing \"{}\" - {}: {:?}",
            name,
            file_to_upload,
            e
        )),
        handle_upload_progress_for_flashing(name, writer, prog_server),
    )
    .and_then(|(_, prog)| {
        if let Some(p) = prog {
            let d = Utc::now().signed_duration_since(p);
            tracing::debug!("Partition duration: {:.2}s", (d.num_milliseconds() / 1000));
            done_time(writer, d)?;
        } else {
            // Write a line break otherwise
            writeln!(writer, "{}Done{}", color::Fg(color::Green), style::Reset)?;
            writer.flush()?;
        }
        Ok(())
    })
}

#[tracing::instrument(skip(writer))]
async fn flash_partition_sparse<W: Write, F: FastbootInterface>(
    writer: &mut W,
    name: &str,
    file_to_upload: &str,
    fastboot_interface: &mut F,
    max_download_size: u64,
    timeout: Duration,
) -> Result<()> {
    tracing::debug!("Preparing to flash {} in sparse mode", file_to_upload);

    let sparse_files = build_sparse_files(
        writer,
        name,
        file_to_upload,
        std::env::temp_dir().as_path(),
        max_download_size,
    )?;
    for tmp_file_path in sparse_files {
        let tmp_file_name = tmp_file_path.to_str().unwrap();
        writeln!(writer, "For partition: {}, flashing sparse image file {}", name, tmp_file_name)?;

        do_flash(writer, name, fastboot_interface, tmp_file_name, timeout).await?;
    }

    Ok(())
}

#[tracing::instrument(skip(writer, file_resolver))]
pub async fn flash_partition<W: Write, F: FileResolver + Sync, T: FastbootInterface>(
    writer: &mut W,
    file_resolver: &mut F,
    name: &str,
    file: &str,
    fastboot_interface: &mut T,
    min_timeout_secs: u64,
    flash_timeout_rate_mb_per_second: u64,
) -> Result<()> {
    let file_to_upload =
        file_resolver.get_file(writer, file).await.context("reconciling file for upload")?;
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

    if u64::from(max_download_size) < file_size {
        writeln!(
            writer,
            "File size ({}) is bigger than device Max Download Size ({})... \
            Flashing image in Sparse mode",
            file_size, max_download_size
        )?;
        return flash_partition_sparse(
            writer,
            name,
            &file_to_upload,
            fastboot_interface,
            max_download_size,
            timeout,
        )
        .await;
    }
    do_flash(writer, name, fastboot_interface, &file_to_upload, timeout).await
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

#[tracing::instrument(skip(writer))]
pub async fn reboot_bootloader<W: Write, F: FastbootInterface>(
    writer: &mut W,
    fastboot_interface: &mut F,
) -> Result<()> {
    write!(writer, "Rebooting to bootloader... ")?;
    writer.flush()?;
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
    done_time(writer, d)?;
    Ok(())
}

pub async fn prepare<W: Write, F: FastbootInterface>(
    writer: &mut W,
    fastboot_interface: &mut F,
) -> Result<()> {
    let (reboot_client, mut reboot_server): (Sender<RebootEvent>, Receiver<RebootEvent>) =
        mpsc::channel(1);
    let mut start_time = None;
    writer.flush()?;

    try_join!(fastboot_interface.prepare(reboot_client).map_err(|e| anyhow!(e)), async {
        match reboot_server.recv().await {
            Some(RebootEvent::OnReboot) => {
                start_time.replace(Utc::now());
                write!(writer, "Rebooting to bootloader... ")?;
                writer.flush()?;
                return Ok(());
            }
            None => {
                return Ok(());
            }
        }
    })?;

    if let Some(s) = start_time {
        let d = Utc::now().signed_duration_since(s);
        tracing::debug!("Reboot duration: {:.2}s", (d.num_milliseconds() / 1000));
        done_time(writer, d)?;
    }
    Ok(())
}

pub async fn stage_oem_files<W: Write, F: FileResolver + Sync, T: FastbootInterface>(
    writer: &mut W,
    file_resolver: &mut F,
    resolve: bool,
    oem_files: &Vec<OemFile>,
    fastboot_interface: &mut T,
) -> Result<()> {
    for oem_file in oem_files {
        stage_file(writer, file_resolver, resolve, oem_file.file(), fastboot_interface).await?;
        writeln!(writer, "Sending command \"{}\"", oem_file.command())?;
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

#[tracing::instrument(skip(writer, file_resolver, partitions))]
pub async fn flash_partitions<
    W: Write,
    F: FileResolver + Sync,
    P: Partition,
    T: FastbootInterface,
>(
    writer: &mut W,
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
                        writer,
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
                    writer,
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

#[tracing::instrument(skip(writer, file_resolver, product, cmd))]
pub async fn flash<W, F, Part, P, T>(
    writer: &mut W,
    file_resolver: &mut F,
    product: &P,
    fastboot_interface: &mut T,
    cmd: ManifestParams,
) -> Result<()>
where
    W: Write,
    F: FileResolver + Sync,
    Part: Partition,
    P: Product<Part>,
    T: FastbootInterface,
{
    flash_bootloader(writer, file_resolver, product, fastboot_interface, &cmd).await?;
    flash_product(writer, file_resolver, product, fastboot_interface, &cmd).await
}

pub async fn is_userspace_fastboot(
    fastboot_interface: &mut impl FastbootInterface,
) -> Result<bool> {
    match fastboot_interface.get_var(IS_USERSPACE_VAR).await {
        Ok(rev) => Ok(rev == "yes"),
        _ => Ok(false),
    }
}

#[tracing::instrument(skip(file_resolver, writer, cmd, product))]
pub async fn flash_bootloader<W, F, Part, P, T>(
    writer: &mut W,
    file_resolver: &mut F,
    product: &P,
    fastboot_interface: &mut T,
    cmd: &ManifestParams,
) -> Result<()>
where
    W: Write,
    F: FileResolver + Sync,
    Part: Partition,
    P: Product<Part>,
    T: FastbootInterface,
{
    flash_partitions(
        writer,
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
        reboot_bootloader(writer, fastboot_interface).await?;
    }
    Ok(())
}

#[tracing::instrument(skip(writer, file_resolver, cmd, product))]
pub async fn flash_product<W, F, Part, P, T>(
    writer: &mut W,
    file_resolver: &mut F,
    product: &P,
    fastboot_interface: &mut T,
    cmd: &ManifestParams,
) -> Result<()>
where
    W: Write,
    F: FileResolver + Sync,
    Part: Partition,
    P: Product<Part>,
    T: FastbootInterface,
{
    flash_partitions(
        writer,
        file_resolver,
        product.partitions(),
        fastboot_interface,
        cmd.flash_min_timeout_seconds,
        cmd.flash_timeout_rate_mb_per_second,
    )
    .await?;
    if !cmd.no_bootloader_reboot && is_userspace_fastboot(fastboot_interface).await? {
        write!(writer, "Rebooting into updated userspace fastboot...\n")?;
        reboot_bootloader(writer, fastboot_interface).await?;
    }
    stage_oem_files(writer, file_resolver, false, &cmd.oem_stage, fastboot_interface).await?;
    stage_oem_files(writer, file_resolver, true, product.oem_files(), fastboot_interface).await
}

#[tracing::instrument(skip(writer, file_resolver, cmd, product))]
pub async fn flash_and_reboot<W, F, Part, P, T>(
    writer: &mut W,
    file_resolver: &mut F,
    product: &P,
    fastboot_interface: &mut T,
    cmd: ManifestParams,
) -> Result<()>
where
    W: Write,
    F: FileResolver + Sync,
    Part: Partition,
    P: Product<Part>,
    T: FastbootInterface,
{
    flash(writer, file_resolver, product, fastboot_interface, cmd).await?;
    finish(writer, fastboot_interface).await
}

pub async fn finish<W: Write, F: FastbootInterface>(
    writer: &mut W,
    fastboot_interface: &mut F,
) -> Result<()> {
    set_slot_a_active(fastboot_interface).await?;
    fastboot_interface.continue_boot().await.map_err(|_| anyhow!("Could not reboot device"))?;
    writeln!(writer, "Continuing to boot - this could take awhile")?;
    Ok(())
}

pub async fn is_locked(fastboot_interface: &mut impl FastbootInterface) -> Result<bool> {
    verify_variable_value(LOCKED_VAR, "no", fastboot_interface).await.map(|l| !l)
}

pub async fn lock_device(fastboot_interface: &mut impl FastbootInterface) -> Result<()> {
    fastboot_interface.oem(LOCK_COMMAND).await.map_err(|_| anyhow!("Could not lock device"))
}

pub async fn from_manifest<W, C, F>(
    writer: &mut W,
    input: C,
    fastboot_interface: &mut F,
) -> Result<()>
where
    W: Write,
    C: Into<ManifestParams>,
    F: FastbootInterface,
{
    let cmd: ManifestParams = input.into();
    match &cmd.manifest {
        Some(manifest) => {
            if !manifest.is_file() {
                ffx_bail!("Manifest \"{}\" is not a file.", manifest.display());
            }
            from_path(writer, manifest.to_path_buf(), fastboot_interface, cmd).await
        }
        None => {
            if let Some(path) = cmd.product_bundle.as_ref().filter(|s| is_local_product_bundle(s)) {
                from_local_product_bundle(writer, PathBuf::from(&*path), fastboot_interface, cmd)
                    .await
            } else {
                let sdk = ffx_config::global_env_context()
                    .context("loading global environment context")?
                    .get_sdk()
                    .await?;
                let mut path = sdk.get_path_prefix().to_path_buf();
                writeln!(writer, "No manifest path was given, using SDK from {}.", path.display())?;
                path.push("flash.json"); // Not actually used, placeholder value needed.
                match sdk.get_version() {
                    SdkVersion::InTree => from_in_tree(writer, fastboot_interface, cmd).await,
                    SdkVersion::Version(_) => from_sdk(writer, fastboot_interface, cmd).await,
                    _ => ffx_bail!("Unknown SDK type"),
                }
            }
        }
    }
}
