// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{clock::create_reference_clock, wav_socket::WavSocket};
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
use futures::future::BoxFuture;
use futures::{FutureExt, TryStreamExt};
use std::cmp;
use std::rc::Rc;

async fn create_renderer_from_location(
    location: fac::PlayDestination,
    format: &Format,
    gain_settings: Option<fac::GainSettings>,
) -> Result<fmedia::AudioRendererProxy, Error> {
    let (client_end, server_end) = create_endpoints::<fmedia::AudioRendererMarker>();

    let audio_renderer_proxy =
        client_end.into_proxy().map_err(|e| anyhow!("Error getting AudioRendererProxy: {e}"))?;

    if let fac::PlayDestination::Renderer(renderer_config) = location {
        match renderer_config {
            fac::RendererConfig::UltrasoundRenderer(_) => {
                let component = connect_to_protocol::<fultrasound::FactoryMarker>()
                    .context("Failed to connect to fuchsia.ultrasound.Factory")?;
                let (_reference_clock, stream_type) = component.create_renderer(server_end).await?;

                if format.channels != stream_type.channels
                    || format.sample_type != stream_type.sample_format
                    || format.frames_per_second != stream_type.frames_per_second
                {
                    return Err(anyhow!(
                        "Requested format for ultrasound renderer does not match available\
                            format. Expected {}hz, {:?}, {:?}ch\n",
                        stream_type.frames_per_second,
                        stream_type.sample_format,
                        stream_type.channels,
                    ));
                }
            }
            fac::RendererConfig::StandardRenderer(renderer_config) => {
                let audio_component = connect_to_protocol::<fmedia::AudioMarker>()
                    .context("Failed to connect to fuchsia.media.Audio")?;
                audio_component.create_audio_renderer(server_end)?;

                if let Some(clock_type) = renderer_config.clock {
                    let reference_clock = create_reference_clock(clock_type)?;
                    audio_renderer_proxy.set_reference_clock(reference_clock)?;
                }

                if let Some(usage) = renderer_config.usage {
                    audio_renderer_proxy.set_usage(usage)?;
                }

                audio_renderer_proxy.set_pcm_stream_type(&fmedia::AudioStreamType::from(format))?;

                if let Some(gain_settings) = gain_settings {
                    let (gain_control_client_end, gain_control_server_end) =
                        create_endpoints::<fmedia_audio::GainControlMarker>();

                    audio_renderer_proxy.bind_gain_control(gain_control_server_end)?;
                    let gain_control_proxy = gain_control_client_end.into_proxy()?;

                    gain_settings
                        .gain
                        .and_then(|gain_db| gain_control_proxy.set_gain(gain_db).ok());
                    gain_settings.mute.and_then(|mute| gain_control_proxy.set_mute(mute).ok());
                }
            }

            _ => return Err(anyhow!("Unexpected RendererType")),
        }
    } else {
        return Err(anyhow!("Unexpected PlayDestination"));
    };
    Ok(audio_renderer_proxy)
}

pub async fn play_renderer(
    request: fac::PlayerPlayRequest,
) -> Result<fac::PlayerPlayResponse, Error> {
    let data_socket = request.wav_source.ok_or(anyhow!("Socket argument missing."))?;

    let mut socket = WavSocket(fasync::Socket::from_socket(data_socket));
    let spec = socket.read_header().await?;
    let format = Format::from(&spec);

    let location = request.destination.ok_or(anyhow!("PlayDestination argument missing."))?;
    let default_packet_count = 4;

    let packet_count = match &location {
        fac::PlayDestination::Renderer(renderer_config) => match &renderer_config {
            fac::RendererConfig::StandardRenderer(config) => {
                config.packet_count.unwrap_or(default_packet_count)
            }
            fac::RendererConfig::UltrasoundRenderer(config) => {
                config.packet_count.unwrap_or(default_packet_count)
            }
            _ => default_packet_count,
        },
        _ => default_packet_count,
    } as usize;

    let audio_renderer_proxy =
        Rc::new(create_renderer_from_location(location, &format, request.gain_settings).await?);

    let vmo_size_bytes = format.frames_per_second as usize * format.bytes_per_frame() as usize;
    let vmo = zx::Vmo::create(vmo_size_bytes as u64)?;

    let bytes_per_packet = cmp::min(vmo_size_bytes / packet_count, 32000 as usize);

    audio_renderer_proxy.add_payload_buffer(0, vmo.duplicate_handle(zx::Rights::SAME_RIGHTS)?)?;

    audio_renderer_proxy.enable_min_lead_time_events(true)?;

    // Wait for AudioRenderer to initialize (lead_time > 0)
    let mut stream = audio_renderer_proxy.take_event_stream();
    while let Some(event) = stream.try_next().await? {
        match event {
            fmedia::AudioRendererEvent::OnMinLeadTimeChanged { min_lead_time_nsec } => {
                if min_lead_time_nsec > 0 {
                    break;
                }
            }
        }
    }

    let offsets: Vec<usize> = (0..packet_count).map(|x| x * bytes_per_packet).collect();

    let futs = offsets.iter().map(|offset| async {
        // TODO(b/300279107): Calculate total bytes sent to an AudioRenderer.
        send_next_packet(
            offset.to_owned() as u64,
            fasync::Socket::from_socket(
                socket.0.as_ref().duplicate_handle(zx::Rights::SAME_RIGHTS)?,
            ),
            vmo.duplicate_handle(zx::Rights::SAME_RIGHTS)?,
            &audio_renderer_proxy,
            bytes_per_packet,
            1,
        )
        .await
    });

    futures::future::try_join_all(futs).await?;
    Ok(fac::PlayerPlayResponse { bytes_processed: None, ..Default::default() })
}

fn send_next_packet<'b>(
    payload_offset: u64,
    socket: fidl::AsyncSocket,
    vmo: zx::Vmo,
    audio_renderer_proxy: &'b fmedia::AudioRendererProxy,
    bytes_per_packet: usize,
    iteration: u32,
) -> BoxFuture<'b, Result<(), Error>> {
    async move {
        let mut socket_wrapper = WavSocket(socket);
        let mut buf = vec![0u8; bytes_per_packet];
        let total_bytes_read = socket_wrapper.read_until_full(&mut buf).await? as usize;

        if total_bytes_read == 0 {
            return Ok(());
        }
        vmo.write(&buf[..total_bytes_read], payload_offset)?;

        let packet_fut = audio_renderer_proxy.send_packet(&fmedia::StreamPacket {
            pts: fmedia::NO_TIMESTAMP,
            payload_buffer_id: 0,
            payload_offset,
            payload_size: total_bytes_read as u64,
            flags: 0,
            buffer_config: 0,
            stream_segment_id: 0,
        });

        if payload_offset == 0 && iteration == 1 {
            audio_renderer_proxy.play(fmedia::NO_TIMESTAMP, fmedia::NO_TIMESTAMP).await?;
        }

        packet_fut.await?;

        if total_bytes_read == bytes_per_packet {
            send_next_packet(
                payload_offset,
                socket_wrapper.0,
                vmo.duplicate_handle(zx::Rights::SAME_RIGHTS)?,
                &audio_renderer_proxy,
                bytes_per_packet,
                iteration + 1,
            )
            .await
        } else {
            Ok(())
        }
    }
    .boxed()
}
