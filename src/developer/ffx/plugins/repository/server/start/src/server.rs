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
use pkg::ServerMode;

// map from the start command to the serve command.
pub(crate) fn to_serve_command(cmd: &StartCommand) -> ServeCommand {
    ServeCommand {
        address: cmd.address.unwrap_or(ffx_repository_serve_args::default_address()),
        alias: cmd.alias.clone(),
        alias_conflict_mode: cmd.alias_conflict_mode,
        port_path: cmd.port_path.clone(),
        no_device: cmd.no_device,
        product_bundle: cmd.product_bundle.clone(),
        repo_path: cmd.repo_path.clone(),
        repository: cmd.repository.clone(),
        storage_type: cmd.storage_type,
        trusted_root: cmd.trusted_root.clone(),
        refresh_metadata: cmd.refresh_metadata,
    }
}

pub(crate) fn to_argv(cmd: &StartCommand) -> Vec<String> {
    let mut argv: Vec<String> = vec![];
    if let Some(addr) = cmd.address {
        argv.extend_from_slice(&["--address".into(), addr.to_string()]);
    }
    for a in &cmd.alias {
        argv.extend_from_slice(&["--alias".into(), a.to_string()]);
    }
    let mode = match cmd.alias_conflict_mode {
        ffx::RepositoryRegistrationAliasConflictMode::ErrorOut => "error-out".to_string(),
        ffx::RepositoryRegistrationAliasConflictMode::Replace => "replace".to_string(),
    };
    argv.extend_from_slice(&["--alias-conflict-mode".into(), mode]);

    if cmd.no_device {
        argv.push("--no-device".into());
    }

    if let Some(pb) = &cmd.product_bundle {
        argv.extend_from_slice(&["--product-bundle".into(), pb.as_str().to_string()]);
    }

    if let Some(p) = &cmd.repo_path {
        argv.extend_from_slice(&["--repo-path".into(), p.as_str().to_string()]);
    }
    if let Some(name) = &cmd.repository {
        argv.extend_from_slice(&["--repository".into(), name.to_string()]);
    }
    if let Some(s) = cmd.storage_type {
        let st = match s {
            ffx::RepositoryStorageType::Ephemeral => "ephemeral",
            ffx::RepositoryStorageType::Persistent => "Persistent",
        };
        argv.extend_from_slice(&["--storage-type".into(), st.to_string()]);
    }

    if let Some(p) = &cmd.trusted_root {
        argv.extend_from_slice(&["--trusted-root".into(), p.as_str().to_string()]);
    }
    if cmd.refresh_metadata {
        argv.push("--refresh-metadata".into());
    }

    argv
}

pub async fn run_foreground_server(
    start_cmd: StartCommand,
    context: EnvironmentContext,
    target_proxy_connector: Connector<ffx::TargetProxy>,
    rcs_proxy_connector: Connector<RemoteControlProxy>,
    w: <ServerStartTool as FfxMain>::Writer,
    mode: ServerMode,
) -> Result<()> {
    serve_impl(
        target_proxy_connector,
        rcs_proxy_connector,
        to_serve_command(&start_cmd),
        context,
        w.simple_writer(),
        mode,
    )
    .await
}
