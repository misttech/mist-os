// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use component_debug::{cli::create_cmd, config::resolve_raw_config_overrides};
use errors::FfxError;
use ffx_component::rcs::{connect_to_lifecycle_controller, connect_to_realm_query};
use ffx_component_create_args::CreateComponentCommand;
use ffx_core::ffx_plugin;
use fidl_fuchsia_developer_remotecontrol as rc;

#[ffx_plugin]
pub async fn cmd(rcs_proxy: rc::RemoteControlProxy, args: CreateComponentCommand) -> Result<()> {
    let lifecycle_controller = connect_to_lifecycle_controller(&rcs_proxy).await?;
    let realm_query = connect_to_realm_query(&rcs_proxy).await?;

    let config_overrides = resolve_raw_config_overrides(
        &realm_query,
        &args.moniker,
        &args.url.to_string(),
        &args.config,
    )
    .await?;

    // All errors from component_debug library are user-visible.
    create_cmd(args.url, args.moniker, config_overrides, lifecycle_controller, std::io::stdout())
        .await
        .map_err(|e| FfxError::Error(e, 1))?;
    Ok(())
}
