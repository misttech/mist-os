// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use component_debug::cli::run_cmd;
use errors::FfxError;
use ffx_component::rcs::connect_to_lifecycle_controller;
use ffx_component_run_args::RunComponentCommand;
use ffx_core::ffx_plugin;
use ffx_log::{log_impl, LogOpts};
use ffx_log_args::LogCommand;
use fidl_fuchsia_developer_ffx::DiagnosticsProxy;
use fidl_fuchsia_developer_remotecontrol as rc;

#[ffx_plugin(DiagnosticsProxy = "daemon::protocol")]
pub async fn cmd(
    diagnostics_proxy: DiagnosticsProxy,
    rcs_proxy: rc::RemoteControlProxy,
    args: RunComponentCommand,
) -> Result<()> {
    let lifecycle_controller = connect_to_lifecycle_controller(&rcs_proxy).await?;

    // All errors from component_debug library are user-visible.
    run_cmd(
        args.moniker.clone(),
        args.url,
        args.recreate,
        args.connect_stdio,
        lifecycle_controller,
        std::io::stdout(),
    )
    .await
    .map_err(|e| FfxError::Error(e, 1))?;

    if args.follow_logs {
        let log_filter = args.moniker.to_string().strip_prefix("/").unwrap().to_string();
        let log_cmd = LogCommand { filter: vec![log_filter], ..LogCommand::default() };

        log_impl(
            diagnostics_proxy,
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
