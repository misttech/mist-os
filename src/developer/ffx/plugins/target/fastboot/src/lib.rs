// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use addr::TargetIpAddr;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use discovery::{FastbootConnectionState, TargetState};
use errors::ffx_bail;
use ffx_config::EnvironmentContext;
use ffx_fastboot::common::fastboot::{
    tcp_proxy, udp_proxy, usb_proxy, FastbootNetworkConnectionConfig,
};
use ffx_fastboot::common::flash_partition_impl;
use ffx_fastboot::common::vars::MAX_DOWNLOAD_SIZE_VAR;
use ffx_fastboot_interface::fastboot_interface::{FastbootInterface, Variable};
use ffx_fastboot_tool_args::{FastbootCommand, FastbootSubcommand};
use ffx_writer::VerifiedMachineWriter;
use fho::{FfxMain, FfxTool};
use fidl_fuchsia_developer_ffx::TargetState as FidlTargetState;
use futures::try_join;
use schemars::JsonSchema;
use serde::Serialize;
use sparse::reader::SparseReader;
use sparse::{build_sparse_files, unsparse};
use std::fs::File;
use std::io::Write;
use std::net::SocketAddr;
use std::path::PathBuf;
use target_holders::TargetInfoHolder;
use tempfile::NamedTempFile;
use tokio::sync::mpsc;
use tokio::sync::mpsc::Receiver;

#[derive(FfxTool)]
#[no_target]
pub struct FastbootTool {
    #[command]
    cmd: FastbootCommand,
    target_info: TargetInfoHolder,
    ctx: EnvironmentContext,
}

fho::embedded_plugin!(FastbootTool);

#[derive(Debug, Serialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FastbootMessage {
    Variable { name: String, value: String },
}

#[async_trait(?Send)]
impl FfxMain for FastbootTool {
    type Writer = VerifiedMachineWriter<FastbootMessage>;

    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        cmd_impl(&self.ctx, &mut writer, &self.cmd, &self.target_info).await
    }
}

async fn sink<T>(mut rx: Receiver<T>) -> Result<()> {
    while rx.recv().await.is_some() {}
    Ok(())
}

async fn handle_vars<W>(writer: &mut W, mut rx: Receiver<Variable>) -> Result<()>
where
    W: Write,
{
    loop {
        match rx.recv().await {
            Some(x) => {
                write!(writer, "{}:{}", x.name, x.value)?;
            }
            None => break,
        }
    }
    Ok(())
}

