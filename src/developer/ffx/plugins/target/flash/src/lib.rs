// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use addr::TargetAddr;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use chrono::Duration;
use errors::ffx_bail;
use ffx_fastboot::common::{
    cmd::OemFile,
    fastboot::{tcp_proxy, udp_proxy, usb_proxy, FastbootNetworkConnectionConfig},
    from_manifest,
};
use ffx_flash_args::FlashCommand;
use ffx_ssh::SshKeyFiles;
use fho::FfxContext;
use fho::{FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_developer_ffx::TargetState;
use fidl_fuchsia_developer_ffx::{
    FastbootInterface as FidlFastbootInterface, TargetInfo, TargetProxy, TargetRebootState,
};
use fuchsia_async::{Time, Timer};
use std::io::Write;
use std::net::SocketAddr;
use std::sync::Once;
use termion::{color, style};

const SSH_OEM_COMMAND: &str = "add-staged-bootloader-file ssh.authorized_keys";

/// Seconds to wait for until we warn the user we cannot rediscover the target
/// in the bootloader
const WAIT_WARN_SECS: u64 = 20;

#[derive(FfxTool)]
pub struct FlashTool {
    #[command]
    cmd: FlashCommand,
    target_proxy: fho::Deferred<TargetProxy>,
}

fho::embedded_plugin!(FlashTool);

#[async_trait(?Send)]
impl FfxMain for FlashTool {
    type Writer = SimpleWriter;
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        // Checks
        preflight_checks(&self.cmd, &mut writer)?;

        // Massage FlashCommand
        let cmd = preprocess_flash_cmd(&mut writer, self.cmd).await?;

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

async fn preprocess_flash_cmd<W: Write>(
    mut writer: W,
    mut cmd: FlashCommand,
) -> Result<FlashCommand> {
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
                let ssh_keys =
                    SshKeyFiles::load(None).await.context("finding ssh authorized_keys file.")?;
                ssh_keys.create_keys_if_needed(false).context("creating ssh keys if needed")?;
                if ssh_keys.authorized_keys.exists() {
                    let k = ssh_keys.authorized_keys.display().to_string();
                    eprintln!("No `--authorized-keys` flag, using {}", k);
                    cmd.oem_stage.push(OemFile::new(SSH_OEM_COMMAND.to_string(), k));
                } else {
                    // Since the key will be initialized, this should never happen.
                    ffx_bail!("Warning: flashing without a SSH key is not advised.");
                }
            }
        }
    };

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
        writeln!(
            writer,
            "Passed product bundle was wrapped in quotes, trimming it to: {}",
            cleaned_product_bundle
        )
        .user_message("Error writing ser message")?;
        cmd.product_bundle = Some(cleaned_product_bundle);
    }
    Ok(cmd)
}

#[tracing::instrument(skip(target_proxy, writer))]
async fn flash_plugin_impl<W: Write>(
    target_proxy: TargetProxy,
    cmd: FlashCommand,
    mut writer: W,
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
            let start = Time::now();
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
                if Time::now() - start > fuchsia_async::Duration::from_secs(WAIT_WARN_SECS) {
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
        None => ffx_bail!("Could not connect to {}: Target not in fastboot", display_name(&info)),
        Some(FidlFastbootInterface::Usb) => {
            let serial_num = info.serial_number.ok_or_else(|| {
                anyhow!("Target was detected in Fastboot USB but did not have a serial number")
            })?;
            let mut proxy = usb_proxy(serial_num).await?;
            from_manifest(&mut writer, cmd, &mut proxy).await.map_err(fho::Error::from)
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
Using address {} as node name",
                        socket_addr.to_string()
                    )
                    .user_message("Error writing user message")?;
                    socket_addr.to_string()
                };
                let config = FastbootNetworkConnectionConfig::new_udp().await;
                let mut proxy = udp_proxy(target_name, &socket_addr, config).await?;
                from_manifest(&mut writer, cmd, &mut proxy).await.map_err(fho::Error::from)
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
Using address {} as node name",
                        socket_addr.to_string()
                    )
                    .user_message("Error writing user message")?;
                    socket_addr.to_string()
                };
                let config = FastbootNetworkConnectionConfig::new_tcp().await;
                let mut proxy = tcp_proxy(target_name, &socket_addr, config).await?;
                from_manifest(&mut writer, cmd, &mut proxy).await.map_err(fho::Error::from)
            } else {
                ffx_bail!("Could not get a valid address for target");
            }
        }
    }
}

////////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_nonexistent_file_throws_err() {
        let writer = Vec::new();
        assert!(preprocess_flash_cmd(
            writer,
            FlashCommand {
                manifest_path: Some(PathBuf::from("ffx_test_does_not_exist")),
                ..Default::default()
            }
        )
        .await
        .is_err())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_clean_quotes() {
        let pb_tmp_file = NamedTempFile::new().expect("tmp access failed");
        let pb_tmp_file_name = pb_tmp_file.path().to_string_lossy().to_string();
        let wrapped_pb_tmp_file_name = format!("\"{}\"", pb_tmp_file_name);

        let ssh_tmp_file = NamedTempFile::new().expect("tmp access failed");
        let ssh_tmp_file_name = ssh_tmp_file.path().to_string_lossy().to_string();

        let writer = Vec::new();
        let cmd = preprocess_flash_cmd(
            writer,
            FlashCommand {
                product_bundle: Some(wrapped_pb_tmp_file_name),
                authorized_keys: Some(ssh_tmp_file_name),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(Some(pb_tmp_file_name), cmd.product_bundle);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_nonexistent_ssh_file_throws_err() {
        let tmp_file = NamedTempFile::new().expect("tmp access failed");
        let tmp_file_name = tmp_file.path().to_string_lossy().to_string();
        let writer = Vec::new();
        assert!(preprocess_flash_cmd(
            writer,
            FlashCommand {
                manifest_path: Some(PathBuf::from(tmp_file_name)),
                authorized_keys: Some("ssh_does_not_exist".to_string()),
                ..Default::default()
            },
        )
        .await
        .is_err())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_specify_manifest_twice_throws_error() {
        let tmp_file = NamedTempFile::new().expect("tmp access failed");
        let tmp_file_name = tmp_file.path().to_string_lossy().to_string();
        let mut w = Vec::new();
        assert!(preflight_checks(
            &FlashCommand {
                manifest: Some(PathBuf::from(tmp_file_name.clone())),
                manifest_path: Some(PathBuf::from(tmp_file_name)),
                ..Default::default()
            },
            &mut w
        )
        .is_err());
        // Additionally, check that the warning was printed
        assert_eq!(
            std::str::from_utf8(&w).expect("UTF8 String"),
            format!(
                "{}WARNING:{} specifying the flash manifest via a positional argument is deprecated. Use the --manifest flag instead (https://fxbug.dev/42076631)\n",
                color::Fg(color::Red),
                style::Reset
        ));
    }
}
