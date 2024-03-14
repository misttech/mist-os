// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{clock::create_reference_clock, error::ControllerError, wav_socket::WavSocket};
use anyhow::{anyhow, Context, Error};
use fidl::endpoints::create_endpoints;
use fidl_fuchsia_audio_controller as fac;
use fidl_fuchsia_media as fmedia;
use fidl_fuchsia_media_audio as fmedia_audio;
use fidl_fuchsia_ultrasound as fultrasound;
use format_utils::Format;
use fuchsia_async as fasync;
use fuchsia_component::client::connect_to_protocol;
use fuchsia_zircon::{self as zx, HandleBased};
use futures::{AsyncWriteExt, TryStreamExt};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

async fn create_capturer_from_location(
    location: fac::RecordSource,
    format: &Format,
    stream_type: fmedia::AudioStreamType,
    gain_settings: Option<fac::GainSettings>,
) -> Result<fmedia::AudioCapturerProxy, Error> {
    let (client_end, server_end) = create_endpoints::<fmedia::AudioCapturerMarker>();

    match location {
        fac::RecordSource::Capturer(capturer_type) => match capturer_type {
            fac::CapturerConfig::StandardCapturer(config) => {
                let audio_component = connect_to_protocol::<fmedia::AudioMarker>()
                    .context("Failed to connect to fuchsia.media.Audio")?;

                audio_component.create_audio_capturer(server_end, false)?;
                let capturer_proxy = client_end.into_proxy()?;

                // Check that connection to AudioCore is valid.
                let _ = match capturer_proxy.get_reference_clock().await {
                    Ok(_) => Ok(()),
                    Err(e) => {
                        println!("{e}");
                        Err(anyhow!("Failed to get reference clock {e}"))
                    }
                }?;

                capturer_proxy.set_pcm_stream_type(&stream_type)?;

                if let Some(gain_settings) = gain_settings {
                    let (gain_control_client_end, gain_control_server_end) =
                        create_endpoints::<fmedia_audio::GainControlMarker>();

                    capturer_proxy.bind_gain_control(gain_control_server_end)?;
                    let gain_control_proxy = gain_control_client_end.into_proxy()?;

                    gain_settings
                        .gain
                        .and_then(|gain_db| gain_control_proxy.set_gain(gain_db).ok());
                    gain_settings.mute.and_then(|mute| gain_control_proxy.set_mute(mute).ok());
                }

                config.usage.and_then(|usage| capturer_proxy.set_usage(usage).ok());

                if let Some(clock_type) = config.clock {
                    let reference_clock = create_reference_clock(clock_type)?;
                    capturer_proxy.set_reference_clock(reference_clock)?;
                }
                Ok(capturer_proxy)
            }
            fac::CapturerConfig::UltrasoundCapturer(_) => {
                let component = connect_to_protocol::<fultrasound::FactoryMarker>()
                    .context("Failed to connect to fuchsia.ultrasound.Factory")?;
                let (_reference_clock, stream_type) = component.create_capturer(server_end).await?;
                if format.channels != stream_type.channels
                    || format.sample_type != stream_type.sample_format
                    || format.frames_per_second != stream_type.frames_per_second
                {
                    return Err(anyhow!(
                        "Requested format for ultrasound capturer\
                            does not match available format.
                            Expected {}hz, {:?}, {:?}ch\n",
                        stream_type.frames_per_second,
                        stream_type.sample_format,
                        stream_type.channels,
                    ));
                }
                client_end
                    .into_proxy()
                    .map_err(|e| anyhow!("Error getting AudioCapturerProxy: {e}"))
            }
            _ => Err(anyhow!("Unsupported capturer type.")),
        },
        fac::RecordSource::Loopback(..) => {
            let audio_component = connect_to_protocol::<fmedia::AudioMarker>()
                .context("Failed to connect to fuchsia.media.Audio")?;
            audio_component.create_audio_capturer(server_end, true)?;

            let capturer_proxy = client_end.into_proxy()?;
            capturer_proxy.set_pcm_stream_type(&stream_type)?;
            Ok(capturer_proxy)
        }
        _ => Err(anyhow!("Unsupported RecordSource")),
    }
}

