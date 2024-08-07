// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::ServerStartTool;
use ffx_config::EnvironmentContext;
use ffx_repository_serve::serve_impl;
use ffx_repository_serve_args::ServeCommand;
use ffx_repository_server_start_args::StartCommand;
use fho::{Connector, FfxMain, Result};
use fidl_fuchsia_developer_ffx as ffx;
use fidl_fuchsia_developer_remotecontrol::RemoteControlProxy;

// map from the start command to the serve command.
fn to_serve_command(cmd: StartCommand) -> ServeCommand {
    ServeCommand {
        address: cmd.address.unwrap_or(ffx_repository_serve_args::default_address()),
        alias: cmd.alias,
        alias_conflict_mode: cmd.alias_conflict_mode,
        port_path: cmd.port_path,
        no_device: cmd.no_device,
        product_bundle: cmd.product_bundle,
        repo_path: cmd.repo_path,
        repository: cmd.repository,
        storage_type: cmd.storage_type,
        trusted_root: cmd.trusted_root,
        refresh_metadata: cmd.refresh_metadata,
    }
}

pub async fn run_foreground_server(
    start_cmd: StartCommand,
    context: EnvironmentContext,
    target_proxy_connector: Connector<ffx::TargetProxy>,
    rcs_proxy_connector: Connector<RemoteControlProxy>,
    w: <ServerStartTool as FfxMain>::Writer,
) -> Result<()> {
    serve_impl(
        target_proxy_connector,
        rcs_proxy_connector,
        to_serve_command(start_cmd),
        context,
        w.simple_writer(),
    )
    .await
}
