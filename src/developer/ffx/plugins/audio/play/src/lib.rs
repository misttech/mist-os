// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result,
    blocking::Unblock,
    errors::ffx_bail,
    ffx_audio_play_args::{PlayCommand, RenderCommand, SubCommand},
    ffx_core::ffx_plugin,
    fidl_fuchsia_audio_ffxdaemon::{AudioDaemonPlayRequest, AudioDaemonProxy},
};

#[ffx_plugin(
    "audio",
    AudioDaemonProxy = "core/audio_ffx_daemon:expose:fuchsia.audio.ffxdaemon.AudioDaemon"
)]
pub async fn play_cmd(audio_proxy: AudioDaemonProxy, cmd: PlayCommand) -> Result<()> {
    match cmd.subcommand {
        SubCommand::Render(renderer_command) => {
            renderer_play(audio_proxy, renderer_command).await?
        }
    }
    Ok(())
}

pub async fn renderer_play(audio_proxy: AudioDaemonProxy, cmd: RenderCommand) -> Result<()> {
    let (play_remote, play_local) = fidl::Socket::create(fidl::SocketOpts::DATAGRAM)?;

    let futs = futures::future::try_join(
        async {
            let request = AudioDaemonPlayRequest {
                socket: Some(play_remote),
                location: Some(fidl_fuchsia_audio_ffxdaemon::PlayLocation::Renderer(
                    fidl_fuchsia_audio_ffxdaemon::RendererInfo {
                        usage: Some(cmd.usage),
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
