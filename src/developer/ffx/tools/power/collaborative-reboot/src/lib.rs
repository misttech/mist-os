// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::io::Write;

use async_trait::async_trait;
use errors::ffx_error;
use ffx_power_collaborative_reboot_args::{
    CollaborativeRebootCommand, PerformPendingRebootCommand, SubCommand,
};
use fho::{FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_power as fpower;
use target_holders::moniker;

#[derive(FfxTool)]
pub struct CollaborativeRebootCmdTool {
    #[command]
    cmd: CollaborativeRebootCommand,
    #[with(moniker("/bootstrap/shutdown_shim"))]
    power_proxy: fpower::CollaborativeRebootInitiatorProxy,
}

fho::embedded_plugin!(CollaborativeRebootCmdTool);

#[async_trait(?Send)]
impl FfxMain for CollaborativeRebootCmdTool {
    type Writer = SimpleWriter;
    async fn main(self, writer: Self::Writer) -> fho::Result<()> {
        collaborative_reboot(writer, self.cmd, self.power_proxy).await
    }
}

async fn collaborative_reboot(
    mut out: SimpleWriter,
    CollaborativeRebootCommand { subcommand }: CollaborativeRebootCommand,
    power_proxy: fpower::CollaborativeRebootInitiatorProxy,
) -> fho::Result<()> {
    match subcommand {
        SubCommand::PerformPendingReboot(PerformPendingRebootCommand {}) => {
            let fpower::CollaborativeRebootInitiatorPerformPendingRebootResponse {
                rebooting, ..
            } = power_proxy
                .perform_pending_reboot()
                .await
                .map_err(|e| ffx_error!("Failed to call PerformPendingReboot: {e}"))?;
            writeln!(out, "rebooting = {rebooting:?}")
                .map_err(|e| ffx_error!("Failed to write output: {e}"))?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use fho::TestBuffers;
    use target_holders::fake_proxy;

    use super::*;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_perform_pending_reboot() {
        let command = CollaborativeRebootCommand {
            subcommand: SubCommand::PerformPendingReboot(PerformPendingRebootCommand {}),
        };

        let power_proxy = fake_proxy(move |req| match req {
            fpower::CollaborativeRebootInitiatorRequest::PerformPendingReboot {
                responder, ..
            } => responder
                .send(&fpower::CollaborativeRebootInitiatorPerformPendingRebootResponse {
                    rebooting: Some(true),
                    ..Default::default()
                })
                .expect("failed to respond"),
        });

        let bufs = TestBuffers::default();
        let writer = SimpleWriter::new_test(&bufs);

        collaborative_reboot(writer, command, power_proxy).await.unwrap();

        assert_eq!(bufs.into_stdout_str(), "rebooting = Some(true)\n");
    }
}
