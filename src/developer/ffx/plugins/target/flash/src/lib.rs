// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use addr::TargetIpAddr;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use chrono::{Duration, Utc};
use discovery::events::TargetEvent;
use discovery::{
    DiscoveryBuilder, FastbootConnectionState, TargetDiscovery, TargetHandle, TargetState,
};
use errors::ffx_bail;
use fastboot_file_discovery::FASTBOOT_FILE_PATH;
use ffx_config::EnvironmentContext;
use ffx_fastboot::common::cmd::OemFile;
use ffx_fastboot::common::fastboot::{
    tcp_proxy, udp_proxy, usb_proxy, FastbootNetworkConnectionConfig,
};
use ffx_fastboot::common::from_manifest;
use ffx_fastboot::util::{Event, UnlockEvent};
use ffx_fastboot_interface::fastboot_interface::UploadProgress;
use ffx_flash_args::FlashCommand;
use ffx_ssh::SshKeyFiles;
use ffx_writer::VerifiedMachineWriter;
use fho::{deferred, return_bug, return_user_error, user_error, FfxContext, FfxMain, FfxTool};
use fidl::Error;
use fidl_fuchsia_developer_ffx::TargetState as FidlTargetState;
use fidl_fuchsia_hardware_power_statecontrol::AdminProxy;
use fidl_fuchsia_hwinfo::DeviceProxy;
use futures::{try_join, FutureExt, StreamExt};
use schemars::JsonSchema;
use serde::Serialize;
use std::cell::RefCell;
use std::collections::HashSet;
use std::fmt::{Display, Formatter};
use std::io::Write;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::rc::Rc;
use target_holders::{moniker, TargetInfoHolder};
use termion::{color, style};
use tokio::sync::mpsc;
use tokio::sync::mpsc::Receiver;

const SSH_OEM_COMMAND: &str = "add-staged-bootloader-file ssh.authorized_keys";

enum AvoidRebootWarning<'a> {
    Udp(&'a SocketAddr),
    Tcp(&'a SocketAddr),
}

impl Display for AvoidRebootWarning<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        let (mode, addr) = match self {
            Self::Udp(addr) => ("UDP", addr),
            Self::Tcp(addr) => ("TCP", addr),
        };
        write!(
            f,
            r"
Warning: the target does not have a node name and is in {} fastboot mode.
Rediscovering the target after bootloader reboot will be impossible.
Please try --no-bootloader-reboot to avoid a reboot.
Using address {} as node name",
            mode, addr
        )
    }
}

#[derive(FfxTool)]
#[no_target]
pub struct FlashTool {
    #[command]
    cmd: FlashCommand,
    target_info: TargetInfoHolder,
    ctx: EnvironmentContext,
    #[with(deferred(moniker("/bootstrap/shutdown_shim")))]
    power_proxy: fho::Deferred<AdminProxy>,
    #[with(deferred(moniker("/core/hwinfo")))]
    device_proxy: fho::Deferred<DeviceProxy>,
}

fho::embedded_plugin!(FlashTool);

#[derive(Debug, Serialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FlashMessage {
    Preflight { message: String },
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
        let cmd = preprocess_flash_cmd(&mut writer, &self.cmd).await?;

        self.flash_plugin_impl(cmd, &mut writer).await
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

async fn preprocess_flash_cmd(
    writer: &mut VerifiedMachineWriter<FlashMessage>,
    i_cmd: &FlashCommand,
) -> Result<FlashCommand> {
    let cmd: &mut FlashCommand = &mut i_cmd.clone();
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
            ffx_bail!(
                "Manifest path: {} does not exist",
                cmd.manifest_path.clone().unwrap().display()
            )
        }
    }

    if cmd.product_bundle.is_none() && cmd.manifest_path.is_none() && cmd.manifest.is_none() {
        let product_path: String = ffx_config::get("product.path")?;
        let message = format!(
            "No product bundle or manifest passed. Inferring product bundle path from config: {}",
            product_path
        );
        tracing::debug!(message);
        writer.machine_or(&FlashMessage::Preflight { message: message.clone() }, message)?;

        cmd.product_bundle = Some(product_path);
    }

    if cmd.product_bundle.is_some()
        && cmd.product_bundle.clone().unwrap().starts_with("\"")
        && cmd.product_bundle.clone().unwrap().ends_with("\"")
    {
        let cleaned_product_bundle = cmd
            .product_bundle
            .clone()
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
    Ok(cmd.to_owned())
}

