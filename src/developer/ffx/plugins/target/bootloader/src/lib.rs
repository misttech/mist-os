// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use addr::TargetAddr;
use anyhow::anyhow;
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use errors::ffx_bail;
use ffx_bootloader_args::SubCommand::{Boot, Info, Lock, Unlock};
use ffx_bootloader_args::{BootCommand, BootloaderCommand, UnlockCommand};
use ffx_fastboot::boot::boot;
use ffx_fastboot::common::fastboot::{
    tcp_proxy, udp_proxy, usb_proxy, FastbootNetworkConnectionConfig,
};
use ffx_fastboot::common::from_manifest;
use ffx_fastboot::file_resolver::resolvers::EmptyResolver;
use ffx_fastboot::info::info;
use ffx_fastboot::lock::lock;
use ffx_fastboot::unlock::unlock;
use ffx_fastboot::util::{Event, UnlockEvent};
use ffx_fastboot_interface::fastboot_interface::{FastbootInterface, UploadProgress, Variable};
use ffx_writer::VerifiedMachineWriter;
use fho::{FfxContext, FfxMain, FfxTool};
use fidl_fuchsia_developer_ffx::{
    FastbootInterface as FidlFastbootInterface, TargetInfo, TargetRebootState, TargetState,
};
use fuchsia_async::{MonotonicInstant, Timer};
use futures::try_join;
use schemars::JsonSchema;
use serde::Serialize;
use std::io::{stdin, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Once;
use target_holders::TargetProxyHolder;
use termion::{color, style};
use tokio::sync::mpsc;
use tokio::sync::mpsc::Receiver;

const MISSING_ZBI: &str = "Error: vbmeta parameter must be used with zbi parameter";

const WARNING: &str = "WARNING: ALL SETTINGS USER CONTENT WILL BE ERASED!\n\
                        Do you want to continue? [yN]";

const WAIT_WARN_SECS: u64 = 20;

#[derive(FfxTool)]
pub struct BootloaderTool {
    #[command]
    cmd: BootloaderCommand,
    target_proxy: TargetProxyHolder,
}

fho::embedded_plugin!(BootloaderTool);

#[derive(Default, Debug, Serialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum BootloaderToolMessageType {
    #[default]
    Unknown,
    Info,
    Rebooting,
    Error,
}

#[derive(Debug, Serialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct VariableMessage {
    key: String,
    value: String,
}

#[derive(Default, Debug, Serialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct InfoMessage {
    variables: Vec<VariableMessage>,
}

#[derive(Default, Debug, Serialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct BootloaderToolMessage {
    message_type: BootloaderToolMessageType,
    info_message: InfoMessage,
}

#[async_trait(?Send)]
impl FfxMain for BootloaderTool {
    type Writer = VerifiedMachineWriter<BootloaderToolMessage>;

    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        let mut info = self.target_proxy.identity().await.map_err(|e| anyhow!(e))?;

        fn display_name(info: &TargetInfo) -> &str {
            info.nodename.as_deref().or(info.serial_number.as_deref()).unwrap_or("<unknown>")
        }

