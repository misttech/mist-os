// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(fxbug.dev/120283): Remove. This is force-included as part of ffx_plugin.
use anyhow as _;
use async_trait::async_trait;
use component_debug::cli::explore_cmd;
use errors::FfxError;
use ffx_component::rcs::connect_to_realm_query;
use ffx_component_explore_args::ExploreComponentCommand;
use fho::{moniker, FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_dash::LauncherProxy;
use fidl_fuchsia_developer_remotecontrol as rc;
use socket_to_stdio::Stdout;

#[derive(FfxTool)]
pub struct ExploreTool {
    #[command]
    cmd: ExploreComponentCommand,
    rcs: rc::RemoteControlProxy,
    #[with(moniker("/core/debug-dash-launcher"))]
    dash_launcher: LauncherProxy,
}

fho::embedded_plugin!(ExploreTool);

// TODO(https://fxbug.dev/102835): This plugin needs E2E tests.
#[async_trait(?Send)]
impl FfxMain for ExploreTool {
    type Writer = SimpleWriter;

    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        let realm_query = connect_to_realm_query(&self.rcs).await?;
        let stdout = if self.cmd.command.is_some() { Stdout::buffered() } else { Stdout::raw()? };

        // All errors from component_debug library are user-visible.
        explore_cmd(
            self.cmd.query,
            self.cmd.ns_layout,
            self.cmd.command,
            self.cmd.tools,
            self.dash_launcher,
            realm_query,
            stdout,
        )
        .await
        .map_err(|e| FfxError::Error(e, 1))?;
        Ok(())
    }
}
