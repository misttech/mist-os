// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::HandleBased;

use {
    anyhow::Result,
    async_trait::async_trait,
    ffx_audio_play_args::{
        AudioRenderUsageExtended::{
            Background, Communication, Interruption, Media, SystemAgent, Ultrasound,
        },
        PlayCommand,
    },
    fho::{moniker, FfxMain, FfxTool, SimpleWriter},
    fidl_fuchsia_audio_ffxdaemon::{AudioDaemonPlayRequest, AudioDaemonProxy},
};

#[derive(FfxTool)]
pub struct PlayTool {
    #[command]
    cmd: PlayCommand,
    #[with(moniker("/core/audio_ffx_daemon"))]
    audio_proxy: AudioDaemonProxy,
}

fho::embedded_plugin!(PlayTool);
#[async_trait(?Send)]
impl FfxMain for PlayTool {
    type Writer = SimpleWriter;
    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        let (play_remote, play_local) = fidl::Socket::create_datagram();
        play_impl(
            self.audio_proxy,
            play_local,
            play_remote,
            self.cmd,
            std::io::stdin(),
            &ffx_audio_common::STDOUT,
            &ffx_audio_common::STDERR,
        )
        .await
        .map_err(Into::into)
    }
}

async fn play_impl<R, W, E>(
    audio_proxy: AudioDaemonProxy,
    play_local: fidl::Socket,
    play_remote: fidl::Socket,
    command: PlayCommand,
    input_reader: R, // Input generalized to stdin or test buffer. Forward to socket.
    output_writer: &'static W, // Output generalized to stdout or a test buffer. Forward data
    // from daemon to this writer.
    output_error_writer: &'static E, // Likewise, forward error data to a separate writer
                                     // generalized to stderr or a test buffer.
) -> Result<(), anyhow::Error>
where
    R: std::io::Read + std::marker::Send + 'static,
    W: std::marker::Send + 'static + std::marker::Sync,
    E: std::marker::Send + 'static + std::marker::Sync,
    &'static W: std::io::Write,
    &'static E: std::io::Write,
{
    let renderer = match command.usage {
        Ultrasound => fidl_fuchsia_audio_ffxdaemon::RendererType::UltrasoundRenderer(
            fidl_fuchsia_audio_ffxdaemon::UltrasoundRenderer {
                packet_count: command.packet_count,
                ..Default::default()
            },
        ),

        Background(usage) | Media(usage) | SystemAgent(usage) | Communication(usage)
        | Interruption(usage) => fidl_fuchsia_audio_ffxdaemon::RendererType::StandardRenderer(
            fidl_fuchsia_audio_ffxdaemon::RendererConfig {
                usage: Some(usage),
                clock: Some(command.clock),
                ..Default::default()
            },
        ),
    };

    // Duplicate socket handle so that connection stays alive in real + testing scenarios.
    let daemon_request_socket = play_remote
        .duplicate_handle(fidl::Rights::SAME_RIGHTS)
        .map_err(|e| anyhow::anyhow!("Error duplicating socket: {e}"))?;

    let request = AudioDaemonPlayRequest {
        socket: Some(daemon_request_socket),
        location: Some(fidl_fuchsia_audio_ffxdaemon::PlayLocation::Renderer(renderer)),
        gain_settings: Some(fidl_fuchsia_audio_ffxdaemon::GainSettings {
            mute: Some(command.mute),
            gain: Some(command.gain),
            ..Default::default()
        }),
        ..Default::default()
    };

    ffx_audio_common::play(
        request,
        audio_proxy,
        play_local,
        input_reader,
        output_writer,
        output_error_writer,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use ffx_audio_play_args::AudioRenderUsageExtended;
    use ffx_core::macro_deps::futures::AsyncWriteExt;
    use ffx_writer as _;
    use fidl::encoding::zerocopy::AsBytes;
    use fidl_fuchsia_media::AudioRenderUsage;

    #[fuchsia_async::run_singlethreaded(test)]
    pub async fn test_play() -> Result<(), fho::Error> {
        let audio_daemon = ffx_audio_common::tests::fake_audio_daemon();

        let command = PlayCommand {
            usage: AudioRenderUsageExtended::Media(AudioRenderUsage::Media),
            buffer_size: Some(48000),
            packet_count: None,
            gain: 0.0,
            mute: false,
            clock: fidl_fuchsia_audio_ffxdaemon::ClockType::Flexible(
                fidl_fuchsia_audio_ffxdaemon::Flexible,
            ),
        };

        let (play_remote, play_local) = fidl::Socket::create_datagram();
        let mut async_play_local = fidl::AsyncSocket::from_socket(
            play_local.duplicate_handle(fidl::Rights::SAME_RIGHTS).unwrap(),
        )
        .unwrap();

        async_play_local.write_all(ffx_audio_common::tests::WAV_HEADER_EXT).await.unwrap();
        let result = play_impl(
            audio_daemon,
            play_local,
            play_remote,
            command,
            &ffx_audio_common::tests::WAV_HEADER_EXT[..],
            &ffx_audio_common::tests::MOCK_STDOUT,
            &ffx_audio_common::tests::MOCK_STDERR,
        )
        .await;

        result.unwrap();
        let expected_output = "Successfully processed all audio data.".as_bytes();
        let lock = ffx_audio_common::tests::MOCK_STDOUT.lock().unwrap();
        let output: &[u8] = lock.as_bytes();

        assert_eq!(output, expected_output);
        Ok(())
    }
}
