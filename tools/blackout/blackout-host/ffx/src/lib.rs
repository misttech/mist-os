// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use ffx_storage_blackout_step_args::{
    BlackoutCommand, BlackoutSubcommand, SetupCommand, TestCommand, VerifyCommand,
};
use fho::{bug, user_error, FfxMain, FfxTool, SimpleWriter};
use fidl::endpoints::DiscoverableProtocolMarker;
use fidl_fuchsia_io::OpenFlags;
use fuchsia_zircon_status::Status;
use {
    fidl_fuchsia_blackout_test as fblackout,
    fidl_fuchsia_developer_remotecontrol as fremotecontrol, fidl_fuchsia_sys2 as fsys,
};

/// Connect to a protocol on a remote device using the remote control proxy.
async fn remotecontrol_connect<S: DiscoverableProtocolMarker>(
    remote_control: &fremotecontrol::RemoteControlProxy,
    moniker: &str,
) -> Result<S::Proxy> {
    let (proxy, server_end) = fidl::endpoints::create_proxy::<S>()?;
    remote_control
        .open_capability(
            moniker,
            fsys::OpenDirType::ExposedDir,
            S::PROTOCOL_NAME,
            server_end.into_channel(),
            OpenFlags::empty(),
        )
        .await?
        .map_err(|e| {
            anyhow::anyhow!(
                "failed to connect to protocol {} at {}: {:?}",
                S::PROTOCOL_NAME.to_string(),
                moniker,
                e
            )
        })?;
    Ok(proxy)
}

#[derive(FfxTool)]
pub struct BlackoutTool {
    #[command]
    pub cmd: BlackoutCommand,
    remote_control: fremotecontrol::RemoteControlProxy,
}

fho::embedded_plugin!(BlackoutTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for BlackoutTool {
    type Writer = SimpleWriter;
    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        let proxy = remotecontrol_connect::<fblackout::ControllerMarker>(
            &self.remote_control,
            "/core/ffx-laboratory:blackout-target",
        )
        .await?;

        let BlackoutCommand { step } = self.cmd;
        match step {
            BlackoutSubcommand::Setup(SetupCommand { device_label, device_path, seed }) => proxy
                .setup(&device_label, device_path.as_deref(), seed)
                .await
                .map_err(|e| bug!(e))?
                .map_err(|e| {
                    anyhow::anyhow!("setup failed: {}", Status::from_raw(e).to_string())
                })?,
            BlackoutSubcommand::Test(TestCommand { device_label, device_path, seed, duration }) => {
                proxy
                    .test(&device_label, device_path.as_deref(), seed, duration)
                    .await
                    .map_err(|e| bug!(e))?
                    .map_err(|e| {
                        anyhow::anyhow!("test step failed: {}", Status::from_raw(e).to_string())
                    })?
            }
            BlackoutSubcommand::Verify(VerifyCommand { device_label, device_path, seed }) => proxy
                .verify(&device_label, device_path.as_deref(), seed)
                .await
                .map_err(|e| bug!(e))?
                .map_err(|e| {
                    let status = Status::from_raw(e);
                    if status == Status::BAD_STATE {
                        user_error!("verification failure")
                    } else {
                        user_error!("retry-able verify step error: {}", status.to_string())
                    }
                })?,
        };

        Ok(())
    }
}
