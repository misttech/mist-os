// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use component_debug::cli::{list_cmd_print, list_cmd_serialized};
use component_debug::list::Instance;
use errors::FfxError;
use ffx_component::rcs::{connect_to_realm_explorer, connect_to_realm_query};
use ffx_component_list_args::ComponentListCommand;
use ffx_core::ffx_plugin;
use ffx_writer::Writer;
use fidl_fuchsia_developer_remotecontrol as rc;

#[ffx_plugin]
pub async fn cmd(
    rcs_proxy: rc::RemoteControlProxy,
    args: ComponentListCommand,
    #[ffx(machine = Vec<Instance>)] writer: Writer,
) -> Result<()> {
    let realm_explorer = connect_to_realm_explorer(&rcs_proxy).await?;
    let realm_query = connect_to_realm_query(&rcs_proxy).await?;

    // All errors from component_debug library are user-visible.
    if writer.is_machine() {
        let output = list_cmd_serialized(args.filter, realm_query, realm_explorer)
            .await
            .map_err(|e| FfxError::Error(e, 1))?;
        writer.machine(&output)
    } else {
        list_cmd_print(args.filter, args.verbose, realm_query, realm_explorer, writer)
            .await
            .map_err(|e| FfxError::Error(e, 1))?;
        Ok(())
    }
}
