// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Context, Result},
    driver_tools::args::DriverCommand,
    fidl_fuchsia_device_manager as fdm, fidl_fuchsia_driver_development as fdd,
    fidl_fuchsia_driver_playground as fdp, fidl_fuchsia_driver_registrar as fdr,
    fidl_fuchsia_io as fio, fidl_fuchsia_test_manager as ftm, fuchsia_async as fasync,
    fuchsia_component::client,
};

struct DriverConnector {}

impl DriverConnector {
    fn new() -> Self {
        Self {}
    }
}

#[async_trait::async_trait]
impl driver_connector::DriverConnector for DriverConnector {
    async fn get_driver_development_proxy(
        &self,
        select: bool,
    ) -> Result<fdd::DriverDevelopmentProxy> {
        if select {
            anyhow::bail!("The 'driver' tool cannot use the select flag. Please use 'ffx driver' in order to select a component.");
        }
        client::connect_to_protocol::<fdd::DriverDevelopmentMarker>()
            .context("Failed to connect to driver development service")
    }

    async fn get_dev_proxy(&self, select: bool) -> Result<fio::DirectoryProxy> {
        if select {
            anyhow::bail!("The 'driver' tool cannot use the select flag. Please use 'ffx driver' in order to select a component.");
        }
        fuchsia_fs::directory::open_in_namespace("/dev", fuchsia_fs::OpenFlags::empty())
            .map_err(Into::into)
    }

    async fn get_device_watcher_proxy(&self) -> Result<fdm::DeviceWatcherProxy> {
        fuchsia_component::client::connect_to_protocol_at_path::<fdm::DeviceWatcherMarker>(
            "/svc/fuchsia.hardware.usb.DeviceWatcher",
        )
    }

    async fn get_driver_registrar_proxy(&self, select: bool) -> Result<fdr::DriverRegistrarProxy> {
        if select {
            anyhow::bail!("The 'driver' tool cannot use the select flag. Please use 'ffx driver' in order to select a component.");
        }
        client::connect_to_protocol::<fdr::DriverRegistrarMarker>()
            .context("Failed to connect to driver registrar service")
    }

    async fn get_tool_runner_proxy(&self, select: bool) -> Result<fdp::ToolRunnerProxy> {
        if select {
            anyhow::bail!("The 'driver' tool cannot use the select flag. Please use 'ffx driver' in order to select a component.");
        }
        client::connect_to_protocol::<fdp::ToolRunnerMarker>()
            .context("Failed to connect to tool runner service")
    }

    async fn get_run_builder_proxy(&self) -> Result<ftm::RunBuilderProxy> {
        unreachable!();
    }
}

#[fasync::run_singlethreaded]
async fn main() -> Result<()> {
    let cmd: DriverCommand = argh::from_env();
    driver_tools::driver(cmd, DriverConnector::new(), &mut std::io::stdout()).await
}
