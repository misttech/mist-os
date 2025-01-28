// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use async_trait::async_trait;
use component_debug::query::get_single_instance_from_query;
use component_debug::realm::{get_runtime, Runtime};
use errors::{ffx_error, FfxError};
use ffx_component::rcs::connect_to_realm_query;
use ffx_component_debug_args::ComponentDebugCommand;
use ffx_zxdb::Debugger;
use fho::{FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_developer_remotecontrol as rc;
use target_holders::moniker;
use zx_types::zx_koid_t;

#[derive(FfxTool)]
pub struct DebugTool {
    #[command]
    cmd: ComponentDebugCommand,

    #[with(moniker("/core/debugger"))]
    debugger_proxy: fidl_fuchsia_debugger::LauncherProxy,

    rcs: rc::RemoteControlProxy,
}

fho::embedded_plugin!(DebugTool);

async fn get_job_koid(runtime: &Runtime) -> Result<zx_koid_t> {
    match runtime {
        Runtime::Elf { job_id, .. } => Ok(*job_id),
        Runtime::Unknown => Err(ffx_error!("Cannot debug non-ELF component.").into()),
    }
}

#[async_trait(?Send)]
impl FfxMain for DebugTool {
    type Writer = SimpleWriter;

    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        let realm_query = connect_to_realm_query(&self.rcs).await?;

        let instance = get_single_instance_from_query(&self.cmd.query, &realm_query)
            .await
            .map_err(|e| FfxError::Error(e, 1))?;
        let runtime =
            get_runtime(&instance.moniker, &realm_query).await.unwrap_or(Runtime::Unknown);
        let job_koid = get_job_koid(&runtime).await?;

        let mut debugger = Debugger::launch(self.debugger_proxy).await?;
        debugger.command.attach_job_koid(job_koid);
        debugger.run().await?;
        Ok(())
    }
}
