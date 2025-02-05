// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use addr::TargetAddr;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use chrono::{Duration, Utc};
use errors::ffx_bail;
use ffx_fastboot::common::cmd::OemFile;
use ffx_fastboot::common::fastboot::{
    tcp_proxy, udp_proxy, usb_proxy, FastbootNetworkConnectionConfig,
};
use ffx_fastboot::common::from_manifest;
use ffx_fastboot::util::{Event, UnlockEvent};
use ffx_fastboot_interface::fastboot_interface::UploadProgress;
use ffx_flash_args::FlashCommand;
use ffx_ssh::SshKeyFiles;
use fho::{FfxContext, FfxMain, FfxTool, VerifiedMachineWriter};
use fidl_fuchsia_developer_ffx::{
    FastbootInterface as FidlFastbootInterface, TargetInfo, TargetRebootState, TargetState,
};
use fuchsia_async::{MonotonicInstant, Timer};
use futures::try_join;
use schemars::JsonSchema;
use serde::Serialize;
use std::io::Write;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Once;
use target_holders::TargetProxyHolder;
use termion::{color, style};
use tokio::sync::mpsc;
use tokio::sync::mpsc::Receiver;

const SSH_OEM_COMMAND: &str = "add-staged-bootloader-file ssh.authorized_keys";

/// Seconds to wait for until we warn the user we cannot rediscover the target
/// in the bootloader
const WAIT_WARN_SECS: u64 = 20;

#[derive(FfxTool)]
pub struct FlashTool {
    #[command]
    cmd: FlashCommand,
    target_proxy: fho::Deferred<TargetProxyHolder>,
}

fho::embedded_plugin!(FlashTool);

#[derive(Debug, Serialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FlashMessage {
    Progress(FlashProgress),
    Finished { success: bool, error_message: String },
}

#[derive(Debug, Serialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FlashProgress {
    FlashPartitionStarted { partition_name: String },
    FlashPartitionFinished { partition_name: String },
    Unlock,
    RebootToBootloaderStarted,
    RebootToBootloaderFinished,
    GotVariable,
    OemCommand { oem_command: String },
    UploadStarted,
    UploadFinished,
    UploadError,
}

#[async_trait(?Send)]
impl FfxMain for FlashTool {
    // TODO(b/b/380444711): Add tests for schema
    type Writer = VerifiedMachineWriter<FlashMessage>;

    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        // Checks
        preflight_checks(&self.cmd, &mut writer)?;

        // Massage FlashCommand
        let cmd = preprocess_flash_cmd(self.cmd).await?;

        flash_plugin_impl(self.target_proxy.await?, cmd, &mut writer).await
    }
}

fn preflight_checks<W: Write>(cmd: &FlashCommand, mut writer: W) -> Result<()> {
    if cmd.manifest_path.is_some() {
        // TODO(https://fxbug.dev/42076631)
        writeln!(writer, "{}WARNING:{} specifying the flash manifest via a positional argument is deprecated. Use the --manifest flag instead (https://fxbug.dev/42076631)", color::Fg(color::Red), style::Reset)
.with_context(||"writing warning to users")
.map_err(fho::Error::from)?;
    }
    if cmd.manifest_path.is_some() && cmd.manifest.is_some() {
        ffx_bail!("Error: the manifest must be specified either by positional argument or the --manifest flag")
    }
    Ok(())
}