async fn rediscover_target(
    ctx: &EnvironmentContext,
    serial_number: Option<String>,
) -> fho::Result<TargetState> {
    // Do discovery of the target... and try to find it again then
    // return the appropriate information
    let emulator_instance_root: PathBuf = ctx
        .get("emu.instance_dir")
        .map_err(|e| user_error!("Unable to get config value: {:#?}", e))?;
    let fastboot_file_path: PathBuf = ctx
        .get(FASTBOOT_FILE_PATH)
        .map_err(|e| user_error!("Unable to get config value: {:#?}", e))?;
    let disco = DiscoveryBuilder::default()
        .notify_removed(false)
        .with_emulator_instance_root(emulator_instance_root)
        .with_fastboot_devices_file_path(fastboot_file_path)
        .build();

    #[derive(Clone)]
    struct Criteria {
        serial: Option<String>,
    }

    let criteria = Criteria { serial: serial_number.clone() };
    let c_clone = criteria.clone();

    let filter_target = move |handle: &TargetHandle| {
        tracing::debug!("Considering handle: {:#?}", handle);
        match &handle.state {
            discovery::TargetState::Fastboot(fts)
                if Some(fts.serial_number.clone()) == c_clone.serial =>
            {
                true
            }
            _ => {
                tracing::debug!("Skipping handle: {:#?}", handle);
                false
            }
        }
    };

    let stream = disco.discover_devices(filter_target).await?;
    let timer = fuchsia_async::Timer::new(std::time::Duration::from_millis(100000)).fuse();
    let found_target_event = async_utils::event::Event::new();
    let found_it = found_target_event.wait().fuse();
    let seen = Rc::new(RefCell::new(HashSet::new()));
    let discovered_devices_stream = stream
        .filter_map(move |ev| {
            let c_clone = criteria.clone();
            let found_ev = found_target_event.clone();
            let seen = seen.clone();
            async move {
                match ev {
                    Ok(TargetEvent::Added(ref h)) => {
                        if seen.borrow().contains(h) {
                            None
                        } else {
                            match &h.state {
                                discovery::TargetState::Fastboot(fts) => match c_clone.serial {
                                    Some(c) if c == fts.serial_number => {
                                        tracing::debug!("Found the target, firing signal");
                                        found_ev.signal();
                                    }
                                    _ => {}
                                },
                                _ => {}
                            }
                            seen.borrow_mut().insert(h.clone());
                            Some(Ok((*h).clone()))
                        }
                    }
                    // We've only asked for Added events
                    Ok(_) => unreachable!(),
                    Err(e) => Some(Err(e)),
                }
            }
        })
        .take_until(futures_lite::future::race(timer, found_it));

    let mut discovered_devices = discovered_devices_stream.collect::<Vec<Result<_, _>>>().await;

    match discovered_devices.len() {
        0 => {
            return_bug!("Could not rediscover device after rebooting to the bootloader")
        }
        1 => {
            let device_res = discovered_devices.pop().unwrap();
            Ok((device_res?).state)
        }
        num @ _ => {
            return_bug!("Expected to rediscover only one device, but found: {}", num)
        }
    }
}

