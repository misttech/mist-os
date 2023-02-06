// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use component_debug::cli::explore_cmd;
use errors::FfxError;
use ffx_component::rcs::connect_to_realm_explorer;
use ffx_component_explore_args::ExploreComponentCommand;
use ffx_core::ffx_plugin;
use fidl_fuchsia_dash::LauncherProxy;
use fidl_fuchsia_developer_remotecontrol as rc;

// TODO(https://fxbug.dev/102835): This plugin needs E2E tests.
#[ffx_plugin(LauncherProxy = "core/debug-dash-launcher:expose:fuchsia.dash.Launcher")]
pub async fn cmd(
    rcs: rc::RemoteControlProxy,
    dash_launcher: LauncherProxy,
    args: ExploreComponentCommand,
) -> Result<()> {
    let realm_explorer = connect_to_realm_explorer(&rcs).await?;

    // All errors from component_debug library are user-visible.
    explore_cmd(
        args.query,
        args.ns_layout,
        args.command,
        args.tools,
        dash_launcher,
        realm_explorer,
    )
    .await
    .map_err(|e| FfxError::Error(e, 1))?;
    Ok(())
}
