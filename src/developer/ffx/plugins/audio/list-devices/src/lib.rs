// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fho::FfxContext;

use {
    anyhow::{Context, Result},
    async_trait::async_trait,
    ffx_audio_listdevices_args::ListDevicesCommand,
    fho::{selector, FfxMain, FfxTool, MachineWriter},
    fidl_fuchsia_audio_ffxdaemon::AudioDaemonProxy,
    fuchsia_zircon_status::Status,
    itertools::Itertools,
    serde::{Deserialize, Serialize},
    std::io::Write,
};

#[derive(Debug, Serialize, Deserialize)]
pub struct ListDeviceResult {
    pub devices: Vec<String>,
}

#[derive(FfxTool)]
pub struct ListDevicesTool {
    #[command]
    _cmd: ListDevicesCommand,
    #[with(selector("core/audio_ffx_daemon:expose:fuchsia.audio.ffxdaemon.AudioDaemon"))]
    audio_proxy: AudioDaemonProxy,
}

fho::embedded_plugin!(ListDevicesTool);
#[async_trait(?Send)]
impl FfxMain for ListDevicesTool {
    type Writer = MachineWriter<ListDeviceResult>;
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        let response = self
            .audio_proxy
            .list_devices()
            .await
            .context("List devices failed")?
            .map_err(Status::from_raw)
            .context("Error from daemon for list devices request")?;

        if let Some(devices) = response.devices {
            writer
                .machine_or_else(&ListDeviceResult { devices: devices.clone() }, || {
                    devices.iter().map(|device| device.as_str()).join("\n")
                })
                .map_err(Into::into)
        } else {
            writeln!(writer, "No devices found.").bug()
        }
    }
}
