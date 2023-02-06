// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use component_debug::cli::capability_cmd;
use errors::FfxError;
use ffx_component::rcs::{connect_to_realm_explorer, connect_to_realm_query};
use ffx_component_capability_args::ComponentCapabilityCommand;
use ffx_core::ffx_plugin;
use fidl_fuchsia_developer_remotecontrol as rc;

#[ffx_plugin()]
pub async fn cmd(
    rcs_proxy: rc::RemoteControlProxy,
    args: ComponentCapabilityCommand,
) -> Result<()> {
    let realm_explorer = connect_to_realm_explorer(&rcs_proxy).await?;
    let realm_query = connect_to_realm_query(&rcs_proxy).await?;

    // All errors from component_debug library are user-visible.
    capability_cmd(args.capability, realm_query, realm_explorer, std::io::stdout())
        .await
        .map_err(|e| FfxError::Error(e, 1))?;
    Ok(())
}
