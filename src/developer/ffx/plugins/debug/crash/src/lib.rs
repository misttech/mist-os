// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use async_trait::async_trait;
use ffx_debug_crash_args::CrashCommand;
use ffx_zxdb::Debugger;
use fho::{FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_debugger as fdebugger;
use fidl_fuchsia_exception::ProcessLimboProxy;
use target_holders::moniker;

#[derive(FfxTool)]
pub struct CrashTool {
    #[command]
    _cmd: CrashCommand,
    #[with(moniker("/core/exceptions"))]
    limbo_proxy: ProcessLimboProxy,
    #[with(moniker("/core/debugger"))]
    launcher_proxy: fdebugger::LauncherProxy,
}

fho::embedded_plugin!(CrashTool);

#[async_trait(?Send)]
impl FfxMain for CrashTool {
    type Writer = SimpleWriter;

    async fn main(self, mut _writer: Self::Writer) -> fho::Result<()> {
        crash_tool_impl(self.limbo_proxy, self.launcher_proxy).await?;
        Ok(())
    }
}

async fn run_debugger(launcher_proxy: fdebugger::LauncherProxy) -> Result<()> {
    let mut debugger = Debugger::launch(launcher_proxy).await?;
    debugger.command.push_str("--auto-attach-limbo");
    debugger.run().await
}

async fn crash_tool_impl(
    limbo_proxy: ProcessLimboProxy,
    launcher_proxy: fdebugger::LauncherProxy,
) -> Result<()> {
    limbo_proxy.set_active(true).await?;
    let run_result = run_debugger(launcher_proxy).await;
    limbo_proxy.set_active(false).await?;

    run_result?;
    Ok(())
}
