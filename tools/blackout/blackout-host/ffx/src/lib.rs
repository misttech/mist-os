// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result,
    ffx_core::ffx_plugin,
    ffx_storage_blackout_step_args::{
        BlackoutCommand, BlackoutSubcommand, SetupCommand, TestCommand, VerifyCommand,
    },
    fidl::endpoints::ProtocolMarker,
    fidl_fuchsia_blackout_test as fblackout,
    fidl_fuchsia_developer_remotecontrol as fremotecontrol,
    fidl_fuchsia_io::OpenFlags,
    fuchsia_zircon_status::Status,
};

/// Connect to a protocol on a remote device using the remote control proxy.
async fn remotecontrol_connect<S: ProtocolMarker>(
    remote_control: &fremotecontrol::RemoteControlProxy,
    moniker: &str,
) -> Result<S::Proxy> {
    let (proxy, server_end) = fidl::endpoints::create_proxy::<S>()?;
    remote_control
        .connect_capability(
            moniker,
            S::DEBUG_NAME,
            server_end.into_channel(),
            OpenFlags::RIGHT_READABLE,
        )
        .await?
        .map_err(|e| {
            anyhow::anyhow!(
                "failed to connect to protocol {} at {}: {:?}",
                S::DEBUG_NAME.to_string(),
                moniker,
                e
            )
        })?;
    Ok(proxy)
}

#[ffx_plugin("storage_dev")]
async fn step(
    cmd: BlackoutCommand,
    remote_control: fremotecontrol::RemoteControlProxy,
) -> Result<()> {
    let proxy = remotecontrol_connect::<fblackout::ControllerMarker>(
        &remote_control,
        "/core/ffx-laboratory:blackout-target",
    )
    .await?;

    let BlackoutCommand { step } = cmd;
    match step {
        BlackoutSubcommand::Setup(SetupCommand { device_label, device_path, seed }) => proxy
            .setup(&device_label, device_path.as_deref(), seed)
            .await?
            .map_err(|e| anyhow::anyhow!("setup failed: {}", Status::from_raw(e).to_string()))?,
        BlackoutSubcommand::Test(TestCommand { device_label, device_path, seed, duration }) => {
            proxy.test(&device_label, device_path.as_deref(), seed, duration).await?.map_err(
                |e| anyhow::anyhow!("test step failed: {}", Status::from_raw(e).to_string()),
            )?
        }
        BlackoutSubcommand::Verify(VerifyCommand { device_label, device_path, seed }) => {
            proxy.verify(&device_label, device_path.as_deref(), seed).await?.map_err(|e| {
                let status = Status::from_raw(e);
                if status == Status::BAD_STATE {
                    anyhow::anyhow!("verification failure")
                } else {
                    anyhow::anyhow!("retry-able verify step error: {}", status.to_string())
                }
            })?
        }
    };

    Ok(())
}