async fn preprocess_flash_cmd(mut cmd: FlashCommand) -> Result<FlashCommand> {
    match cmd.authorized_keys.as_ref() {
        Some(ssh) => {
            let ssh_file = match std::fs::canonicalize(ssh) {
                Ok(path) => path,
                Err(err) => {
                    ffx_bail!("Cannot find SSH key \"{}\": {}", ssh, err);
                }
            };
            if cmd.oem_stage.iter().any(|f| f.command() == SSH_OEM_COMMAND) {
                ffx_bail!("Both the SSH key and the SSH OEM Stage flags were set. Only use one.");
            }
            if cmd.skip_authorized_keys {
                ffx_bail!("Both the SSH key and Skip Uploading Authorized Keys flags were set. Only use one.");
            }
            cmd.oem_stage.push(OemFile::new(
                SSH_OEM_COMMAND.to_string(),
                ssh_file
                    .into_os_string()
                    .into_string()
                    .map_err(|s| anyhow!("Cannot convert OsString \"{:?}\" to String", s))?,
            ));
        }
        None => {
            if !cmd.oem_stage.iter().any(|f| f.command() == SSH_OEM_COMMAND) {
                if cmd.skip_authorized_keys {
                    tracing::warn!("Skipping uploading authorized keys");
                } else {
                    let ssh_keys = SshKeyFiles::load(None)
                        .await
                        .context("finding ssh authorized_keys file.")?;
                    ssh_keys.create_keys_if_needed(false).context("creating ssh keys if needed")?;
                    if ssh_keys.authorized_keys.exists() {
                        let k = ssh_keys.authorized_keys.display().to_string();
                        tracing::debug!("No `--authorized-keys` flag, using {}", k);
                        cmd.oem_stage.push(OemFile::new(SSH_OEM_COMMAND.to_string(), k));
                    } else {
                        // Since the key will be initialized, this should never happen.
                        ffx_bail!("We requested ssh keys to be created but they were not");
                    }
                }
            } else if cmd.skip_authorized_keys {
                // We have both skip authorized-keys and the OEM command including
                // the authorized keys... this is a problem.
                ffx_bail!("Both the SSH OEM Stage and Skip Uploading Authorized Keys flags were set. Only use one.");
            }
        }
    };

    if cmd.manifest_path.is_some() {
        if !std::path::Path::exists(cmd.manifest_path.clone().unwrap().as_path()) {
            ffx_bail!("Manifest path: {} does not exist", cmd.manifest_path.unwrap().display())
        }
    }

    if cmd.product_bundle.is_none() && cmd.manifest_path.is_none() && cmd.manifest.is_none() {
        let product_path: String = ffx_config::get("product.path")?;
        tracing::debug!(
            "No product bundle or manifest passed. Inferring product bundle path from config: {}",
            product_path
        );
        cmd.product_bundle = Some(product_path);
    }

    if cmd.product_bundle.is_some()
        && cmd.product_bundle.clone().unwrap().starts_with("\"")
        && cmd.product_bundle.clone().unwrap().ends_with("\"")
    {
        let cleaned_product_bundle = cmd
            .product_bundle
            .unwrap()
            .strip_prefix('"')
            .unwrap()
            .strip_suffix('"')
            .unwrap()
            .to_string();
        tracing::debug!(
            "Passed product bundle was wrapped in quotes, trimming it to: {}",
            cleaned_product_bundle
        );
        cmd.product_bundle = Some(cleaned_product_bundle);
    }
    Ok(cmd)
}