        match info.target_state {
            Some(TargetState::Fastboot) => {
                // Nothing to do
                tracing::debug!("Target already in Fastboot state");
            }
            Some(TargetState::Disconnected) => {
                // Nothing to do, for a slightly different reason.
                // Since there's no knowledge about the state of the target, assume the
                // target is in Fastboot.
                tracing::info!("Target not connected, assuming Fastboot state");
            }
            Some(mode) => {
                // Wait to allow the Target to fully cycle to the bootloader
                writeln!(writer, "Waiting for Target to reboot to bootloader")
                    .user_message("Error writing user message")?;
                writer.flush().user_message("Error flushing writer buffer")?;

                // Tell the target to reboot to the bootloader
                tracing::debug!("Target in {:#?} state. Rebooting to bootloader...", mode);
                self.target_proxy
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
                    info = self
                        .target_proxy
                        .identity()
                        .await
                        .user_message("Error getting the target's identity")?;

                    if matches!(info.target_state, Some(TargetState::Fastboot)) {
                        break;
                    }

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
        match info.fastboot_interface {
            None => {
                ffx_bail!("Could not connect to {}: Target not in fastboot", display_name(&info))
            }
            Some(FidlFastbootInterface::Usb) => {
                let serial_num = info.serial_number.ok_or_else(|| {
                    anyhow!("Target was detected in Fastboot USB but did not have a serial number")
                })?;
                let proxy = usb_proxy(serial_num).await?;
                bootloader_impl(proxy, self.cmd, &mut writer).await
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
                        tracing::debug!(
                            r"
Warning: the target does not have a node name and is in UDP fastboot mode.
Rediscovering the target after bootloader reboot will be impossible.
Using address {} as node name",
                            socket_addr.to_string()
                        );
                        socket_addr.to_string()
                    };
                    let config = FastbootNetworkConnectionConfig::new_udp().await;
                    let fastboot_device_file_path: Option<PathBuf> =
                        ffx_config::get(fastboot_file_discovery::FASTBOOT_FILE_PATH).ok();
                    let proxy =
                        udp_proxy(target_name, fastboot_device_file_path, &socket_addr, config)
                            .await?;
                    bootloader_impl(proxy, self.cmd, &mut writer).await
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
                        tracing::debug!(
                            r"
Warning: the target does not have a node name and is in TCP fastboot mode.
Rediscovering the target after bootloader reboot will be impossible.
Using address {} as node name
",
                            socket_addr.to_string()
                        );
                        socket_addr.to_string()
                    };
                    let config = FastbootNetworkConnectionConfig::new_tcp().await;
                    let fastboot_device_file_path: Option<PathBuf> =
                        ffx_config::get(fastboot_file_discovery::FASTBOOT_FILE_PATH).ok();
                    let proxy =
                        tcp_proxy(target_name, fastboot_device_file_path, &socket_addr, config)
                            .await?;
                    bootloader_impl(proxy, self.cmd, &mut writer).await
                } else {
                    ffx_bail!("Could not get a valid address for target");
                }
            }
        }
    }
}

