// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use async_trait::async_trait;
use ffx_writer::SimpleWriter;
use fho::{FfxMain, FfxTool};
use target_holders::moniker;
use {
    ffx_power_system_activity_application_activity_args as args_mod,
    fidl_fuchsia_power_topology_test as fpt,
};

#[derive(FfxTool)]
pub struct ApplicationActivityTool {
    #[command]
    cmd: args_mod::Command,
    #[with(moniker("/core/topology-test-daemon"))]
    system_activity_control: fpt::SystemActivityControlProxy,
}

fho::embedded_plugin!(ApplicationActivityTool);

#[async_trait(?Send)]
impl FfxMain for ApplicationActivityTool {
    type Writer = SimpleWriter;
    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        match self.cmd.subcommand {
            args_mod::SubCommand::Start(_) => {
                start(self.system_activity_control).await?;
            }
            args_mod::SubCommand::Stop(_) => stop(self.system_activity_control).await?,
        };
        Ok(())
    }
}

pub async fn start(system_activity_control: fpt::SystemActivityControlProxy) -> Result<()> {
    let _ = system_activity_control.start_application_activity().await?;
    Ok(())
}

pub async fn stop(system_activity_control: fpt::SystemActivityControlProxy) -> Result<()> {
    let _ = system_activity_control.stop_application_activity().await?;
    Ok(())
}