#[tracing::instrument(skip(target_proxy, writer))]
async fn flash_plugin_impl(
    target_proxy: TargetProxyHolder,
    cmd: FlashCommand,
    writer: &mut VerifiedMachineWriter<FlashMessage>,
) -> fho::Result<()> {
    let mut info = target_proxy.identity().await.user_message("Error getting Target's identity")?;

    fn display_name(info: &TargetInfo) -> &str {
        info.nodename.as_deref().or(info.serial_number.as_deref()).unwrap_or("<unknown>")
    }

    match info.target_state {
        Some(TargetState::Fastboot) => {
            // Nothing to do
        }
        Some(TargetState::Disconnected) => {
            // Nothing to do, for a slightly different reason.
            // Since there's no knowledge about the state of the target, assume the
            // target is in Fastboot.
            tracing::info!("Target not connected, assuming Fastboot state");
        }
        Some(_) => {
            // Wait to allow the Target to fully cycle to the bootloader
            writeln!(writer, "Waiting for Target to reboot...")
                .user_message("Error writing user message")?;
            writer.flush().user_message("Error flushing writer buffer")?;

            // Tell the target to reboot to the bootloader
            target_proxy
                .reboot(TargetRebootState::Bootloader)
                .await
                .user_message("Got error rebooting")?
                .map_err(|e| anyhow!("Got error rebooting target: {:#?}", e))
                .user_message("Got an error rebooting")?;

            let wait_duration = Duration::seconds(1)
                .to_std()
                .user_message("Error converting 1 seconds to Duration")?;
            let once = Once::new();
            let start = MonotonicInstant::now();
            loop {
                // Get the info again since the target changed state
                info = target_proxy
                    .identity()
                    .await
                    .user_message("Error getting the target's identity")?;

                if matches!(info.target_state, Some(TargetState::Fastboot)) {
                    break;
                }

                // Warn the user
                if MonotonicInstant::now() - start
                    > fuchsia_async::MonotonicDuration::from_secs(WAIT_WARN_SECS)
                {
                    once.call_once(|| {
                        let _ = writeln!(
                            writer,
                            "Have been waiting for Target \
                                                {} to reboot to bootloader for \
                                                more than {} seconds but still \
                                                have not rediscovered it. You \
                                                may want to cancel this \
                                                operation and check your \
                                                connection to the target",
                            display_name(&info),
                            WAIT_WARN_SECS
                        );
                    });
                }

                tracing::debug!("Target was requested to reboot to the bootloader, but was found in {:#?} state. Waiting 1 second.", info.target_state);
                Timer::new(wait_duration).await;
            }
        }
        None => {
            ffx_bail!("Target had an unknown, non-existant state")
        }
    };

    let start_time = Utc::now();

    let res = match info.fastboot_interface {
        None => ffx_bail!("Could not connect to {}: Target not in fastboot", display_name(&info)),
        Some(FidlFastbootInterface::Usb) => {
            let serial_num = info.serial_number.ok_or_else(|| {
                anyhow!("Target was detected in Fastboot USB but did not have a serial number")
            })?;
            let mut proxy = usb_proxy(serial_num).await?;
            let (client, server) = mpsc::channel(1);
            try_join!(from_manifest(client, cmd, &mut proxy), handle_event(writer, server))
                .map_err(fho::Error::from)?;
            Ok::<(), fho::Error>(())
        }
        Some(FidlFastbootInterface::Udp) => {
            // We take the first address as when a target is in Fastboot mode and over
            // UDP it only exposes one address
            if let Some(addr) = info.addresses.unwrap().into_iter().take(1).next() {
                let target_addr: TargetAddr = addr.into();
                let socket_addr: SocketAddr = target_addr.into();

                let target_name = if let Some(nodename) = info.nodename {
                    nodename
                } else {
                    writeln!(
                        writer,
                        r"
Warning: the target does not have a node name and is in UDP fastboot mode.
Rediscovering the target after bootloader reboot will be impossible.
Please try --no-bootloader-reboot to avoid a reboot.
Using address {} as node name",
                        socket_addr
                    )
                    .user_message("Error writing user message")?;
                    socket_addr.to_string()
                };
                let config = FastbootNetworkConnectionConfig::new_udp().await;
                let fastboot_device_file_path: Option<PathBuf> =
                    ffx_config::get(fastboot_file_discovery::FASTBOOT_FILE_PATH).ok();
                let mut proxy =
                    udp_proxy(target_name, fastboot_device_file_path, &socket_addr, config).await?;
                let (client, server) = mpsc::channel(1);
                try_join!(from_manifest(client, cmd, &mut proxy), handle_event(writer, server))
                    .map_err(fho::Error::from)?;
                Ok(())
            } else {
                ffx_bail!("Could not get a valid address for target");
            }
        }
        Some(FidlFastbootInterface::Tcp) => {
            // We take the first address as when a target is in Fastboot mode and over
            // TCP it only exposes one address
            if let Some(addr) = info.addresses.unwrap().into_iter().take(1).next() {
                let target_addr: TargetAddr = addr.into();
                let socket_addr: SocketAddr = target_addr.into();

                let target_name = if let Some(nodename) = info.nodename {
                    nodename
                } else {
                    writeln!(
                        writer,
                        r"
Warning: the target does not have a node name and is in TCP fastboot mode.
Rediscovering the target after bootloader reboot will be impossible.
Please try --no-bootloader-reboot to avoid a reboot.
Using address {} as node name",
                        socket_addr
                    )
                    .user_message("Error writing user message")?;
                    socket_addr.to_string()
                };
                let config = FastbootNetworkConnectionConfig::new_tcp().await;
                let fastboot_device_file_path: Option<PathBuf> =
                    ffx_config::get(fastboot_file_discovery::FASTBOOT_FILE_PATH).ok();
                let mut proxy =
                    tcp_proxy(target_name, fastboot_device_file_path, &socket_addr, config).await?;
                let (client, server) = mpsc::channel(1);
                try_join!(from_manifest(client, cmd, &mut proxy), handle_event(writer, server))
                    .map_err(fho::Error::from)?;
                Ok(())
            } else {
                ffx_bail!("Could not get a valid address for target");
            }
        }
    };

    match res {
        Ok(()) => {
            let duration = Utc::now().signed_duration_since(start_time);
            let finished_message = format!("Continuing to boot - this could take awhile\n{}Done{}. {}Total Time{} [{}{:.2}s{}]",
                color::Fg(color::Green),
                style::Reset,
                color::Fg(color::Green),
                style::Reset,
                color::Fg(color::Blue),
                (duration.num_milliseconds() as f32) / (1000 as f32),
                style::Reset
            );

            writer.machine_or(
                &FlashMessage::Finished { success: true, error_message: "".to_string() },
                finished_message,
            )?;
        }
        Err(e) => {
            writer.machine_or(
                &FlashMessage::Finished { success: false, error_message: format!("{}", e) },
                format!("Error: {:?}", e),
            )?;
            return Err(e);
        }
    }
    Ok(())
}