async fn fastboot_impl<F>(
    writer: &mut VerifiedMachineWriter<FastbootMessage>,
    command: &FastbootCommand,
    interface: &mut F,
) -> fho::Result<()>
where
    F: FastbootInterface,
{
    match &command.subcommand {
        FastbootSubcommand::Flash(cmd) => {
            let flash_timeout_rate_mb_per_second: u64 = 5000;
            let flash_min_timeout_seconds: u64 = 200;
            let (client, server) = mpsc::channel(1);
            try_join!(
                flash_partition_impl(
                    client,
                    &cmd.partition,
                    cmd.file.to_str().unwrap(),
                    interface,
                    flash_min_timeout_seconds,
                    flash_timeout_rate_mb_per_second
                ),
                sink(server)
            )
            .map_err(fho::Error::from)?;
        }
        FastbootSubcommand::GetVar(cmd) => match cmd.var_name.as_str() {
            "all" => {
                let (client, server) = mpsc::channel(1);
                try_join!(
                    async { interface.get_all_vars(client).await.map_err(|e| anyhow!(e)) },
                    handle_vars(writer, server)
                )
                .map_err(fho::Error::from)?;
            }
            v @ _ => {
                let value = interface
                    .get_var(&v)
                    .await
                    .map_err(|e| anyhow!(e))
                    .map_err(fho::Error::from)?;
                writeln!(writer, "{}: {}", v, value)
                    .map_err(|e| anyhow!(e))
                    .map_err(fho::Error::from)?;
            }
        },
        FastbootSubcommand::Stage(cmd) => {
            let (client, server) = mpsc::channel(1);
            try_join!(
                async {
                    interface
                        .stage(cmd.file.as_os_str().to_str().unwrap(), client)
                        .await
                        .map_err(|e| anyhow!(e))
                },
                sink(server)
            )
            .map_err(fho::Error::from)?;
        }
        FastbootSubcommand::Oem(cmd) => {
            interface
                .oem(&cmd.command.join(" ").to_string())
                .await
                .map_err(|e| anyhow!(e))
                .map_err(fho::Error::from)?;
        }
        FastbootSubcommand::Continue(_) => {
            interface.continue_boot().await.map_err(|e| anyhow!(e)).map_err(fho::Error::from)?;
        }
        FastbootSubcommand::Reboot(cmd) => match &cmd.bootloader {
            Some(b) => match b.as_str() {
                "bootloader" => {
                    let (client, server) = mpsc::channel(1);
                    try_join!(
                        async { interface.reboot_bootloader(client).await.map_err(|e| anyhow!(e)) },
                        sink(server)
                    )
                    .map_err(fho::Error::from)?;
                }
                mode => {
                    ffx_bail!("Unsupported mode: {}", mode);
                }
            },
            None => {
                interface.reboot().await.map_err(|e| anyhow!(e)).map_err(fho::Error::from)?;
            }
        },
        FastbootSubcommand::Sparse(cmd) => {
            let download_size = match cmd.size {
                Some(s) => s,
                None => {
                    let max_download_size_var = interface
                        .get_var(MAX_DOWNLOAD_SIZE_VAR)
                        .await
                        .map_err(|e| anyhow!("Communication error with the device: {:?}", e))?;

                    let trimmed_max_download_size_var =
                        max_download_size_var.trim_start_matches("0x");

                    let max_download_size: u64 =
                        u64::from_str_radix(trimmed_max_download_size_var, 16)
                            .expect("Fastboot max download size var was not a valid u32");
                    max_download_size
                }
            };

            let mut file_handle =
                File::open(&cmd.file).map_err(|e| anyhow!(e)).map_err(fho::Error::from)?;
            let file_to_sparse = match SparseReader::is_sparse_file(&mut file_handle) {
                Ok(true) => {
                    // First unsparse it to a temporary file
                    let (mut unsparsed_file, unsparsed_temp_path) =
                        NamedTempFile::new_in(std::env::temp_dir().as_path())
                            .map_err(|e| anyhow!(e))
                            .map_err(fho::Error::from)?
                            .into_parts();
                    unsparse(&mut file_handle, &mut unsparsed_file)?;
                    // Now build sparse filees
                    unsparsed_temp_path.keep().map_err(|e| anyhow!(e)).map_err(fho::Error::from)?
                }
                Err(_) | Ok(false) => cmd.file.clone(),
            };

            log::debug!(
                "About to build sparse files for: {:?}, and put them in: {:?}",
                file_to_sparse,
                cmd.out_dir
            );

            let files = build_sparse_files(
                "test",
                file_to_sparse.to_str().unwrap(),
                &cmd.out_dir,
                download_size,
            )?;
            for (i, file) in files.into_iter().enumerate() {
                let out_path = cmd.out_dir.join(format!("{}-tmp.simg", i));
                file.persist(&out_path).map_err(|e| anyhow!(e)).map_err(fho::Error::from)?;
                log::debug!("Keeping file: {:?}", out_path);
            }
        }
    };
    Ok(())
}

async fn cmd_impl(
    ctx: &EnvironmentContext,
    writer: &mut VerifiedMachineWriter<FastbootMessage>,
    command: &FastbootCommand,
    info: &TargetInfoHolder,
) -> fho::Result<()> {
    let target_state = match &info.target_state {
        Some(FidlTargetState::Fastboot) => {
            // Nothing to do
            tracing::debug!("Target already in Fastboot state");
            let s: discovery::TargetHandle = (**info).clone().try_into()?;
            s.state
        }
        _ => {
            ffx_bail!("This plugin only works when the target is in Fastboot mode");
        }
    };

    let res = match target_state {
        TargetState::Fastboot(fastboot_state) => match fastboot_state.connection_state {
            FastbootConnectionState::Usb => {
                let serial_num = fastboot_state.serial_number;
                let mut proxy = usb_proxy(serial_num).await?;
                fastboot_impl(writer, command, &mut proxy).await
            }
            FastbootConnectionState::Udp(addrs) => {
                let config = FastbootNetworkConnectionConfig::new_udp().await;
                let NetworkConnectionInfo { target_name, addr, fastboot_device_file_path } =
                    gather_connection_info(ctx, info, addrs)?;
                let mut proxy =
                    udp_proxy(target_name, fastboot_device_file_path, &addr, config).await?;
                fastboot_impl(writer, command, &mut proxy).await
            }
            FastbootConnectionState::Tcp(addrs) => {
                let config = FastbootNetworkConnectionConfig::new_tcp().await;
                let NetworkConnectionInfo { target_name, addr, fastboot_device_file_path } =
                    gather_connection_info(ctx, info, addrs)?;
                let mut proxy =
                    tcp_proxy(target_name, fastboot_device_file_path, &addr, config).await?;
                fastboot_impl(writer, command, &mut proxy).await
            }
        },
        _ => {
            ffx_bail!("Could not connect. Target not in fastboot: {}", target_state);
        }
    };
    res
}

