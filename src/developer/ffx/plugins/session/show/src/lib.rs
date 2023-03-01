// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Context, Result},
    component_debug::cli::show_cmd_print,
    ffx_core::ffx_plugin,
    ffx_session_show_args::SessionShowCommand,
    fidl_fuchsia_developer_remotecontrol as rc, fidl_fuchsia_sys2 as fsys,
    fuchsia_zircon_status::Status,
};

const DETAILS_FAILURE: &str = "Could not get session information from the target. This may be
because there are no running sessions, or because the target is using a product configuration
that does not support the session framework.";

#[ffx_plugin()]
async fn show(rcs_proxy: rc::RemoteControlProxy, _cmd: SessionShowCommand) -> Result<()> {
    let (query_proxy, query_server) = fidl::endpoints::create_proxy::<fsys::RealmQueryMarker>()
        .context("creating query proxy")?;
    rcs_proxy
        .root_realm_query(query_server)
        .await?
        .map_err(|i| Status::ok(i).unwrap_err())
        .context("opening realm query")?;

    show_cmd_print("session:session".to_string(), query_proxy, std::io::stdout())
        .await
        .context(DETAILS_FAILURE)?;

    Ok(())
}