async fn reboot_target_to_bootloader_and_rediscover(
    writer: &mut VerifiedMachineWriter<FlashMessage>,
    ctx: &EnvironmentContext,
    device_proxy: DeviceProxy,
    power_proxy: AdminProxy,
) -> fho::Result<TargetState> {
    // Wait to allow the Target to fully cycle to the bootloader
    writeln!(writer, "Waiting for Target to reboot...")
        .user_message("Error writing user message")?;
    writer.flush().user_message("Error flushing writer buffer")?;

    // Tell the target to reboot to the bootloader
    // Should probably get the serial number of the target just in case
    // let device_proxy = device_proxy.await.bug_context("Initializing device proxy")?;
    let info = device_proxy.get_info().await.bug_context("Getting target device info")?;

    // Tell the target to reboot to the bootloader
    tracing::debug!("Target in Product state. Rebooting to bootloader...",);

    // These calls erroring is fine...
    match power_proxy.reboot_to_bootloader().await {
        Ok(_) => {}
        Err(e) => handle_fidl_connection_err(e)?,
    };

    rediscover_target(&ctx, info.serial_number).await
}

impl FlashTool {
    async fn flash_plugin_impl(
        self,
        cmd: FlashCommand,
        writer: &mut VerifiedMachineWriter<FlashMessage>,
    ) -> fho::Result<()> {
        let target_state = match &self.target_info.target_state {
            Some(FidlTargetState::Fastboot) => {
                // Nothing to do
                tracing::debug!("Target already in Fastboot state");
                let s: discovery::TargetHandle = (*self.target_info).clone().try_into()?;
                s.state
            }
            Some(FidlTargetState::Product) => {
                if self.ctx.is_strict() {
                    return_user_error!(
                        r"
When running in strict mode, this tool does not support Targets in Product mode.
Reboot the Target to the bootloader and re-run this command."
                    );
                }
                let device_proxy =
                    self.device_proxy.await.bug_context("Initializing device proxy")?;
                let power_proxy = self.power_proxy.await?;

                reboot_target_to_bootloader_and_rediscover(
                    writer,
                    &self.ctx,
                    device_proxy,
                    power_proxy,
                )
                .await?
            }
            Some(FidlTargetState::Unknown) => {
                ffx_bail!("Target is in an Unknown state.");
            }
            Some(FidlTargetState::Zedboot) => {
                ffx_bail!("Bootloader operations not supported with Zedboot");
            }
            Some(FidlTargetState::Disconnected) => {
                tracing::info!("Target: {:#?} not connected bailing", self.target_info);
                ffx_bail!("Target is disconnected...");
            }
            None => {
                ffx_bail!("Target had an unknown, non-existant state")
            }
        };

        let start_time = Utc::now();

        let res = match target_state {
            TargetState::Fastboot(fastboot_state) => match fastboot_state.connection_state {
                FastbootConnectionState::Usb => {
                    let serial_num = fastboot_state.serial_number;
                    let mut proxy = usb_proxy(serial_num).await?;
                    let (client, server) = mpsc::channel(1);
                    try_join!(from_manifest(client, cmd, &mut proxy), handle_event(writer, server))
                        .map_err(fho::Error::from)?;
                    Ok::<(), fho::Error>(())
                }
                FastbootConnectionState::Udp(addrs) => {
                    // We take the first address as when a target is in Fastboot mode and over
                    // UDP it only exposes one address
                    if let Some(addr) = addrs.into_iter().take(1).next() {
                        let target_addr: TargetIpAddr = addr.into();
                        let socket_addr: SocketAddr = target_addr.into();

                        let target_name = if let Some(nodename) = &self.target_info.nodename {
                            nodename
                        } else {
                            if !cmd.no_bootloader_reboot {
                                writeln!(writer, "{}", AvoidRebootWarning::Udp(&socket_addr))
                                    .user_message("Error writing user message")?;
                            }
                            &socket_addr.to_string()
                        };
                        let config = FastbootNetworkConnectionConfig::new_udp().await;
                        let fastboot_device_file_path: Option<PathBuf> =
                            ffx_config::get(fastboot_file_discovery::FASTBOOT_FILE_PATH).ok();
                        let mut proxy = udp_proxy(
                            target_name.clone(),
                            fastboot_device_file_path,
                            &socket_addr,
                            config,
                        )
                        .await?;
                        let (client, server) = mpsc::channel(1);
                        try_join!(
                            from_manifest(client, cmd, &mut proxy),
                            handle_event(writer, server)
                        )
                        .map_err(fho::Error::from)?;
                        Ok(())
                    } else {
                        ffx_bail!("Could not get a valid address for target");
                    }
                }
                FastbootConnectionState::Tcp(addrs) => {
                    // We take the first address as when a target is in Fastboot mode and over
                    // TCP it only exposes one address
                    if let Some(addr) = addrs.into_iter().take(1).next() {
                        let target_addr: TargetIpAddr = addr.into();
                        let socket_addr: SocketAddr = target_addr.into();

                        let target_name = if let Some(nodename) = &self.target_info.nodename {
                            nodename
                        } else {
                            if !cmd.no_bootloader_reboot {
                                writeln!(writer, "{}", AvoidRebootWarning::Tcp(&socket_addr))
                                    .user_message("Error writing user message")?;
                            }
                            &socket_addr.to_string()
                        };
                        let config = FastbootNetworkConnectionConfig::new_tcp().await;
                        let fastboot_device_file_path: Option<PathBuf> =
                            ffx_config::get(fastboot_file_discovery::FASTBOOT_FILE_PATH).ok();
                        let mut proxy = tcp_proxy(
                            target_name.clone(),
                            fastboot_device_file_path,
                            &socket_addr,
                            config,
                        )
                        .await?;
                        let (client, server) = mpsc::channel(1);
                        try_join!(
                            from_manifest(client, cmd, &mut proxy),
                            handle_event(writer, server)
                        )
                        .map_err(fho::Error::from)?;
                        Ok(())
                    } else {
                        ffx_bail!("Could not get a valid address for target");
                    }
                }
            },
            _ => {
                ffx_bail!("Could not connect. Target not in fastboot: {}", target_state);
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
}

fn handle_fidl_connection_err(e: Error) -> fho::Result<()> {
    match e {
        Error::ClientChannelClosed { protocol_name, .. } => {
            // Changing this to an info from warn since reboot has succeeded The assumption that
            // reboot has succeeded is correct since we received a ClientChannelClosed
            // successfully. So let's just make the message clearer to the user.
            //
            // Check the 'protocol_name' and if it is 'fuchsia.hardware.power.statecontrol.Admin'
            // then we can be more confident that target reboot/shutdown has succeeded.
            if protocol_name == "fuchsia.hardware.power.statecontrol.Admin" {
                tracing::info!("Target reboot succeeded.");
            } else {
                tracing::info!("Assuming target reboot succeeded. Client received a PEER_CLOSED from '{protocol_name}'");
            }
            tracing::debug!("{:?}", e);
            Ok(())
        }
        _ => {
            tracing::error!("Target communication error: {:?}", e);
            return_bug!("Target communication error: {:?}", e)
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
    use ffx_writer::{Format, TestBuffers};
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    #[fuchsia::test]
    async fn test_preprocess_flash_command_infers_product_bundle() {
        let env = ffx_config::test_init().await.expect("Failed to initialize test env");
        env.context
            .query("product.path")
            .level(Some(ffx_config::ConfigLevel::User))
            .set("foo".into())
            .await
            .expect("creating temp product.path");
        let buffers = TestBuffers::default();
        let mut writer = <FlashTool as FfxMain>::Writer::new_test(Some(Format::Json), &buffers);
        let cmd = preprocess_flash_cmd(&mut writer, &FlashCommand { ..Default::default() })
            .await
            .unwrap();
        assert_eq!(cmd.product_bundle, Some("foo".to_string()));
    }

    #[fuchsia::test]
    async fn test_nonexistent_file_throws_err() {
        let _env = ffx_config::test_init().await.expect("Failed to initialize test env");
        let buffers = TestBuffers::default();
        let mut writer = <FlashTool as FfxMain>::Writer::new_test(Some(Format::Json), &buffers);
        assert!(preprocess_flash_cmd(
            &mut writer,
            &FlashCommand {
                manifest_path: Some(PathBuf::from("ffx_test_does_not_exist")),
                ..Default::default()
            }
        )
        .await
        .is_err())
    }

    #[fuchsia::test]
    async fn test_clean_quotes() {
        let _env = ffx_config::test_init().await.expect("Failed to initialize test env");
        let pb_tmp_file = NamedTempFile::new().expect("tmp access failed");
        let pb_tmp_file_name = pb_tmp_file.path().to_string_lossy().to_string();
        let wrapped_pb_tmp_file_name = format!("\"{}\"", pb_tmp_file_name);

        let ssh_tmp_file = NamedTempFile::new().expect("tmp access failed");
        let ssh_tmp_file_name = ssh_tmp_file.path().to_string_lossy().to_string();

        let buffers = TestBuffers::default();
        let mut writer = <FlashTool as FfxMain>::Writer::new_test(Some(Format::Json), &buffers);

        let cmd = preprocess_flash_cmd(
            &mut writer,
            &FlashCommand {
                product_bundle: Some(wrapped_pb_tmp_file_name),
                authorized_keys: Some(ssh_tmp_file_name),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(Some(pb_tmp_file_name), cmd.product_bundle);
    }

    #[fuchsia::test]
    async fn test_nonexistent_ssh_file_throws_err() {
        let _env = ffx_config::test_init().await.expect("Failed to initialize test env");
        let tmp_file = NamedTempFile::new().expect("tmp access failed");
        let tmp_file_name = tmp_file.path().to_string_lossy().to_string();

        let buffers = TestBuffers::default();
        let mut writer = <FlashTool as FfxMain>::Writer::new_test(Some(Format::Json), &buffers);
        assert!(preprocess_flash_cmd(
            &mut writer,
            &FlashCommand {
                manifest_path: Some(PathBuf::from(tmp_file_name)),
                authorized_keys: Some("ssh_does_not_exist".to_string()),
                ..Default::default()
            },
        )
        .await
        .is_err())
    }

    #[fuchsia::test]
    async fn test_specify_manifest_twice_throws_error() {
        let _env = ffx_config::test_init().await.expect("Failed to initialize test env");
        let tmp_file = NamedTempFile::new().expect("tmp access failed");
        let tmp_file_name = tmp_file.path().to_string_lossy().to_string();
        let buffers = TestBuffers::default();
        let mut writer = <FlashTool as FfxMain>::Writer::new_test(Some(Format::Json), &buffers);
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

    #[test]
    fn test_avoid_reboot_bootloader_warning() {
        let udp_addr = "127.0.0.1:0".parse().unwrap();
        let tcp_addr = "192.168.1.2:22".parse().unwrap();
        let udp = AvoidRebootWarning::Udp(&udp_addr);
        let tcp = AvoidRebootWarning::Tcp(&tcp_addr);
        assert_eq!(
            udp.to_string(),
            r"
Warning: the target does not have a node name and is in UDP fastboot mode.
Rediscovering the target after bootloader reboot will be impossible.
Please try --no-bootloader-reboot to avoid a reboot.
Using address 127.0.0.1:0 as node name",
        );
        assert_eq!(
            tcp.to_string(),
            r"
Warning: the target does not have a node name and is in TCP fastboot mode.
Rediscovering the target after bootloader reboot will be impossible.
Please try --no-bootloader-reboot to avoid a reboot.
Using address 192.168.1.2:22 as node name",
        );
    }
}
