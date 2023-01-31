// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result, component_debug::cli::create_cmd, errors::FfxError,
    ffx_component::rcs::connect_to_lifecycle_controller,
    ffx_component_create_args::CreateComponentCommand, ffx_core::ffx_plugin,
    fidl_fuchsia_developer_remotecontrol as rc,
};

#[ffx_plugin]
pub async fn cmd(rcs_proxy: rc::RemoteControlProxy, args: CreateComponentCommand) -> Result<()> {
    let lifecycle_controller = connect_to_lifecycle_controller(&rcs_proxy).await?;

    // All errors from component_debug library are user-visible.
    create_cmd(args.url, args.moniker, lifecycle_controller, std::io::stdout())
        .await
        .map_err(|e| FfxError::Error(e, 1))?;
    Ok(())
}
