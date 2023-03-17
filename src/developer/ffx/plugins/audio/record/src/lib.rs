// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_audio_record_args::AudioCaptureUsageExtended;

use {
    anyhow::Result,
    blocking::Unblock,
    errors::ffx_bail,
    ffx_audio_record_args::RecordCommand,
    ffx_core::ffx_plugin,
    fidl_fuchsia_audio_ffxdaemon::{
        AudioDaemonProxy, AudioDaemonRecordRequest, CapturerInfo, CapturerType, RecordLocation,
    },
    fidl_fuchsia_media::AudioStreamType,
};

#[ffx_plugin(
    "audio",
    AudioDaemonProxy = "core/audio_ffx_daemon:expose:fuchsia.audio.ffxdaemon.AudioDaemon"
)]
pub async fn record_cmd(audio_proxy: AudioDaemonProxy, cmd: RecordCommand) -> Result<()> {
    record_capture(audio_proxy, cmd).await?;
    Ok(())
}

pub async fn record_capture(
    audio_proxy: AudioDaemonProxy,
    record_command: RecordCommand,
) -> Result<()> {
    let capturer_usage = match record_command.usage {
        AudioCaptureUsageExtended::Background(usage)
        | AudioCaptureUsageExtended::Foreground(usage)
        | AudioCaptureUsageExtended::Communication(usage)
        | AudioCaptureUsageExtended::SystemAgent(usage) => Some(usage),
        _ => None,
    };

    let (location, gain_settings) = match record_command.usage {
        AudioCaptureUsageExtended::Loopback => {
            (RecordLocation::Loopback(fidl_fuchsia_audio_ffxdaemon::Loopback {}), None)
        }
        AudioCaptureUsageExtended::Ultrasound => (
            RecordLocation::Capturer(CapturerType::UltrasoundCapturer(
                fidl_fuchsia_audio_ffxdaemon::UltrasoundCapturer {},
            )),
            None,
        ),
        _ => (
            RecordLocation::Capturer(CapturerType::StandardCapturer(CapturerInfo {
                usage: capturer_usage,
                ..CapturerInfo::EMPTY
            })),
            Some(fidl_fuchsia_audio_ffxdaemon::GainSettings {
                mute: Some(record_command.mute),
                gain: Some(record_command.gain),
                ..fidl_fuchsia_audio_ffxdaemon::GainSettings::EMPTY
            }),
        ),
    };

    let (cancel_client, cancel_server) = fidl::endpoints::create_endpoints::<
        fidl_fuchsia_audio_ffxdaemon::AudioDaemonCancelerMarker,
    >();

    let request = AudioDaemonRecordRequest {
        location: Some(location),
        stream_type: Some(AudioStreamType::from(&record_command.format)),
        duration: record_command.duration.map(|duration| duration.as_nanos() as i64),
        canceler: Some(cancel_server),
        gain_settings,
        buffer_size: record_command.buffer_size,
        ..AudioDaemonRecordRequest::EMPTY
    };

    let (stdout_sock, stderr_sock) = match audio_proxy.record(request).await? {
        Ok(value) => (
            value.stdout.ok_or(anyhow::anyhow!("No stdout socket."))?,
            value.stderr.ok_or(anyhow::anyhow!("No stderr socket."))?,
        ),
        Err(err) => ffx_bail!("Record failed with err: {}", err),
    };

    let mut stdout = Unblock::new(std::io::stdout());
    let mut stderr = Unblock::new(std::io::stderr());

    futures::future::try_join3(
        futures::io::copy(fidl::AsyncSocket::from_socket(stdout_sock)?, &mut stdout),
        futures::io::copy(fidl::AsyncSocket::from_socket(stderr_sock)?, &mut stderr),
        ffx_audio_common::wait_for_keypress(cancel_client),
    )
    .await
    .map(|_| ())
    .map_err(|e| anyhow::anyhow!("Error copying data from socket. {}", e))
}
