// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result,
    //  errors::ffx_bail,
    ffx_audio_record_args::{RecordCommand, SubCommand},
    ffx_core::ffx_plugin,
    fidl_fuchsia_audio_ffxdaemon::AudioDaemonProxy,
    futures as _,
    //fidl_fuchsia_media::{AudioCapturerEvent, AudioSampleFormat, AudioStreamType},
    // fidl_fuchsia_media_audio,
    //  std::{cmp::min, io, io::Seek, io::SeekFrom, io::Write},
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
    _audio_proxy: AudioDaemonProxy,
    record_command: RecordCommand,
) -> Result<()> {
    let (_usage_extended, _buffer_size_arg, _gain, _mute) = match record_command.subcommand {
        SubCommand::Capture(capture_command) => (
            capture_command.usage,
            capture_command.buffer_size,
            capture_command.gain,
            capture_command.mute,
        ),
    };

    // let frames_to_capture = (record_command.format.sample_rate as f64
    //     * record_command.duration.as_secs_f64())
    // .ceil() as u64;

    // let buffer_size_bytes = match buffer_size_arg {
    //     Some(size) => size,
    //     None => {
    //         record_command.format.sample_rate as u64
    //             * record_command.format.bytes_per_frame() as u64
    //     }
    // };

    // // Use 10 packets with 100ms of data each to allow room for latency spikes.
    // let number_of_packets = 10;
    // let bytes_per_packet =
    //     min(buffer_size_bytes / number_of_packets, MAX_AUDIO_BUFFER_BYTES as u64);
    // let frames_per_packet = bytes_per_packet / record_command.format.bytes_per_frame() as u64;

    // let total_packets_to_capture =
    //     (frames_to_capture as f64 / frames_per_packet as f64).ceil() as u64;

    // let stream_type = AudioStreamType {
    //     sample_format: AudioSampleFormat::from(&record_command.format),
    //     channels: record_command.format.channels as u32,
    //     frames_per_second: record_command.format.sample_rate,
    // };
    // // CreateAudioDaemon returns a client end to an AudioCapturer channel and a VMOProxy channel.
    // // Daemon handles setting pcm_stream_type and adding payload buffer.
    // let request = AudioDaemonCreateAudioCapturerRequest {
    //     buffer_size: Some(buffer_size_bytes),
    //     loopback: Some(matches!(usage_extended, AudioCaptureUsageExtended::Loopback)),
    //     stream_type: Some(stream_type),
    //     ..AudioDaemonCreateAudioCapturerRequest::EMPTY
    // };

    // let (capturer_client_end, vmo_client_end, gain_control_client_end) = match audio_proxy
    //     .create_audio_capturer(request)
    //     .await
    //     .context("Error sending request to create audio capturer")?
    // {
    //     Ok(value) => (
    //         value
    //             .capturer
    //             .context("Failed to retrieve client end of AudioCapturer from response")?,
    //         value
    //             .vmo_channel
    //             .context("Failed to retrieve client end of VmoWrapper from response")?,
    //         value
    //             .gain_control
    //             .context("Failed to retrieve gain control client end from response")?,
    //     ),
    //     Err(err) => ffx_bail!("Create audio capturer returned error {}", err),
    // };

    // let capturer_proxy = capturer_client_end.into_proxy()?;
    // match usage_extended {
    //     AudioCaptureUsageExtended::Background(usage)
    //     | AudioCaptureUsageExtended::Foreground(usage)
    //     | AudioCaptureUsageExtended::Communication(usage)
    //     | AudioCaptureUsageExtended::SystemAgent(usage) => {
    //         capturer_proxy.set_usage(usage)?;
    //     }
    //     _ => {}
    // }

    // let gain_control_proxy = gain_control_client_end.into_proxy()?;
    // gain_control_proxy.set_gain(
    //     gain.clamp(fidl_fuchsia_media_audio::MUTED_GAIN_DB, fidl_fuchsia_media_audio::MAX_GAIN_DB),
    // )?;

    // gain_control_proxy.set_mute(mute)?;
    // capturer_proxy.start_async_capture(frames_per_packet.try_into().unwrap())?;

    // let mut stream = capturer_proxy.take_event_stream();
    // let mut packets_so_far = 0;

    // // A valid Wav File Header must have the data format and data length fields.
    // // We need all values corresponding to wav header fields set on the cursor_writer before
    // // writing to stdout.
    // let mut cursor_writer = io::Cursor::new(Vec::<u8>::new());
    // {
    //     // Creation of WavWriter writes the Wav File Header to cursor_writer.
    //     // This written header has the file size field and data chunk size field both set to 0,
    //     // since the number of samples (and resulting file and chunk sizes) are unknown to
    //     // the WavWriter at this point.
    //     let _writer =
    //         hound::WavWriter::new(&mut cursor_writer, hound::WavSpec::from(&record_command.format))
    //             .unwrap();
    // }

    // // The file and chunk size fields are set to 0 as placeholder values by the construction of
    // // the WavWriter above. We can compute the actual values based on the command arguments
    // // for format and duration, and set the file size and chunk size fields to the computed
    // // values in the cursor_writer before writing to stdout.

    // let bytes_to_capture: u32 =
    //     frames_to_capture as u32 * record_command.format.bytes_per_frame() as u32;

    // // The File Size field of a WAV header. 32-bit int starting at position 4, represents
    // // the size of the overall file minus 8 bytes (exclude RIFF description and file size description)
    // let file_size_bytes: u32 = bytes_to_capture + audio_utils::WAV_HEADER_BYTE_COUNT - 8;
    // cursor_writer.seek(SeekFrom::Start(4))?;
    // cursor_writer.write_all(&file_size_bytes.to_le_bytes()[..])?;

    // // Data size field of a WAV header. For PCM, this is a 32-bit int starting at position 40,
    // // and represents the size of the data section.
    // cursor_writer.seek(SeekFrom::Start(40))?;
    // cursor_writer.write_all(&bytes_to_capture.to_le_bytes()[..])?;

    // // Write the completed WAV header to stdout. We then write the raw sample values from the
    // // packets received directly to stdout.
    // let mut stdout = io::stdout().lock();
    // stdout.write_all(&cursor_writer.into_inner())?;
    // stdout.flush()?;

    // let vmo_proxy = vmo_client_end.into_proxy()?;
    // while let Some(event) = stream.try_next().await? {
    //     match event {
    //         AudioCapturerEvent::OnPacketProduced { mut packet } => {
    //             packets_so_far += 1;

    //             let request = VmoWrapperReadRequest {
    //                 offset: Some(packet.payload_offset),
    //                 buffer_size_bytes: Some(packet.payload_size),
    //                 ..VmoWrapperReadRequest::EMPTY
    //             };
    //             let mut packet_data = match vmo_proxy
    //                 .read(request)
    //                 .await
    //                 .context("Error sending request to read vmo wrapper.")?
    //             {
    //                 Ok(value) => value.data.unwrap(),
    //                 Err(err) => ffx_bail!("VmoWrapper read error {}", err),
    //             };

    //             stdout.write_all(&mut packet_data)?;

    //             capturer_proxy.release_packet(&mut packet)?;
    //             if packets_so_far == total_packets_to_capture {
    //                 break;
    //             }
    //         }
    //         AudioCapturerEvent::OnEndOfStream {} => {
    //             break;
    //         }
    //     }
    // }

    // stdout.flush()?;

    Ok(())
}
