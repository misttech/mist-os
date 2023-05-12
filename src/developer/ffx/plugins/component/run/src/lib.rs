// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use component_debug::cli::run_cmd;
use ffx_component::rcs::connect_to_lifecycle_controller;
use ffx_component_run_args::RunComponentCommand;
use ffx_core::ffx_plugin;
use ffx_log::{error::LogError, log_impl, LogOpts};
use ffx_log_args::LogCommand;
use ffx_log_frontend::RemoteDiagnosticsBridgeProxyWrapper;
use fidl_fuchsia_developer_ffx::TargetCollectionProxy;
use fidl_fuchsia_developer_remotecontrol as rc;
use std::sync::Arc;

async fn cmd_impl(
    target_collection_proxy: TargetCollectionProxy,
    rcs_proxy: rc::RemoteControlProxy,
    args: RunComponentCommand,
) -> Result<(), LogError> {
    let lifecycle_controller = connect_to_lifecycle_controller(&rcs_proxy).await?;
    let host = rcs_proxy.identify_host().await??;

    // All errors from component_debug library are user-visible.
    run_cmd(
        args.moniker.clone(),
        args.url,
        args.recreate,
        args.connect_stdio,
        lifecycle_controller,
        std::io::stdout(),
    )
    .await?;

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
            &mut std::io::stdout(),
            LogOpts::default(),
        )
        .await?;
    }
    Ok(())
}

#[ffx_plugin(TargetCollectionProxy = "daemon::protocol")]
pub async fn cmd(
    target_collection_proxy: TargetCollectionProxy,
    rcs_proxy: rc::RemoteControlProxy,
    args: RunComponentCommand,
) -> Result<()> {
    cmd_impl(target_collection_proxy, rcs_proxy, args).await?;
    Ok(())
}
