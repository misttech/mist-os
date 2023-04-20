// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use errors::ffx_bail;
use ffx_inspect_args::{InspectCommand, InspectSubCommand};
use ffx_inspect_common::{run_command, InspectOutput};
use fho::{deferred, selector, Deferred, FfxMain, FfxTool, MachineWriter};
use fidl_fuchsia_developer_remotecontrol::{RemoteControlProxy, RemoteDiagnosticsBridgeProxy};

mod commands;

fho::embedded_plugin!(InspectTool);

#[derive(FfxTool)]
pub struct InspectTool {
    #[command]
    cmd: InspectCommand,
    #[with(deferred(selector("core/remote-diagnostics-bridge:expose:fuchsia.developer.remotecontrol.RemoteDiagnosticsBridge")))]
    remote_diagnostics_bridge: Deferred<RemoteDiagnosticsBridgeProxy>,
    rcs: Deferred<RemoteControlProxy>,
}

#[async_trait::async_trait(?Send)]
impl FfxMain for InspectTool {
    type Writer = MachineWriter<InspectOutput>;

    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        let Self { rcs, remote_diagnostics_bridge, cmd } = self;
        let (Ok(rcs), Ok(remote_diagnostics_bridge)) =
            futures::future::join(rcs, remote_diagnostics_bridge).await else {
                ffx_bail!("Failed to connect to necessary emote Control protocols")
            };
        match cmd.sub_command {
            InspectSubCommand::ApplySelectors(cmd) => {
                commands::apply_selectors::execute(rcs, remote_diagnostics_bridge, cmd).await?;
            }
            InspectSubCommand::Show(cmd) => {
                run_command(rcs, remote_diagnostics_bridge, cmd, &mut writer).await?;
            }
            InspectSubCommand::List(cmd) => {
                run_command(rcs, remote_diagnostics_bridge, cmd, &mut writer).await?;
            }
            InspectSubCommand::ListAccessors(cmd) => {
                run_command(rcs, remote_diagnostics_bridge, cmd, &mut writer).await?;
            }
            InspectSubCommand::ListFiles(cmd) => {
                run_command(rcs, remote_diagnostics_bridge, cmd, &mut writer).await?;
            }
            InspectSubCommand::Selectors(cmd) => {
                run_command(rcs, remote_diagnostics_bridge, cmd, &mut writer).await?;
            }
        }
        Ok(())
    }
}