#[derive(Debug, PartialEq)]
struct NetworkConnectionInfo {
    target_name: String,
    addr: SocketAddr,
    fastboot_device_file_path: Option<PathBuf>,
}

fn gather_connection_info(
    ctx: &EnvironmentContext,
    info: &TargetInfoHolder,
    addrs: Vec<TargetIpAddr>,
) -> Result<NetworkConnectionInfo> {
    if let Some(addr) = addrs.into_iter().take(1).next() {
        let target_addr: TargetIpAddr = addr.into();
        let socket_addr: SocketAddr = target_addr.into();

        let target_name =
            if let Some(nodename) = &info.nodename { nodename } else { &socket_addr.to_string() };
        let fastboot_device_file_path: Option<PathBuf> =
            ctx.get(fastboot_file_discovery::FASTBOOT_FILE_PATH).ok();
        Ok(NetworkConnectionInfo {
            target_name: target_name.to_owned(),
            addr: socket_addr,
            fastboot_device_file_path,
        })
    } else {
        ffx_bail!("Could not get a valid address for target");
    }
}

////////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use super::*;
    use fidl_fuchsia_developer_ffx::TargetInfo;
    use std::str::FromStr;

    #[fuchsia::test]
    async fn test_gather_connection_info_fails() -> Result<()> {
        let env = ffx_config::test_env().build().await?;
        let target_info: TargetInfoHolder =
            TargetInfo { nodename: Some("Foo".to_string()), ..Default::default() }.into();
        gather_connection_info(&env.context, &target_info, vec![])
            .expect_err("Should fail on no addrs");
        Ok(())
    }

    #[fuchsia::test]
    async fn test_gather_connection_info_success() -> Result<()> {
        let env = ffx_config::test_env()
            .runtime_config(fastboot_file_discovery::FASTBOOT_FILE_PATH, "/foo")
            .build()
            .await?;
        let target_info: TargetInfoHolder =
            TargetInfo { nodename: Some("Foo".to_string()), ..Default::default() }.into();

        let info = gather_connection_info(
            &env.context,
            &target_info,
            vec![TargetIpAddr::from_str("127.0.0.1:8081")?],
        )?;

        assert_eq!(
            info,
            NetworkConnectionInfo {
                target_name: "Foo".to_string(),
                addr: SocketAddr::from_str("127.0.0.1:8081")?,
                fastboot_device_file_path: Some("/foo".into()),
            }
        );
        Ok(())
    }

    #[fuchsia::test]
    async fn test_gather_connection_info_node_name() -> Result<()> {
        let env = ffx_config::test_env()
            .runtime_config(fastboot_file_discovery::FASTBOOT_FILE_PATH, "/foo")
            .build()
            .await?;
        let target_info: TargetInfoHolder =
            TargetInfo { nodename: None, ..Default::default() }.into();

        let info = gather_connection_info(
            &env.context,
            &target_info,
            vec![TargetIpAddr::from_str("127.0.0.1:8081")?],
        )?;

        assert_eq!(
            info,
            NetworkConnectionInfo {
                target_name: "127.0.0.1:8081".to_string(),
                addr: SocketAddr::from_str("127.0.0.1:8081")?,
                fastboot_device_file_path: Some("/foo".into()),
            }
        );
        Ok(())
    }
}
