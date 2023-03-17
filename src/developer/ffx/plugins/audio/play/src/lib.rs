// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result,
    ffx_audio_play_args::{
        AudioRenderUsageExtended::{
            Background, Communication, Interruption, Media, SystemAgent, Ultrasound,
        },
        PlayCommand,
    },
    ffx_core::ffx_plugin,
    fidl_fuchsia_audio_ffxdaemon::{AudioDaemonPlayRequest, AudioDaemonProxy},
};

#[ffx_plugin(
    "audio",
    AudioDaemonProxy = "core/audio_ffx_daemon:expose:fuchsia.audio.ffxdaemon.AudioDaemon"
)]
pub async fn play_cmd(audio_proxy: AudioDaemonProxy, cmd: PlayCommand) -> Result<()> {
    let (play_remote, play_local) = fidl::Socket::create_datagram();

    let renderer = match cmd.usage {
        Ultrasound => fidl_fuchsia_audio_ffxdaemon::RendererType::UltrasoundRenderer(
            fidl_fuchsia_audio_ffxdaemon::UltrasoundRenderer {},
        ),

        Background(usage) | Media(usage) | SystemAgent(usage) | Communication(usage)
        | Interruption(usage) => fidl_fuchsia_audio_ffxdaemon::RendererType::StandardRenderer(
            fidl_fuchsia_audio_ffxdaemon::RendererConfig {
                usage: Some(usage),
                clock: Some(cmd.clock),
                ..fidl_fuchsia_audio_ffxdaemon::RendererConfig::EMPTY
            },
        ),
    };

    let request = AudioDaemonPlayRequest {
        socket: Some(play_remote),
        location: Some(fidl_fuchsia_audio_ffxdaemon::PlayLocation::Renderer(renderer)),
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
