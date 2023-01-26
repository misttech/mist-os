// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result,
    blocking::Unblock,
    errors::ffx_bail,
    ffx_audio_play_args::{PlayCommand, SubCommand},
    ffx_core::ffx_plugin,
    fidl::Socket,
    fidl_fuchsia_audio_ffxdaemon::{AudioDaemonPlayRequest, AudioDaemonProxy},
};

#[ffx_plugin(
    "audio",
    AudioDaemonProxy = "core/audio_ffx_daemon:expose:fuchsia.audio.ffxdaemon.AudioDaemon"
)]
pub async fn play_cmd(audio_proxy: AudioDaemonProxy, cmd: PlayCommand) -> Result<()> {
    let (play_remote, play_local) = fidl::Socket::create(fidl::SocketOpts::DATAGRAM)?;
    match cmd.subcommand {
        SubCommand::Render(renderer_command) => {
            let request = AudioDaemonPlayRequest {
                socket: Some(play_remote),
                location: Some(fidl_fuchsia_audio_ffxdaemon::PlayLocation::Renderer(
                    fidl_fuchsia_audio_ffxdaemon::RendererInfo {
                        usage: Some(renderer_command.usage),
                        ..fidl_fuchsia_audio_ffxdaemon::RendererInfo::EMPTY
                    },
                )),

                gain_settings: Some(fidl_fuchsia_audio_ffxdaemon::GainSettings {
                    mute: Some(renderer_command.mute),
                    gain: Some(renderer_command.gain),
                    ..fidl_fuchsia_audio_ffxdaemon::GainSettings::EMPTY
                }),
                ..AudioDaemonPlayRequest::EMPTY
            };
            play(request, audio_proxy, play_local).await?
        }
        SubCommand::Device(device_command) => {
            let request = AudioDaemonPlayRequest {
                socket: Some(play_remote),
                location: Some(fidl_fuchsia_audio_ffxdaemon::PlayLocation::RingBuffer(
                    fidl_fuchsia_audio_ffxdaemon::DeviceSelector {
                        is_input: Some(true),
                        id: Some(device_command.id),
                        ..fidl_fuchsia_audio_ffxdaemon::DeviceSelector::EMPTY
                    },
                )),

                gain_settings: Some(fidl_fuchsia_audio_ffxdaemon::GainSettings {
                    mute: None, // TODO
                    gain: None, // TODO
                    ..fidl_fuchsia_audio_ffxdaemon::GainSettings::EMPTY
                }),
                ..AudioDaemonPlayRequest::EMPTY
            };
            play(request, audio_proxy, play_local).await?;
        }
    }
    Ok(())
}

pub async fn play(
    request: AudioDaemonPlayRequest,
    audio_proxy: AudioDaemonProxy,
    play_local: Socket,
) -> Result<()> {
    let futs = futures::future::try_join(
        async {
            let (stdout_sock, stderr_sock) = match audio_proxy.play(request).await? {
                Ok(value) => (
                    value.stdout.ok_or(anyhow::anyhow!("No stdout socket"))?,
                    value.stderr.ok_or(anyhow::anyhow!("No stderr socket."))?,
                ),
                Err(err) => ffx_bail!("Play failed with err: {}", err),
            };

            let mut stdout = Unblock::new(std::io::stdout());
            let mut stderr = Unblock::new(std::io::stderr());

            futures::future::try_join(
                futures::io::copy(fidl::AsyncSocket::from_socket(stdout_sock)?, &mut stdout),
                futures::io::copy(fidl::AsyncSocket::from_socket(stderr_sock)?, &mut stderr),
            )
            .await
            .map_err(|e| anyhow::anyhow!("Error joining stdio futures: {}", e))
        },
        async move {
            let mut socket_writer = fidl::AsyncSocket::from_socket(play_local)?;
            let stdin_res =
                futures::io::copy(Unblock::new(std::io::stdin()), &mut socket_writer).await;

            // Close ffx end of socket so that daemon end reads EOF and stops waiting for data.
            drop(socket_writer);
            stdin_res.map_err(|e| anyhow::anyhow!("Error stdin: {}", e))
        },
    );

    futs.await?;

    Ok(())
}
