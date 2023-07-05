// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Error,
    async_trait::async_trait,
    ffx_wlan_dev_args as arg_types,
    fho::{moniker, FfxMain, FfxTool, SimpleWriter},
    fidl_fuchsia_wlan_device_service as wlan_service, wlan_dev,
};

#[derive(FfxTool)]
pub struct DevTool {
    #[command]
    cmd: arg_types::DevCommand,
    #[with(moniker("/core/wlandevicemonitor"))]
    monitor_proxy: wlan_service::DeviceMonitorProxy,
}

fho::embedded_plugin!(DevTool);

#[async_trait(?Send)]
impl FfxMain for DevTool {
    type Writer = SimpleWriter;
    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        handle_dev_cmd(self.monitor_proxy, self.cmd).await?;
        Ok(())
    }
}

async fn handle_dev_cmd(
    monitor_proxy: wlan_service::DeviceMonitorProxy,
    cmd: arg_types::DevCommand,
) -> Result<(), Error> {
    wlan_dev::handle_wlantool_command(monitor_proxy, wlan_dev::opts::Opt::from(cmd.subcommand))
        .await
}
