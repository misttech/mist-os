// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context as _, Result};
use async_trait::async_trait;
use component_debug::{cli::run_cmd, config::resolve_raw_config_overrides};
use ffx_component::rcs::{connect_to_lifecycle_controller, connect_to_realm_query};
use ffx_component_run_args::RunComponentCommand;
use ffx_core::macro_deps::errors::FfxError;
use ffx_log::{error::LogError, log_impl, LogOpts};
use ffx_log_args::LogCommand;
use ffx_log_frontend::RemoteDiagnosticsBridgeProxyWrapper;
use fho::{daemon_protocol, FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_developer_ffx::TargetCollectionProxy;
use fidl_fuchsia_developer_remotecontrol as rc;
use std::sync::Arc;

async fn cmd_impl(
    target_collection_proxy: TargetCollectionProxy,
    rcs_proxy: rc::RemoteControlProxy,
    args: RunComponentCommand,
    mut writer: SimpleWriter,
) -> Result<(), anyhow::Error> {
    let lifecycle_controller = connect_to_lifecycle_controller(&rcs_proxy).await?;
    let host = rcs_proxy
        .identify_host()
        .await?
        .map_err(|err| anyhow!("Error identifying host: {:?}", err))?;
    let realm_query = connect_to_realm_query(&rcs_proxy).await?;

    let config_overrides = resolve_raw_config_overrides(
        &realm_query,
        &args.moniker,
        &args.url.to_string(),
        &args.config,
    )
    .await
    .context("resolving config overridese")?;

    // All errors from component_debug library are user-visible.
    run_cmd(
        args.moniker.clone(),
        args.url,
        args.recreate,
        args.connect_stdio,
        config_overrides,
        lifecycle_controller,
        &mut writer,
    )
    .await
    .map_err(|e| FfxError::Error(e, 1))?;

    if args.follow_logs {
        let log_filter = args.moniker.to_string().strip_prefix("/").unwrap().to_string();
        let log_cmd = LogCommand { filter: vec![log_filter], ..LogCommand::default() };

        log_impl(
            Arc::new(RemoteDiagnosticsBridgeProxyWrapper::new(
                target_collection_proxy,
                host.nodename.ok_or(LogError::NoHostname)?,
            )),
            Some(rcs_proxy),
            &None,
            log_cmd,
            &mut writer,
            LogOpts::default(),
        )
        .await?;
    }
    Ok(())
}

#[derive(FfxTool)]
pub struct RunTool {
    #[command]
    cmd: RunComponentCommand,
    rcs: rc::RemoteControlProxy,
    #[with(daemon_protocol())]
    target_collection: TargetCollectionProxy,
}

fho::embedded_plugin!(RunTool);

#[async_trait(?Send)]
impl FfxMain for RunTool {
    type Writer = SimpleWriter;

    async fn main(self, writer: Self::Writer) -> fho::Result<()> {
        cmd_impl(self.target_collection, self.rcs, self.cmd, writer).await?;
        Ok(())
    }
}
