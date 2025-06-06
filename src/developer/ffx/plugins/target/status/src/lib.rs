// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use discovery::{TargetHandle, TargetState};
use ffx_config::EnvironmentContext;
use ffx_diagnostics::{Check, CheckExt, NotificationType, Notifier};
use ffx_target_status_args as args;
use ffx_writer::VerifiedMachineWriter;
use fho::{FfxMain, FfxTool};
use std::io::Write;

mod checks;

#[derive(FfxTool)]
pub struct Status {
    #[command]
    cmd: args::TargetStatus,
    ctx: EnvironmentContext,
}

fho::embedded_plugin!(Status);

struct DefaultNotifier {
    writer: <Status as FfxMain>::Writer,
}

impl Notifier for DefaultNotifier {
    fn update_status(
        &mut self,
        ty: NotificationType,
        status: impl Into<String>,
    ) -> anyhow::Result<()> {
        let prefix = match ty {
            NotificationType::Info => "[i] ",
            NotificationType::Success => "\t[✓] ",
            NotificationType::Warning => "\t[!] ",
            NotificationType::Error => "\t[✗] ",
        };
        writeln!(&mut self.writer, "{}{}", prefix, status.into())?;
        self.writer.flush().map_err(Into::into)
    }
}

impl Status {
    async fn check_product_device(
        &self,
        notifier: &mut DefaultNotifier,
        device: TargetHandle,
    ) -> fho::Result<()> {
        // Depending on the number of targets resolved and their types,
        // this could go one of several ways. It may also be nice to mention where the devices
        // originated. This does not check VSock devices.
        let (info, notifier) = checks::ConnectSsh::new(&self.ctx)
            .check_with_notifier(device, notifier)
            .and_then_check(checks::ConnectRemoteControlProxy::new(
                std::time::Duration::from_secs_f64(self.cmd.proxy_connect_timeout),
            ))
            .await
            .map_err(|e| fho::Error::User(e.into()))?;
        notifier.on_success(format!("Got device info: {:?}", info))?;
        Ok(())
    }

    async fn check_fastboot_device(
        &self,
        notifier: &mut DefaultNotifier,
        device: TargetHandle,
    ) -> fho::Result<()> {
        let (info, notifier) = checks::FastbootDeviceStatus::new()
            .check_with_notifier(device, notifier)
            .await
            .map_err(|e| fho::Error::User(e.into()))?;
        notifier.on_success(format!("Got device info: {:?}", info))?;
        Ok(())
    }
}

#[async_trait(?Send)]
impl FfxMain for Status {
    // This is a machine notifier, but there is (currently) no machine output in use yet.
    type Writer = VerifiedMachineWriter<String>;

    async fn main(self, writer: Self::Writer) -> fho::Result<()> {
        let mut notifier = DefaultNotifier { writer };
        let (target, notifier) = checks::GetTargetSpecifier::new(&self.ctx)
            .check_with_notifier((), &mut notifier)
            .and_then_check(checks::ResolveTarget::new(&self.ctx))
            .await
            .map_err(|e| fho::Error::User(e.into()))?;
        match target.state {
            TargetState::Product { .. } => self.check_product_device(notifier, target).await?,
            TargetState::Fastboot(_) => self.check_fastboot_device(notifier, target).await?,
            TargetState::Unknown => {
                fho::return_user_error!("Device is in an unknown state. No way to check status.")
            }
            TargetState::Zedboot => {
                fho::return_user_error!("Zedboot is not currently supported for this command.")
            }
        }
        notifier.on_success("All checks passed.")?;
        Ok(())
    }
}
