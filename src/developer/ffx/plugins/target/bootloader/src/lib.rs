// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use addr::TargetAddr;
use anyhow::anyhow;
use async_trait::async_trait;
use chrono::Duration;
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
use ffx_fastboot_interface::fastboot_interface::FastbootInterface;
use fho::{FfxContext, FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_developer_ffx::{
    FastbootInterface as FidlFastbootInterface, TargetInfo, TargetProxy, TargetRebootState,
    TargetState,
};
use fuchsia_async::{MonotonicInstant, Timer};
use std::io::{stdin, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Once;

const MISSING_ZBI: &str = "Error: vbmeta parameter must be used with zbi parameter";

const WARNING: &str = "WARNING: ALL SETTINGS USER CONTENT WILL BE ERASED!\n\
                        Do you want to continue? [yN]";

const WAIT_WARN_SECS: u64 = 20;

#[derive(FfxTool)]
pub struct BootloaderTool {
    #[command]
    cmd: BootloaderCommand,
    target_proxy: TargetProxy,
}

fho::embedded_plugin!(BootloaderTool);

#[async_trait(?Send)]
impl FfxMain for BootloaderTool {
    type Writer = SimpleWriter;
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

pub async fn bootloader_impl<W: Write>(
    mut fastboot_proxy: impl FastbootInterface,
    cmd: BootloaderCommand,
    writer: &mut W,
) -> fho::Result<()> {
    // SubCommands can overwrite the manifest with their own parameters, so check for those
    // conditions before continuing through to check the flash manifest.
    match &cmd.subcommand {
        Info(_) => return info(writer, &mut fastboot_proxy).await.map_err(fho::Error::from),
        Lock(_) => return lock(writer, &mut fastboot_proxy).await.map_err(fho::Error::from),
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
                    return unlock(
                        writer,
                        &mut EmptyResolver::new()?,
                        &vec![cred_file.to_string()],
                        &mut fastboot_proxy,
                    )
                    .await
                    .map_err(fho::Error::from);
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
                    return boot(
                        writer,
                        &mut EmptyResolver::new()?,
                        z.to_owned(),
                        vbmeta.to_owned(),
                        &mut fastboot_proxy,
                    )
                    .await
                    .map_err(fho::Error::from);
                }
                _ => {}
            }
        }
    }

    from_manifest(writer, cmd, &mut fastboot_proxy).await.map_err(fho::Error::from)
}

////////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use super::*;
    use ffx_bootloader_args::LockCommand;
    use ffx_fastboot::common::vars::LOCKED_VAR;
    use ffx_fastboot::test::setup;
    use tempfile::NamedTempFile;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_boot_stages_file_and_calls_boot() -> fho::Result<()> {
        let zbi_file = NamedTempFile::new().expect("tmp access failed");
        let zbi_file_name = zbi_file.path().to_string_lossy().to_string();
        let vbmeta_file = NamedTempFile::new().expect("tmp access failed");
        let vbmeta_file_name = vbmeta_file.path().to_string_lossy().to_string();
        let (state, proxy) = setup();
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
            &mut std::io::stdout(),
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
            &mut std::io::stdout(),
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
            &mut std::io::stdout(),
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
        bootloader_impl(
            proxy,
            BootloaderCommand {
                manifest: None,
                product: "Fuchsia".to_string(),
                product_bundle: None,
                skip_verify: false,
                subcommand: Lock(LockCommand {}),
            },
            &mut std::io::stdout(),
        )
        .await?;
        let state = state.lock().unwrap();
        assert_eq!(1, state.oem_commands.len());
        assert_eq!("vx-lock".to_string(), state.oem_commands[0]);
        Ok(())
    }
}
