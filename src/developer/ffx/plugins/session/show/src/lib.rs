// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Context, Result},
    async_trait::async_trait,
    component_debug::cli::show_cmd_print,
    ffx_session_show_args::SessionShowCommand,
    fho::{FfxMain, FfxTool, SimpleWriter},
    fidl_fuchsia_developer_remotecontrol as rc, fidl_fuchsia_sys2 as fsys,
    fuchsia_zircon_status::Status,
};

const DETAILS_FAILURE: &str = "Could not get session information from the target. This may be
because there are no running sessions, or because the target is using a product configuration
that does not include `session_manager`.";

#[derive(FfxTool)]
pub struct ShowTool {
    #[command]
    cmd: SessionShowCommand,
    rcs: rc::RemoteControlProxy,
}

fho::embedded_plugin!(ShowTool);

#[async_trait(?Send)]
impl FfxMain for ShowTool {
    type Writer = SimpleWriter;
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        show_impl(self.rcs, self.cmd, &mut writer).await?;
        Ok(())
    }
}

async fn show_impl<W: std::io::Write>(
    rcs_proxy: rc::RemoteControlProxy,
    _cmd: SessionShowCommand,
    writer: &mut W,
) -> Result<()> {
    let (query_proxy, query_server) = fidl::endpoints::create_proxy::<fsys::RealmQueryMarker>()
        .context("creating query proxy")?;
    rcs_proxy
        .root_realm_query(query_server)
        .await?
        .map_err(|i| Status::ok(i).unwrap_err())
        .context("opening realm query")?;

    show_cmd_print("core/session-manager/session:session".to_string(), query_proxy, writer)
        .await
        .context(DETAILS_FAILURE)?;

    Ok(())
}