fn done_time(duration: Duration) -> String {
    format!(
        "{}Done{} [{}{:.2}s{}]",
        color::Fg(color::Green),
        style::Reset,
        color::Fg(color::Blue),
        (duration.num_milliseconds() as f32) / (1000 as f32),
        style::Reset
    )
}

async fn handle_event(
    writer: &mut VerifiedMachineWriter<FlashMessage>,
    mut rec: Receiver<Event>,
) -> Result<()> {
    loop {
        match rec.recv().await {
            Some(event) => match event {
                Event::Upload(upload) => match upload {
                    UploadProgress::OnStarted { size: _size } => {
                        let (machine, message) = (
                            FlashMessage::Progress(FlashProgress::UploadStarted),
                            "Uploading... ".to_string(),
                        );
                        writer.machine_or_write(&machine, message)?;
                    }
                    UploadProgress::OnProgress { bytes_written } => {
                        tracing::trace!("Made progres, wrote: {}", bytes_written);
                    }
                    UploadProgress::OnFinished => {
                        let (machine, message) = (
                            FlashMessage::Progress(FlashProgress::UploadFinished),
                            format!("{}Done{}", color::Fg(color::Green), style::Reset),
                        );
                        writer.machine_or(&machine, message)?;
                    }
                    UploadProgress::OnError { error } => {
                        let (machine, message) = (
                            FlashMessage::Progress(FlashProgress::UploadError),
                            format!("Error {}", error),
                        );
                        writer.machine_or(&machine, message)?;
                    }
                },
                Event::FlashPartition { partition_name } => {
                    let (machine, message) = (
                        FlashMessage::Progress(FlashProgress::FlashPartitionStarted {
                            partition_name: partition_name.clone(),
                        }),
                        format!("Beginning flash for partition {}... ", partition_name),
                    );
                    writer.machine_or_write(&machine, message)?;
                }
                Event::FlashPartitionFinished { partition_name, duration } => {
                    let (machine, message) = (
                        FlashMessage::Progress(FlashProgress::FlashPartitionFinished {
                            partition_name: partition_name.clone(),
                        }),
                        done_time(duration),
                    );
                    writer.machine_or(&machine, message)?;
                }
                Event::Unlock(unlock_event) => {
                    let (machine, message) = match unlock_event {
                        UnlockEvent::SearchingForCredentials => (
                            FlashMessage::Progress(FlashProgress::Unlock),
                            "Looking for unlock credentials... ".to_string(),
                        ),
                        UnlockEvent::FoundCredentials(delta) => (
                            FlashMessage::Progress(FlashProgress::Unlock),
                            format!("{}\n", done_time(delta)),
                        ),
                        UnlockEvent::GeneratingToken => (
                            FlashMessage::Progress(FlashProgress::Unlock),
                            "Generating unlock token... ".to_string(),
                        ),
                        UnlockEvent::FinishedGeneratingToken(delta) => (
                            FlashMessage::Progress(FlashProgress::Unlock),
                            format!("{}\n", done_time(delta)),
                        ),
                        UnlockEvent::BeginningUploadOfToken => (
                            FlashMessage::Progress(FlashProgress::Unlock),
                            "Preparing to upload unlock token\n".to_string(),
                        ),
                        UnlockEvent::Done => {
                            (FlashMessage::Progress(FlashProgress::Unlock), "Done\n".to_string())
                        }
                    };
                    writer.machine_or_write(&machine, message)?;
                }
                Event::Oem { oem_command } => {
                    let (machine, message) = (
                        FlashMessage::Progress(FlashProgress::OemCommand {
                            oem_command: oem_command.clone(),
                        }),
                        format!("Sendinge command: \"{}\"", oem_command),
                    );
                    writer.machine_or(&machine, message)?;
                }
                Event::RebootStarted => {
                    let (machine, message) = (
                        FlashMessage::Progress(FlashProgress::RebootToBootloaderStarted),
                        "Rebooting to bootloader... ".to_string(),
                    );
                    writer.machine_or_write(&machine, message)?;
                }
                Event::Rebooted(delta) => {
                    let (machine, message) = (
                        FlashMessage::Progress(FlashProgress::RebootToBootloaderFinished),
                        done_time(delta),
                    );
                    writer.machine_or(&machine, message)?;
                }
                Event::Variable(variable) => {
                    tracing::trace!("got variable {:#?}", variable);
                    let (machine, message) = (
                        FlashMessage::Progress(FlashProgress::GotVariable),
                        "variable".to_string(),
                    );
                    writer.machine_or(&machine, message)?;
                }
                Event::Locked => {
                    let msg = format!("The flashing library should not Lock the device...");
                    let (machine, message) = (
                        FlashMessage::Finished { success: false, error_message: msg.clone() },
                        msg.clone(),
                    );
                    writer.machine_or(&machine, message)?;
                    return Err(anyhow!(msg));
                }
            },
            None => return Ok(()),
        }
    }
}

