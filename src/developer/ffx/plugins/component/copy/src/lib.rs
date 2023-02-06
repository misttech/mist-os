// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use component_debug::copy::copy_cmd;
use errors::ffx_bail;
use ffx_component::rcs::connect_to_realm_query;
use ffx_component_copy_args::CopyComponentCommand;
use ffx_core::ffx_plugin;
use fidl_fuchsia_developer_remotecontrol as rc;

#[ffx_plugin]
pub async fn component_copy(
    rcs_proxy: rc::RemoteControlProxy,
    cmd: CopyComponentCommand,
) -> Result<()> {
    let query_proxy = connect_to_realm_query(&rcs_proxy).await?;
    let CopyComponentCommand { paths, verbose } = cmd;

    match copy_cmd(&query_proxy, paths, verbose).await {
        Ok(_) => Ok(()),
        Err(e) => ffx_bail!("{}", e),
    }
}