async fn handle_upload(
    writer: &mut VerifiedMachineWriter<BootloaderToolMessage>,
    mut prog_server: Receiver<UploadProgress>,
) -> anyhow::Result<()> {
    let mut start_time: Option<DateTime<Utc>> = None;
    loop {
        match prog_server.recv().await {
            Some(UploadProgress::OnStarted { size, .. }) => {
                start_time.replace(Utc::now());
                tracing::debug!("Upload started: {}", size);
                write!(writer, "Uploading... ")?;
                if size > (1 << 24) {
                    write!(writer, "Large file")?;
                }
                writer.flush()?;
            }
            Some(UploadProgress::OnFinished { .. }) => {
                if let Some(st) = start_time {
                    let d = Utc::now().signed_duration_since(st);
                    tracing::debug!("Upload duration: {:.2}s", (d.num_milliseconds() / 1000));
                } else {
                    writeln!(writer, "{}Done{}", color::Fg(color::Green), style::Reset)?;
                    writer.flush()?;
                }
                tracing::debug!("Upload finished");
            }
            Some(UploadProgress::OnError { error, .. }) => {
                tracing::error!("{}", error);
                ffx_bail!("{}", error)
            }
            Some(UploadProgress::OnProgress { bytes_written, .. }) => {
                tracing::trace!("Upload progress: {}", bytes_written);
            }
            None => return Ok(()),
        }
    }
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
async fn handle_events(
    writer: &mut VerifiedMachineWriter<BootloaderToolMessage>,
    mut var_server: Receiver<Event>,
) -> anyhow::Result<()> {
    let mut start_time: Option<DateTime<Utc>> = None;
    loop {
        match var_server.recv().await {
            Some(Event::Locked) => {
                writeln!(writer, "Locked")?;
            }
            Some(Event::Unlock(unlock_event)) => {
                let message = match unlock_event {
                    UnlockEvent::SearchingForCredentials => {
                        "Looking for unlock credentials... ".to_string()
                    }
                    UnlockEvent::FoundCredentials(delta) => format!("{}\n", done_time(delta)),
                    UnlockEvent::GeneratingToken => "Generating unlock token... ".to_string(),
                    UnlockEvent::FinishedGeneratingToken(delta) => {
                        format!("{}\n", done_time(delta))
                    }
                    UnlockEvent::BeginningUploadOfToken => {
                        "Preparing to upload unlock token\n".to_string()
                    }
                    UnlockEvent::Done => "Done\n".to_string(),
                };
                write!(writer, "{}", message)?;
            }
            Some(Event::RebootStarted) => {
                writeln!(writer, "Reboot started")?;
            }
            Some(Event::Rebooted(_)) => {
                writeln!(writer, "Rebooted")?;
            }
            Some(Event::Oem { oem_command }) => {
                writeln!(writer, "settnt oem command: {}", oem_command)?;
            }
            Some(Event::Upload(upload)) => match upload {
                UploadProgress::OnStarted { size, .. } => {
                    start_time.replace(Utc::now());
                    tracing::debug!("Upload started: {}", size);
                    write!(writer, "Uploading... ")?;
                    if size > (1 << 24) {
                        write!(writer, "Large file")?;
                    }
                    writer.flush()?;
                }
                UploadProgress::OnFinished { .. } => {
                    if let Some(st) = start_time {
                        let d = Utc::now().signed_duration_since(st);
                        tracing::debug!("Upload duration: {:.2}s", (d.num_milliseconds() / 1000));
                    } else {
                        writeln!(writer, "{}Done{}", color::Fg(color::Green), style::Reset)?;
                        writer.flush()?;
                    }
                    tracing::debug!("Upload finished");
                }
                UploadProgress::OnError { error, .. } => {
                    tracing::error!("{}", error);
                    ffx_bail!("{}", error)
                }
                UploadProgress::OnProgress { bytes_written, .. } => {
                    tracing::trace!("Upload progress: {}", bytes_written);
                }
            },
            Some(Event::FlashPartition { .. }) | Some(Event::FlashPartitionFinished { .. }) => {
                ffx_bail!("Should not get flash partition events in this bootloader command.");
            }
            Some(Event::Variable(_)) => {
                ffx_bail!("Should not get variable event in this bootloader command.");
            }
            None => break,
        }
    }
    return Ok(());
}

async fn handle_variables_for_fastboot(
    writer: &mut VerifiedMachineWriter<BootloaderToolMessage>,
    mut var_server: Receiver<Variable>,
) -> anyhow::Result<()> {
    let mut variables = vec![];
    loop {
        match var_server.recv().await {
            Some(Variable { name, value, .. }) => {
                variables.push(VariableMessage { key: name, value });
            }
            None => break,
        }
    }
    let message = variables
        .iter()
        .map(|x| format!("{}: {}", x.key, x.value))
        .collect::<Vec<String>>()
        .join("\n");
    writer
        .machine_or(
            &BootloaderToolMessage {
                message_type: BootloaderToolMessageType::Info,
                info_message: InfoMessage { variables },
            },
            message,
        )
        .map_err(|e| anyhow!(e))
}

pub async fn bootloader_impl(
    mut fastboot_proxy: impl FastbootInterface,
    mut cmd: BootloaderCommand,
    writer: &mut VerifiedMachineWriter<BootloaderToolMessage>,
) -> fho::Result<()> {
    if cmd.product_bundle.is_none() && cmd.manifest.is_none() {
        let product_path = ffx_config::get("product.path").ok();
        if let Some(product_path) = product_path {
            writeln!(
                writer,
                "No product bundle or manifest passed. Inferring product bundle path from config: {}",
                product_path
            )
            .user_message("Error writing user message")?;
            cmd.product_bundle = Some(product_path);
        }
    }
    // SubCommands can overwrite the manifest with their own parameters, so check for those
    // conditions before continuing through to check the flash manifest.
    match &cmd.subcommand {
        Info(_) => {
            let (client, server) = mpsc::channel(1);
            try_join!(
                info(client, &mut fastboot_proxy),
                handle_variables_for_fastboot(writer, server)
            )
            .map_err(fho::Error::from)?;
            return Ok(());
        }
        Lock(_) => {
            lock(&mut fastboot_proxy).await.map_err(fho::Error::from)?;
            writeln!(writer, "Target is now locked.").bug_context("failed to write")?;
            return Ok(());
        }
        Unlock(UnlockCommand { cred, force }) => {
            if !force {
                writeln!(writer, "{}", WARNING).bug_context("failed to write")?;
                let answer = blocking::unblock(|| {
                    use std::io::BufRead;
                    let mut line = String::new();
                    let stdin = stdin();
                    let mut locked = stdin.lock();
                    let _ = locked.read_line(&mut line);
                    line
                })
                .await;
                if answer.trim() != "y" {
                    ffx_bail!("User aborted");
                }
            }
            match cred {
                Some(cred_file) => {
                    let (client, server) = mpsc::channel(1);
                    let credentials = vec![cred_file.to_string()];
                    let mut resolver = EmptyResolver::new()?;
                    try_join!(
                        unlock(client, &mut resolver, &credentials, &mut fastboot_proxy,),
                        handle_events(writer, server)
                    )
                    .map_err(fho::Error::from)?;
                    return Ok(());
                }
                _ => {}
            }
        }
        Boot(BootCommand { zbi, vbmeta, .. }) => {
            if vbmeta.is_some() && zbi.is_none() {
                ffx_bail!("{}", MISSING_ZBI)
            }
            match zbi {
                Some(z) => {
                    let (client, server) = mpsc::channel(1);
                    let mut resolver = EmptyResolver::new()?;
                    try_join!(
                        boot(
                            client,
                            &mut resolver,
                            z.to_owned(),
                            vbmeta.to_owned(),
                            &mut fastboot_proxy,
                        ),
                        handle_upload(writer, server)
                    )
                    .map_err(fho::Error::from)?;
                    return Ok(());
                }
                _ => {}
            }
        }
    }

    let (client, server) = mpsc::channel(1);
    try_join!(from_manifest(client, cmd, &mut fastboot_proxy), handle_events(writer, server))
        .map_err(fho::Error::from)?;
    return Ok(());
}

////////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use super::*;
    use ffx_bootloader_args::LockCommand;
    use ffx_fastboot::common::vars::LOCKED_VAR;
    use ffx_fastboot::test::setup;
    use ffx_writer::Format;
    use tempfile::NamedTempFile;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_boot_stages_file_and_calls_boot() -> fho::Result<()> {
        let zbi_file = NamedTempFile::new().expect("tmp access failed");
        let zbi_file_name = zbi_file.path().to_string_lossy().to_string();
        let vbmeta_file = NamedTempFile::new().expect("tmp access failed");
        let vbmeta_file_name = vbmeta_file.path().to_string_lossy().to_string();
        let (state, proxy) = setup();
        let mut w = VerifiedMachineWriter::<BootloaderToolMessage>::new(Some(Format::Json));
        bootloader_impl(
            proxy,
            BootloaderCommand {
                manifest: None,
                product: "Fuchsia".to_string(),
                product_bundle: None,
                skip_verify: false,
                subcommand: Boot(BootCommand {
                    zbi: Some(zbi_file_name),
                    vbmeta: Some(vbmeta_file_name),
                    slot: "a".to_string(),
                }),
            },
            &mut w,
        )
        .await?;
        let state = state.lock().unwrap();
        assert_eq!(1, state.staged_files.len());
        assert_eq!(1, state.boots);
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_boot_stages_file_and_calls_boot_with_just_zbi() -> fho::Result<()> {
        let zbi_file = NamedTempFile::new().expect("tmp access failed");
        let zbi_file_name = zbi_file.path().to_string_lossy().to_string();
        let (state, proxy) = setup();
        let mut w = VerifiedMachineWriter::<BootloaderToolMessage>::new(Some(Format::Json));
        bootloader_impl(
            proxy,
            BootloaderCommand {
                manifest: None,
                product: "Fuchsia".to_string(),
                product_bundle: None,
                skip_verify: false,
                subcommand: Boot(BootCommand {
                    zbi: Some(zbi_file_name),
                    vbmeta: None,
                    slot: "a".to_string(),
                }),
            },
            &mut w,
        )
        .await?;
        let state = state.lock().unwrap();
        assert_eq!(1, state.staged_files.len());
        assert_eq!(1, state.boots);
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_boot_fails_with_just_vbmeta() {
        let vbmeta_file = NamedTempFile::new().expect("tmp access failed");
        let vbmeta_file_name = vbmeta_file.path().to_string_lossy().to_string();
        let (_, proxy) = setup();
        let mut w = VerifiedMachineWriter::<BootloaderToolMessage>::new(Some(Format::Json));
        assert!(bootloader_impl(
            proxy,
            BootloaderCommand {
                manifest: None,
                product: "Fuchsia".to_string(),
                product_bundle: None,
                skip_verify: false,
                subcommand: Boot(BootCommand {
                    zbi: None,
                    vbmeta: Some(vbmeta_file_name),
                    slot: "a".to_string(),
                }),
            },
            &mut w,
        )
        .await
        .is_err());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_lock_calls_oem_command() -> fho::Result<()> {
        let (state, proxy) = setup();
        {
            let mut state = state.lock().unwrap();
            // is_locked
            state.set_var(LOCKED_VAR.to_string(), "no".to_string());
            state.set_var("vx-unlockable".to_string(), "no".to_string());
        }
        let mut w = VerifiedMachineWriter::<BootloaderToolMessage>::new(Some(Format::Json));
        bootloader_impl(
            proxy,
            BootloaderCommand {
                manifest: None,
                product: "Fuchsia".to_string(),
                product_bundle: None,
                skip_verify: false,
                subcommand: Lock(LockCommand {}),
            },
            &mut w,
        )
        .await?;
        let state = state.lock().unwrap();
        assert_eq!(1, state.oem_commands.len());
        assert_eq!("vx-lock".to_string(), state.oem_commands[0]);
        Ok(())
    }
}
