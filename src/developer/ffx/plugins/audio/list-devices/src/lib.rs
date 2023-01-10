// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result,
    errors::ffx_bail,
    ffx_audio_listdevices_args::ListDevicesCommand,
    ffx_core::ffx_plugin,
    fidl_fuchsia_audio_ffxdaemon::AudioDaemonProxy,
    serde::{Deserialize, Serialize},
};

#[derive(Debug, Serialize, Deserialize)]
pub struct ListDeviceResult {
    pub devices: Vec<String>,
}

#[ffx_plugin(
    "audio",
    AudioDaemonProxy = "core/audio_ffx_daemon:expose:fuchsia.audio.ffxdaemon.AudioDaemon"
)]
pub async fn list_devices_cmd(
    audio_proxy: AudioDaemonProxy,
    cmd: ListDevicesCommand,
) -> Result<()> {
    let devices = match audio_proxy.list_devices().await? {
        Ok(value) => value.devices.ok_or(anyhow::anyhow!("No input devices"))?,
        Err(err) => ffx_bail!("Record failed with err: {}", err),
    };

    match cmd.output {
        ffx_audio_listdevices_args::InfoOutputFormat::Json => {
            let json_list = serde_json::to_string(&ListDeviceResult { devices })?;

            println!("{}", json_list);
        }
        ffx_audio_listdevices_args::InfoOutputFormat::Text => {
            for device in devices.iter() {
                println!("{}", device);
            }
        }
    };

    Ok(())
}
