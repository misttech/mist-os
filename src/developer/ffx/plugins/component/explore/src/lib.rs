// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result,
    component_debug::{explore::*, query::get_cml_moniker_from_query},
    ffx_component::rcs::connect_to_realm_explorer,
    ffx_component_explore_args::ExploreComponentCommand,
    ffx_core::ffx_plugin,
    fidl_fuchsia_dash::LauncherProxy,
    fidl_fuchsia_developer_remotecontrol as rc,
    moniker::{AbsoluteMoniker, AbsoluteMonikerBase, RelativeMoniker, RelativeMonikerBase},
};

// TODO(https://fxbug.dev/102835): This plugin needs E2E tests.
#[ffx_plugin(LauncherProxy = "core/debug-dash-launcher:expose:fuchsia.dash.Launcher")]
pub async fn explore(
    rcs: rc::RemoteControlProxy,
    launcher_proxy: LauncherProxy,
    cmd: ExploreComponentCommand,
) -> Result<()> {
    let realm_explorer = connect_to_realm_explorer(&rcs).await?;
    let moniker = get_cml_moniker_from_query(&cmd.query, &realm_explorer).await?;

    println!("Moniker: {}", moniker);

    // Convert the absolute moniker into a relative moniker w.r.t. root.
    // LifecycleController expects relative monikers only.
    let relative_moniker = RelativeMoniker::scope_down(&AbsoluteMoniker::root(), &moniker).unwrap();

    let (client, server) = fidl::Socket::create(fidl::SocketOpts::STREAM).unwrap();
    let ns_layout = cmd.ns_layout.unwrap_or(DashNamespaceLayout::NestAllInstanceDirs);

    let stdout = if cmd.command.is_some() { Stdout::buffered() } else { Stdout::raw()? };

    launch_with_socket(
        relative_moniker,
        server,
        cmd.tools,
        cmd.command,
        ns_layout,
        &launcher_proxy,
    )
    .await?;

    connect_socket_to_stdio(client, stdout).await?;

    let exit_code = wait_for_shell_exit(&launcher_proxy).await?;

    std::process::exit(exit_code);
}
