// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use addr::TargetAddr;
use anyhow::Result;
use async_trait::async_trait;
use errors::FfxError;
use ffx_config::EnvironmentContext;
use ffx_get_ssh_address_args::GetSshAddressCommand;
use fho::{daemon_protocol, FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_developer_ffx::{
    DaemonError, TargetAddrInfo, TargetCollectionProxy, TargetMarker, TargetQuery,
};
use std::io::Write;
use std::net::IpAddr;
use std::time::Duration;
use timeout::timeout;

#[derive(FfxTool)]
pub struct GetSshAddressTool {
    #[command]
    cmd: GetSshAddressCommand,
    #[with(daemon_protocol())]
    collection_proxy: TargetCollectionProxy,
    context: EnvironmentContext,
}

fho::embedded_plugin!(GetSshAddressTool);

#[async_trait(?Send)]
impl FfxMain for GetSshAddressTool {
    type Writer = SimpleWriter;
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        get_ssh_address_impl(self.collection_proxy, self.cmd, self.context, &mut writer).await?;
        Ok(())
    }
}

// This constant can be removed, and the implementation can assert that a port
// always comes from the daemon after some transition period (~May '21).
const DEFAULT_SSH_PORT: u16 = 22;

async fn get_ssh_address_impl<W: Write>(
    collection_proxy: TargetCollectionProxy,
    cmd: GetSshAddressCommand,
    context: EnvironmentContext,
    writer: &mut W,
) -> Result<()> {
    let timeout_dur = Duration::from_secs_f64(cmd.timeout()?);
    let (proxy, handle) = fidl::endpoints::create_proxy::<TargetMarker>();
    let target_spec: Option<String> = ffx_target::get_target_specifier(&context).await?;
    let ts_clone = target_spec.clone();
    let ts_clone_2 = target_spec.clone();
    let res = timeout(timeout_dur, async {
        collection_proxy
            .open_target(&TargetQuery { string_matcher: target_spec, ..Default::default() }, handle)
            .await?
            .map_err(|err| {
                anyhow::Error::from(FfxError::OpenTargetError { err, target: ts_clone_2 })
            })?;
        proxy.get_ssh_address().await.map_err(anyhow::Error::from)
    })
    .await
    .map_err(|_| FfxError::DaemonError { err: DaemonError::Timeout, target: ts_clone })??;

    let (addr, port) = match res {
        TargetAddrInfo::Ip(ref _info) => {
            let target = TargetAddr::from(&res);
            (target, 0)
        }
        TargetAddrInfo::IpPort(ref info) => {
            let target = TargetAddr::from(&res);
            (target, info.port)
        }
    };
    match addr.ip() {
        IpAddr::V4(_) => {
            write!(writer, "{}", addr)?;
        }
        IpAddr::V6(_) => {
            write!(writer, "[{}]", addr)?;
        }
    }
    write!(writer, ":{}", if port == 0 { DEFAULT_SSH_PORT } else { port })?;
    writeln!(writer)?;
    Ok(())
}
