// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use async_trait::async_trait;
use component_debug::cli::format::format_start_error;
use component_debug::lifecycle::start_instance;
use component_debug::query::get_cml_moniker_from_query;
use errors::FfxError;
use ffx_component::rcs::{connect_to_lifecycle_controller, connect_to_realm_query};
use ffx_component_start_args::ComponentStartCommand;
use ffx_zxdb::Debugger;
use fho::{FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_developer_remotecontrol as rc;
use target_holders::moniker;

#[derive(FfxTool)]
pub struct StartTool {
    #[command]
    cmd: ComponentStartCommand,

    #[with(moniker("/core/debugger"))]
    debugger_proxy: fidl_fuchsia_debugger::LauncherProxy,

    rcs: rc::RemoteControlProxy,
}

fho::embedded_plugin!(StartTool);

#[async_trait(?Send)]
impl FfxMain for StartTool {
    type Writer = SimpleWriter;

    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        start_tool_impl(self).await.map_err(|e| FfxError::Error(e, 1))?;
        Ok(())
    }
}

async fn start_tool_impl(tool: StartTool) -> Result<()> {
    let lifecycle_controller = connect_to_lifecycle_controller(&tool.rcs).await?;
    let realm_query = connect_to_realm_query(&tool.rcs).await?;
    let moniker = get_cml_moniker_from_query(&tool.cmd.query, &realm_query).await?;

    // If the user wants to debug the component, we need to start the debugger with a breakpoint
    // on `_start`, which will give the user a chance to set any further breakpoints they want.
    let maybe_session = if tool.cmd.debug {
        let mut debugger = Debugger::launch(tool.debugger_proxy).await?;
        debugger.command.attach(&format!("{}", moniker));
        debugger.command.break_at("_start");
        let session = debugger.start().await?;
        Some(session)
    } else {
        None
    };

    let _ = start_instance(&lifecycle_controller, &moniker)
        .await
        .map_err(|e| format_start_error(&moniker, e))?;

    // Wait for the user to interactively debug the component.
    if let Some(session) = maybe_session {
        session.wait().await?;
    }
    Ok(())
}
