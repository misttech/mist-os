// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use discovery::{TargetHandle, TargetState};
use ffx_config::EnvironmentContext;
use ffx_diagnostics::{Check, CheckExt};
use ffx_target_status_args as args;
use ffx_writer::VerifiedMachineWriter;
use fho::{FfxContext, FfxMain, FfxTool};
use std::io::Write;

mod checks;

#[derive(FfxTool)]
pub struct Status {
    #[command]
    cmd: args::TargetStatus,
    ctx: EnvironmentContext,
}

fho::embedded_plugin!(Status);

impl Status {
    async fn check_product_device(
        &self,
        writer: &mut <Self as FfxMain>::Writer,
        device: TargetHandle,
    ) -> fho::Result<()> {
        // Depending on the number of targets resolved and their types,
        // this could go one of several ways. It may also be nice to mention where the devices
        // originated. This does not check VSock devices.
        let (info, writer) = checks::ConnectSsh::new(&self.ctx)
            .check_with_output(device, writer)
            .and_then_check(checks::ConnectRemoteControlProxy::new(
                std::time::Duration::from_secs_f64(self.cmd.proxy_connect_timeout),
            ))
            .await
            .map_err(|e| fho::Error::User(e.into()))?;
        writeln!(writer, "Got device info: {:?}", info).bug()?;
        Ok(())
    }

    async fn check_fastboot_device(
        &self,
        writer: &mut <Self as FfxMain>::Writer,
        device: TargetHandle,
    ) -> fho::Result<()> {
        let (info, writer) = checks::FastbootDeviceStatus::new()
            .check_with_output(device, writer)
            .await
            .map_err(|e| fho::Error::User(e.into()))?;
        writeln!(writer, "Got device info: {:?}", info).bug()?;
        Ok(())
    }
}

#[async_trait(?Send)]
impl FfxMain for Status {
    // This is a machine writer, but there is (currently) no machine output in use yet.
    type Writer = VerifiedMachineWriter<String>;

    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        let (target, writer) = checks::GetTargetSpecifier::new(&self.ctx)
            .check_with_output((), &mut writer)
            .and_then_check(checks::ResolveTarget::new(&self.ctx))
            .await
            .map_err(|e| fho::Error::User(e.into()))?;
        match target.state {
            TargetState::Product { .. } => self.check_product_device(writer, target).await?,
            TargetState::Fastboot(_) => self.check_fastboot_device(writer, target).await?,
            TargetState::Unknown => {
                fho::return_user_error!("Device is in an unknown state. No way to check status.")
            }
            TargetState::Zedboot => {
                fho::return_user_error!("Zedboot is not currently supported for this command.")
            }
        }
        writeln!(writer, "All checks passed.").bug()?;
        Ok(())
    }
}
