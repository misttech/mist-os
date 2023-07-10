// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use errors::ffx_bail;
use ffx_fastboot::common::{cmd::OemFile, from_manifest};
use ffx_flash_args::FlashCommand;
use ffx_ssh::SshKeyFiles;
use fho::{FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_developer_ffx::FastbootProxy;
use std::io::Write;
use termion::{color, style};

const SSH_OEM_COMMAND: &str = "add-staged-bootloader-file ssh.authorized_keys";

#[derive(FfxTool)]
pub struct FlashTool {
    #[command]
    cmd: FlashCommand,
    fastboot_proxy: FastbootProxy,
}

fho::embedded_plugin!(FlashTool);

#[async_trait(?Send)]
impl FfxMain for FlashTool {
    type Writer = SimpleWriter;
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        flash_plugin_impl(self.fastboot_proxy, self.cmd, &mut writer).await?;
        Ok(())
    }
}

#[tracing::instrument(skip(fastboot_proxy, writer))]
pub async fn flash_plugin_impl<W: Write>(
    fastboot_proxy: FastbootProxy,
    mut cmd: FlashCommand,
    writer: &mut W,
) -> Result<()> {
    if cmd.manifest_path.is_some() {
        // TODO(fxb/125854)
        writeln!(writer, "{}WARNING:{} specifying the flash manifest via a positional argument is deprecated. Use the --manifest flag instead (fxb/125854)", color::Fg(color::Red), style::Reset)?;
    }
    if cmd.manifest_path.is_some() && cmd.manifest.is_some() {
        ffx_bail!("Error: the manifest must be specified either by positional argument or the --manifest flag")
    }
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
                    SshKeyFiles::load().await.context("finding ssh authorized_keys file.")?;
                ssh_keys.create_keys_if_needed().context("creating ssh keys if needed")?;
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
    }

    from_manifest(writer, cmd, fastboot_proxy).await
}

////////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use super::*;
    use ffx_fastboot::test::setup;
    use pretty_assertions::assert_eq;
    use std::{default::Default, path::PathBuf};
    use tempfile::NamedTempFile;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_nonexistent_file_throws_err() {
        assert!(flash_plugin_impl(
            setup().1,
            FlashCommand {
                manifest_path: Some(PathBuf::from("ffx_test_does_not_exist")),
                ..Default::default()
            },
            &mut vec![],
        )
        .await
        .is_err())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_nonexistent_ssh_file_throws_err() {
        let tmp_file = NamedTempFile::new().expect("tmp access failed");
        let tmp_file_name = tmp_file.path().to_string_lossy().to_string();
        assert!(flash_plugin_impl(
            setup().1,
            FlashCommand {
                manifest_path: Some(PathBuf::from(tmp_file_name)),
                authorized_keys: Some("ssh_does_not_exist".to_string()),
                ..Default::default()
            },
            &mut vec![],
        )
        .await
        .is_err())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_specify_manifest_twice_throws_error() {
        let tmp_file = NamedTempFile::new().expect("tmp access failed");
        let tmp_file_name = tmp_file.path().to_string_lossy().to_string();
        let mut w = Vec::new();
        assert!(flash_plugin_impl(
            setup().1,
            FlashCommand {
                manifest: Some(PathBuf::from(tmp_file_name.clone())),
                manifest_path: Some(PathBuf::from(tmp_file_name)),
                ..Default::default()
            },
            &mut w
        )
        .await
        .is_err());
        // Additionally, check that the warning was printed
        assert_eq!(
            std::str::from_utf8(&w).expect("UTF8 String"),
            format!(
                "{}WARNING:{} specifying the flash manifest via a positional argument is deprecated. Use the --manifest flag instead (fxb/125854)\n",
                color::Fg(color::Red),
                style::Reset
        ));
    }
}
