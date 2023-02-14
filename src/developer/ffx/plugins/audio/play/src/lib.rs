// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result,
    ffx_audio_play_args::PlayCommand,
    ffx_core::ffx_plugin,
    fidl_fuchsia_audio_ffxdaemon::{AudioDaemonPlayRequest, AudioDaemonProxy},
};

#[ffx_plugin(
    "audio",
    AudioDaemonProxy = "core/audio_ffx_daemon:expose:fuchsia.audio.ffxdaemon.AudioDaemon"
)]
pub async fn play_cmd(audio_proxy: AudioDaemonProxy, cmd: PlayCommand) -> Result<()> {
    let (play_remote, play_local) = fidl::Socket::create_datagram();

    let request = AudioDaemonPlayRequest {
        socket: Some(play_remote),
        location: Some(fidl_fuchsia_audio_ffxdaemon::PlayLocation::Renderer(
            fidl_fuchsia_audio_ffxdaemon::RendererInfo {
                usage: Some(cmd.usage),
                clock: Some(cmd.clock),
                ..fidl_fuchsia_audio_ffxdaemon::RendererInfo::EMPTY
            },
        )),

        gain_settings: Some(fidl_fuchsia_audio_ffxdaemon::GainSettings {
            mute: Some(cmd.mute),
            gain: Some(cmd.gain),
            ..fidl_fuchsia_audio_ffxdaemon::GainSettings::EMPTY
        }),
        ..AudioDaemonPlayRequest::EMPTY
    };

    ffx_audio_common::play(request, audio_proxy, play_local).await?;
    Ok(())
}