////////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use super::*;
    use fho::Format;
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_preprocess_flash_command_infers_product_bundle() {
        let env = ffx_config::test_init().await.expect("Failed to initialize test env");
        env.context
            .query("product.path")
            .level(Some(ffx_config::ConfigLevel::User))
            .set("foo".into())
            .await
            .expect("creating temp product.path");
        let cmd = preprocess_flash_cmd(FlashCommand { ..Default::default() }).await.unwrap();
        assert_eq!(cmd.product_bundle, Some("foo".to_string()));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_nonexistent_file_throws_err() {
        let _env = ffx_config::test_init().await.expect("Failed to initialize test env");
        assert!(preprocess_flash_cmd(FlashCommand {
            manifest_path: Some(PathBuf::from("ffx_test_does_not_exist")),
            ..Default::default()
        })
        .await
        .is_err())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_clean_quotes() {
        let _env = ffx_config::test_init().await.expect("Failed to initialize test env");
        let pb_tmp_file = NamedTempFile::new().expect("tmp access failed");
        let pb_tmp_file_name = pb_tmp_file.path().to_string_lossy().to_string();
        let wrapped_pb_tmp_file_name = format!("\"{}\"", pb_tmp_file_name);

        let ssh_tmp_file = NamedTempFile::new().expect("tmp access failed");
        let ssh_tmp_file_name = ssh_tmp_file.path().to_string_lossy().to_string();

        let cmd = preprocess_flash_cmd(FlashCommand {
            product_bundle: Some(wrapped_pb_tmp_file_name),
            authorized_keys: Some(ssh_tmp_file_name),
            ..Default::default()
        })
        .await
        .unwrap();
        assert_eq!(Some(pb_tmp_file_name), cmd.product_bundle);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_nonexistent_ssh_file_throws_err() {
        let _env = ffx_config::test_init().await.expect("Failed to initialize test env");
        let tmp_file = NamedTempFile::new().expect("tmp access failed");
        let tmp_file_name = tmp_file.path().to_string_lossy().to_string();
        assert!(preprocess_flash_cmd(FlashCommand {
            manifest_path: Some(PathBuf::from(tmp_file_name)),
            authorized_keys: Some("ssh_does_not_exist".to_string()),
            ..Default::default()
        },)
        .await
        .is_err())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_specify_manifest_twice_throws_error() {
        let _env = ffx_config::test_init().await.expect("Failed to initialize test env");
        let tmp_file = NamedTempFile::new().expect("tmp access failed");
        let tmp_file_name = tmp_file.path().to_string_lossy().to_string();
        let mut writer = VerifiedMachineWriter::<FlashMessage>::new(Some(Format::Json));
        assert!(preflight_checks(
            &FlashCommand {
                manifest: Some(PathBuf::from(tmp_file_name.clone())),
                manifest_path: Some(PathBuf::from(tmp_file_name)),
                ..Default::default()
            },
            &mut writer
        )
        .is_err());
    }
}
