// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result,
    component_debug::cli::start_cmd,
    errors::FfxError,
    ffx_component::rcs::{connect_to_lifecycle_controller, connect_to_realm_explorer},
    ffx_component_start_args::ComponentStartCommand,
    ffx_core::ffx_plugin,
    fidl_fuchsia_developer_remotecontrol as rc,
};

#[ffx_plugin]
pub async fn cmd(rcs_proxy: rc::RemoteControlProxy, args: ComponentStartCommand) -> Result<()> {
    let lifecycle_controller = connect_to_lifecycle_controller(&rcs_proxy).await?;
    let realm_explorer = connect_to_realm_explorer(&rcs_proxy).await?;

    // All errors from component_debug library are user-visible.
    start_cmd(args.query, lifecycle_controller, realm_explorer, std::io::stdout())
        .await
        .map_err(|e| FfxError::Error(e, 1))?;
    Ok(())
}
