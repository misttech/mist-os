// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_audio_record_args::AudioCaptureUsageExtended;

use {
    anyhow::Result,
    blocking::Unblock,
    errors::ffx_bail,
    ffx_audio_record_args::{RecordCommand, SubCommand},
    ffx_core::ffx_plugin,
    fidl_fuchsia_audio_ffxdaemon::{
        AudioDaemonProxy, AudioDaemonRecordRequest, CapturerInfo, RecordLocation,
    },
    fidl_fuchsia_media::AudioStreamType,
    futures,
};

#[ffx_plugin(
    "audio",
    AudioDaemonProxy = "core/audio_ffx_daemon:expose:fuchsia.audio.ffxdaemon.AudioDaemon"
)]
pub async fn record_cmd(audio_proxy: AudioDaemonProxy, cmd: RecordCommand) -> Result<()> {
    match cmd.subcommand {
        SubCommand::Capture(_) => record_capture(audio_proxy, cmd).await?,
    }
    Ok(())
}

pub async fn record_capture(
    audio_proxy: AudioDaemonProxy,
    record_command: RecordCommand,
) -> Result<()> {
    let (usage_extended, _buffer_size_arg) = match record_command.subcommand {
        SubCommand::Capture(capture_command) => {
            (capture_command.usage, capture_command.buffer_size)
        }
    };

    let loopback = usage_extended == AudioCaptureUsageExtended::Loopback;
    let capturer_usage = match usage_extended {
        AudioCaptureUsageExtended::Background(usage)
        | AudioCaptureUsageExtended::Foreground(usage)
        | AudioCaptureUsageExtended::Communication(usage)
        | AudioCaptureUsageExtended::SystemAgent(usage) => Some(usage),
        _ => None,
    };
    let stream_type = AudioStreamType {
        sample_format: record_command.format.sample_type,
        channels: record_command.format.channels as u32,
        frames_per_second: record_command.format.frames_per_second,
    };

    let fut = async {
        let request = AudioDaemonRecordRequest {
            location: if loopback {
                Some(RecordLocation::Loopback(fidl_fuchsia_audio_ffxdaemon::Loopback {}))
            } else {
                Some(RecordLocation::Capturer(CapturerInfo {
                    usage: capturer_usage,
                    ..CapturerInfo::EMPTY
                }))
            },
            stream_type: Some(stream_type),
            duration: Some(record_command.duration.as_nanos() as i64),
            ..AudioDaemonRecordRequest::EMPTY
        };

        let (stdout_sock, stderr_sock) = match audio_proxy.record(request).await? {
            Ok(value) => (
                value.stdout.ok_or(anyhow::anyhow!("No stdout socket"))?,
                value.stderr.ok_or(anyhow::anyhow!("No stderr socket."))?,
            ),
            Err(err) => ffx_bail!("Record failed with err: {}", err),
        };

        let mut stdout = Unblock::new(std::io::stdout());
        let mut stderr = Unblock::new(std::io::stderr());

        futures::future::try_join(
            futures::io::copy(fidl::AsyncSocket::from_socket(stdout_sock)?, &mut stdout),
            futures::io::copy(fidl::AsyncSocket::from_socket(stderr_sock)?, &mut stderr),
        )
        .await
        .map_err(|e| anyhow::anyhow!("Error joining stdio futures: {}", e))
    };

    fut.await?;

    Ok(())
}