pub async fn record_capturer(
    request: fac::RecorderRecordRequest,
) -> Result<fac::RecorderRecordResponse, ControllerError> {
    let location = request
        .source
        .ok_or(ControllerError::new(fac::Error::ArgumentsMissing, format!("Input missing.")))?;
    let wav_data = request.wav_data.ok_or(ControllerError::new(
        fac::Error::ArgumentsMissing,
        format!("Socket for wav data missing"),
    ))?;

    let stop_signal = AtomicBool::new(false);
    let cancel_server = request.canceler;

    let stream_type = request.stream_type.ok_or(ControllerError::new(
        fac::Error::ArgumentsMissing,
        format!("Stream type missing"),
    ))?;
    let format = Format::from(&stream_type);

    let duration =
        request.duration.map(|duration_nanos| Duration::from_nanos(duration_nanos as u64));

    let mut socket =
        WavSocket(fasync::Socket::from_socket(wav_data.duplicate_handle(zx::Rights::SAME_RIGHTS)?));

    let capturer_proxy =
        create_capturer_from_location(location, &format, stream_type, request.gain_settings)
            .await?;

    let packet_count = 4;
    let bytes_per_frame = format.bytes_per_frame() as u64;
    let buffer_size_bytes =
        request.buffer_size.unwrap_or(format.frames_per_second as u64 * bytes_per_frame);

    let bytes_per_packet = buffer_size_bytes / packet_count;

    let frames_per_packet = bytes_per_packet / bytes_per_frame;

    let packets_to_capture = duration.map(|duration| {
        (format.frames_in_duration(duration) as f64 * bytes_per_frame as f64
            / bytes_per_packet as f64)
            .ceil() as u64
    });
    let vmo = zx::Vmo::create(buffer_size_bytes)?;

    capturer_proxy.add_payload_buffer(0, vmo.duplicate_handle(zx::Rights::SAME_RIGHTS)?)?;
    capturer_proxy.start_async_capture(
        frames_per_packet
            .try_into()
            .map_err(|e| anyhow!("Frames per packet argument is too large: {}", e))?,
    )?;

    let mut stream = capturer_proxy.take_event_stream();
    let mut packets_so_far = 0;

    let mut async_wav_writer = fidl::AsyncSocket::from_socket(wav_data);

    socket.write_header(duration, &format).await?;
    let packet_fut = async {
        while let Some(event) = stream.try_next().await? {
            if stop_signal.load(Ordering::SeqCst) {
                break;
            }
            match event {
                fmedia::AudioCapturerEvent::OnPacketProduced { packet } => {
                    packets_so_far += 1;

                    let mut data = vec![0u8; packet.payload_size as usize];
                    let _audio_data = vmo
                        .read(&mut data[..], packet.payload_offset)
                        .map_err(|e| anyhow!("Failed to read vmo {e}"))?;

                    async_wav_writer
                        .write_all(&data)
                        .await
                        .map_err(|e| anyhow!("Error writing to stdout socket: {e}"))?;

                    capturer_proxy
                        .release_packet(&packet)
                        .map_err(|e| anyhow!("Release packet error: {}", e))?;

                    if let Some(packets_to_capture) = packets_to_capture {
                        if packets_so_far == packets_to_capture {
                            break;
                        }
                    }
                }
                fmedia::AudioCapturerEvent::OnEndOfStream {} => break,
            }
        }

        Ok(fac::RecorderRecordResponse {
            bytes_processed: Some(packets_so_far * bytes_per_packet),
            packets_processed: Some(packets_so_far),
            late_wakeups: None,
            ..Default::default()
        })
    };

    if let Some(cancel_server) = cancel_server {
        let (_cancel_res, packet_res) = futures::future::try_join(
            listener_utils::stop_listener(cancel_server, &stop_signal),
            packet_fut,
        )
        .await?;
        Ok(packet_res)
    } else {
        Ok(packet_fut.await?)
    }
}
