// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use component_debug::cli::reload_cmd;
use errors::FfxError;
use ffx_component::rcs::{connect_to_lifecycle_controller, connect_to_realm_query};
use ffx_component_reload_args::ReloadComponentCommand;
use ffx_core::ffx_plugin;
use fidl_fuchsia_developer_remotecontrol as rc;

#[ffx_plugin]
pub async fn cmd(rcs_proxy: rc::RemoteControlProxy, args: ReloadComponentCommand) -> Result<()> {
    let lifecycle_controller = connect_to_lifecycle_controller(&rcs_proxy).await?;
    let realm_query = connect_to_realm_query(&rcs_proxy).await?;

    // All errors from component_debug library are user-visible.
    reload_cmd(args.query, lifecycle_controller, realm_query, std::io::stdout())
        .await
        .map_err(|e| FfxError::Error(e, 1))?;
    Ok(())
}
