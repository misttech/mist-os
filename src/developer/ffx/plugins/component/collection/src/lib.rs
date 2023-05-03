// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use component_debug::cli::{collection_list_cmd, collection_show_cmd};
use errors::FfxError;
use ffx_component::rcs::connect_to_realm_query;
use ffx_component_collection_args::{CollectionCommand, ShowArgs, SubCommandEnum};
use ffx_core::ffx_plugin;
use fidl_fuchsia_developer_remotecontrol as rc;

#[ffx_plugin()]
pub async fn cmd(rcs_proxy: rc::RemoteControlProxy, args: CollectionCommand) -> Result<()> {
    let realm_query = connect_to_realm_query(&rcs_proxy).await?;

    // All errors from component_debug library are user-visible.
    match args.subcommand {
        SubCommandEnum::List(_) => collection_list_cmd(realm_query, std::io::stdout()).await,
        SubCommandEnum::Show(ShowArgs { query }) => {
            collection_show_cmd(query, realm_query, std::io::stdout()).await
        }
    }
    .map_err(|e| FfxError::Error(e, 1))?;

    Ok(())
}
