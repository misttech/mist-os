// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod device;
mod ring_buffer;
mod socket;

use fidl_fuchsia_media::AudioStreamType;
pub use ring_buffer::RingBuffer;

use {
    anyhow::{self, Context, Error},
    async_lock as _,
    fidl::HandleBased,
    fidl_fuchsia_audio_ffxdaemon::{
        AudioDaemonDeviceInfoResponse, AudioDaemonListDevicesResponse, AudioDaemonPlayRequest,
        AudioDaemonPlayResponder, AudioDaemonPlayResponse, AudioDaemonRecordRequest,
        AudioDaemonRecordResponder, AudioDaemonRecordResponse, AudioDaemonRequest,
        AudioDaemonRequestStream, PlayLocation, RecordLocation,
    },
    fidl_fuchsia_media::AudioRendererProxy,
    fidl_fuchsia_media_audio, fuchsia as _, fuchsia_async as fasync,
    fuchsia_component::server::ServiceFs,
    fuchsia_inspect::{component, health::Reporter},
    fuchsia_zircon::{self as zx},
    futures::future::{BoxFuture, FutureExt},
    futures::prelude::*,
    futures::StreamExt,
    std::cmp,
    std::rc::Rc,
};

const SECONDS_PER_NANOSECOND: f64 = 1.0 / 10_u64.pow(9) as f64;

/// Wraps all hosted protocols into a single type that can be matched against
/// and dispatched.
enum IncomingRequest {
    AudioDaemon(AudioDaemonRequestStream),
}
struct AudioDaemon {}
impl AudioDaemon {
    async fn record_capturer(
        &self,
        request: AudioDaemonRecordRequest,
        responder: AudioDaemonRecordResponder,
    ) -> Result<(), anyhow::Error> {
        let (stdout_remote, stdout_local) = zx::Socket::create_stream();
        let (stderr_remote, _stderr_local) = zx::Socket::create_stream();

        let response = AudioDaemonRecordResponse {
            stdout: Some(stdout_remote),
            stderr: Some(stderr_remote),
            ..AudioDaemonRecordResponse::EMPTY
        };
        responder.send(&mut Ok(response)).expect("Failed to send play response.");

        let audio_component =
            fuchsia_component::client::connect_to_protocol::<fidl_fuchsia_media::AudioMarker>()
                .context("Failed to connect to fuchsia.media.Audio")?;

        let location = request.location.ok_or(anyhow::anyhow!("Input missing."))?;

        let (capturer_usage, loopback, clock) = match location {
            RecordLocation::Capturer(capturer_info) => {
                (capturer_info.usage, false, capturer_info.clock)
            }
            RecordLocation::Loopback(..) => (None, true, None),
            _ => panic!("Expected Capturer RecordLocation"),
        };

        let mut stream_type = request.stream_type.ok_or(anyhow::anyhow!("Stream type missing"))?;
        let format = format_utils::Format::from(&stream_type);
        let duration_nanos =
            request.duration.ok_or(anyhow::anyhow!("Duration argument missing."))? as u64;
        let duration = std::time::Duration::from_nanos(duration_nanos);

        let mut socket = socket::Socket {
            socket: &mut fasync::Socket::from_socket(
                stdout_local.duplicate_handle(zx::Rights::SAME_RIGHTS)?,
            )?,
        };
        socket.write_wav_header(duration, &format).await?;

        let (client_end, server_end) =
            fidl::endpoints::create_endpoints::<fidl_fuchsia_media::AudioCapturerMarker>();

        audio_component.create_audio_capturer(server_end, loopback)?;

        let capturer_proxy = client_end.into_proxy()?;
        capturer_proxy.set_pcm_stream_type(&mut stream_type)?;

        if !(loopback) {
            match capturer_usage {
                Some(capturer_usage) => capturer_proxy.set_usage(capturer_usage)?,
                None => panic!("No usage specified how to capture audio."),
            }
        }

        if let Some(clock_type) = clock {
            let reference_clock = Self::setup_reference_clock(clock_type)?;
            capturer_proxy.set_reference_clock(reference_clock)?;
        }

        let num_packets = 4;
        let bytes_per_frame = format.bytes_per_frame() as u64;
        let buffer_size_bytes = format.frames_per_second as u64 * bytes_per_frame as u64;
        let bytes_per_packet = buffer_size_bytes / num_packets;

        let frames_per_packet = bytes_per_packet / bytes_per_frame;
        let bytes_to_capture = format.frames_in_duration(duration) * bytes_per_frame;
        let packets_to_capture = (bytes_to_capture as f64 / bytes_per_packet as f64).ceil() as u64;
        let vmo = zx::Vmo::create(buffer_size_bytes)?;

        capturer_proxy.add_payload_buffer(0, vmo.duplicate_handle(zx::Rights::SAME_RIGHTS)?)?;
        capturer_proxy.start_async_capture(frames_per_packet.try_into().unwrap())?;

        let mut stream = capturer_proxy.take_event_stream();
        let mut packets_so_far = 0;

        while let Some(event) = stream.try_next().await? {
            match event {
                fidl_fuchsia_media::AudioCapturerEvent::OnPacketProduced { mut packet } => {
                    packets_so_far += 1;

                    let mut data = vec![0u8; packet.payload_size as usize];
                    let _audio_data = vmo.read(&mut data[..], packet.payload_offset)?;

                    let mut bytes_written_from_packet = 0usize;
                    while bytes_written_from_packet < packet.payload_size as usize {
                        bytes_written_from_packet +=
                            stdout_local.write(&data[bytes_written_from_packet..])?;
                    }
                    capturer_proxy.release_packet(&mut packet)?;
                    if packets_so_far == packets_to_capture {
                        break;
                    }
                }
                fidl_fuchsia_media::AudioCapturerEvent::OnEndOfStream {} => break,
            }
        }

        Ok(())
    }

    fn setup_reference_clock(
        clock_type: fidl_fuchsia_audio_ffxdaemon::ClockType,
    ) -> Result<Option<zx::Clock>, Error> {
        match clock_type {
            fidl_fuchsia_audio_ffxdaemon::ClockType::Flexible(_) => Ok(None),
            fidl_fuchsia_audio_ffxdaemon::ClockType::Monotonic(_) => {
                let clock =
                    zx::Clock::create(zx::ClockOpts::CONTINUOUS | zx::ClockOpts::AUTO_START, None)
                        .expect("Creating reference clock failed");
                let rights_clock = clock
                    .replace_handle(zx::Rights::READ | zx::Rights::DUPLICATE | zx::Rights::TRANSFER)
                    .expect("Replace handle for reference clock failed");
                Ok(Some(rights_clock))
            }
            fidl_fuchsia_audio_ffxdaemon::ClockType::Custom(info) => {
                let rate = info.rate_adjust;
                let offset = info.offset;
                let now = zx::Time::get_monotonic();
                let delta_time = now + zx::Duration::from_nanos(offset.unwrap_or(0).into());

                let update_builder = zx::ClockUpdate::builder()
                    .rate_adjust(rate.unwrap_or(0))
                    .absolute_value(now, delta_time);

                let auto_start = if offset.is_some() {
                    zx::ClockOpts::empty()
                } else {
                    zx::ClockOpts::AUTO_START
                };

                let clock = zx::Clock::create(zx::ClockOpts::CONTINUOUS | auto_start, None)
                    .expect("Creating reference clock failed");

                clock.update(update_builder.build()).expect("Updating reference clock failed");

                Ok(Some(
                    clock
                        .replace_handle(
                            zx::Rights::READ | zx::Rights::DUPLICATE | zx::Rights::TRANSFER,
                        )
                        .expect("Replace handle for reference clock failed"),
                ))
            }
            fidl_fuchsia_audio_ffxdaemon::ClockTypeUnknown!() => Ok(None),
        }
    }

    fn send_next_packet<'b>(
        payload_offset: u64,
        mut socket: fidl::AsyncSocket,
        vmo: zx::Vmo,
        audio_renderer_proxy: &'b AudioRendererProxy,
        bytes_per_packet: usize,
        iteration: u32,
    ) -> BoxFuture<'b, Result<(), Error>> {
        async move {
            let mut socket_wrapper = socket::Socket { socket: &mut socket };
            let mut buf = vec![0u8; bytes_per_packet];
            let total_bytes_read = socket_wrapper.read_until_full(&mut buf).await? as usize;

            if total_bytes_read == 0 {
                return Ok(());
            }
            vmo.write(&buf[..total_bytes_read], payload_offset)?;

            let packet_fut =
                audio_renderer_proxy.send_packet(&mut fidl_fuchsia_media::StreamPacket {
                    pts: fidl_fuchsia_media::NO_TIMESTAMP,
                    payload_buffer_id: 0,
                    payload_offset: payload_offset,
                    payload_size: total_bytes_read as u64,
                    flags: 0,
                    buffer_config: 0,
                    stream_segment_id: 0,
                });

            if payload_offset == 0 && iteration == 1 {
                audio_renderer_proxy
                    .play(fidl_fuchsia_media::NO_TIMESTAMP, fidl_fuchsia_media::NO_TIMESTAMP)
                    .await?;
            }

            packet_fut.await?;

            if total_bytes_read == bytes_per_packet {
                Self::send_next_packet(
                    payload_offset,
                    socket,
                    vmo.duplicate_handle(zx::Rights::SAME_RIGHTS)?,
                    &audio_renderer_proxy,
                    bytes_per_packet,
                    iteration + 1,
                )
                .await?;
            } else {
                return Ok(());
            }
            Ok(())
        }
        .boxed()
    }

    async fn play_renderer(
        &self,
        request: AudioDaemonPlayRequest,
        responder: AudioDaemonPlayResponder,
    ) -> Result<(), anyhow::Error> {
        let audio_component =
            fuchsia_component::client::connect_to_protocol::<fidl_fuchsia_media::AudioMarker>()
                .context("Failed to connect to fuchsia.media.Audio")?;
        let num_packets = 4;
        let (stdout_remote, stdout_local) = zx::Socket::create_stream();
        let (stderr_remote, _stderr_local) = zx::Socket::create_stream();

        let (client_end, server_end) =
            fidl::endpoints::create_endpoints::<fidl_fuchsia_media::AudioRendererMarker>();

        let (gain_control_client_end, gain_control_server_end) =
            fidl::endpoints::create_endpoints::<fidl_fuchsia_media_audio::GainControlMarker>();

        let data_socket = request.socket.ok_or(anyhow::anyhow!("Socket argument missing."))?;
        audio_component.create_audio_renderer(server_end)?;
        let audio_renderer_proxy = Rc::new(client_end.into_proxy()?);

        let response = AudioDaemonPlayResponse {
            stdout: Some(stdout_remote),
            stderr: Some(stderr_remote),
            ..AudioDaemonPlayResponse::EMPTY
        };

        let mut socket = socket::Socket {
            socket: &mut fasync::Socket::from_socket(
                data_socket.duplicate_handle(zx::Rights::SAME_RIGHTS)?,
            )?,
        };
        let spec = socket.read_wav_header().await?;
        let format = format_utils::Format::from(&spec);

        let vmo_size_bytes = format.frames_per_second as usize * format.bytes_per_frame() as usize;
        let vmo = zx::Vmo::create(vmo_size_bytes as u64)?;

        let bytes_per_packet = cmp::min(vmo_size_bytes / num_packets as usize, 32000 as usize);
        let location = request.location.ok_or(anyhow::anyhow!("Location missing"))?;

        let (usage, clock) = match location {
            PlayLocation::Renderer(renderer_info) => (
                renderer_info.usage.ok_or(anyhow::anyhow!("No usage provided."))?,
                renderer_info.clock,
            ),
            _ => panic!("Expected Renderer PlayLocation"),
        };

        if let Some(clock_type) = clock {
            let reference_clock = Self::setup_reference_clock(clock_type)?;
            audio_renderer_proxy.set_reference_clock(reference_clock)?;
        }

        audio_renderer_proxy.set_usage(usage)?;
        audio_renderer_proxy.set_pcm_stream_type(&mut AudioStreamType::from(&format))?;
        audio_renderer_proxy
            .add_payload_buffer(0, vmo.duplicate_handle(zx::Rights::SAME_RIGHTS)?)?;
        audio_renderer_proxy.bind_gain_control(gain_control_server_end)?;

        let gain_control_proxy = gain_control_client_end.into_proxy()?;
        let (gain_db, mute) = {
            let settings =
                request.gain_settings.ok_or(anyhow::anyhow!("Gain settings argument missing."))?;
            (
                settings.gain.ok_or(anyhow::anyhow!("Gain value not specified."))?,
                settings.mute.ok_or(anyhow::anyhow!("Mute option not specified."))?,
            )
        };
        gain_control_proxy.set_gain(gain_db)?;
        gain_control_proxy.set_mute(mute)?;

        audio_renderer_proxy.enable_min_lead_time_events(true)?;

        // Wait for AudioRenderer to initialize (lead_time > 0)
        let mut stream = audio_renderer_proxy.take_event_stream();
        while let Some(event) = stream.try_next().await? {
            match event {
                fidl_fuchsia_media::AudioRendererEvent::OnMinLeadTimeChanged {
                    min_lead_time_nsec,
                } => {
                    if min_lead_time_nsec > 0 {
                        break;
                    }
                }
            }
        }

        responder.send(&mut Ok(response)).expect("Failed to send play response.");

        let offsets: Vec<usize> = (0..num_packets).map(|x| x * bytes_per_packet).collect();

        let futs = offsets.iter().map(|offset| async {
            Self::send_next_packet(
                offset.to_owned() as u64,
                fasync::Socket::from_socket(
                    data_socket.duplicate_handle(zx::Rights::SAME_RIGHTS)?,
                )?,
                vmo.duplicate_handle(zx::Rights::SAME_RIGHTS)?,
                &audio_renderer_proxy,
                bytes_per_packet,
                1,
            )
            .await?;
            Ok::<(), Error>(())
        });

        futures::future::try_join_all(futs).await?;

        let mut async_stdout =
            fasync::Socket::from_socket(stdout_local).expect("Async socket create failed.");
        async_stdout
            .write_all("Succesfully processed all audio data. \n".as_bytes())
            .await
            .expect("Write to socket failed.");
        Ok(())
    }

    async fn play_device(
        &self,
        request: AudioDaemonPlayRequest,
        responder: AudioDaemonPlayResponder,
    ) -> Result<(), anyhow::Error> {
        let (stdout_remote, stdout_local) = zx::Socket::create_stream();
        let (stderr_remote, _stderr_local) = zx::Socket::create_stream();

        let response = AudioDaemonPlayResponse {
            stdout: Some(stdout_remote),
            stderr: Some(stderr_remote),
            ..AudioDaemonPlayResponse::EMPTY
        };
        responder.send(&mut Ok(response)).expect("Failed to send play response.");

        let device_id = request
            .location
            .ok_or(anyhow::anyhow!("Device id argument missing."))
            .and_then(|play_location| match play_location {
                PlayLocation::RingBuffer(device_selector) => Ok(device_selector.id.unwrap()),
                _ => Err(anyhow::anyhow!("Expected Ring Buffer play location")),
            })?;

        let data_socket = request.socket.ok_or(anyhow::anyhow!("Socket argument missing."))?;

        let device = device::Device::connect(format!("/dev/class/audio-output/{}", device_id));

        let output_message = device.play(fasync::Socket::from_socket(data_socket)?).await?;
        let mut async_stdout =
            fasync::Socket::from_socket(stdout_local).expect("Async socket create failed.");

        async_stdout.write_all(output_message.as_bytes()).await.expect("Write to socket failed.");
        Ok(())
    }

    async fn serve(&mut self, mut stream: AudioDaemonRequestStream) -> Result<(), Error> {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                AudioDaemonRequest::Play { payload, responder } => match payload.location {
                    Some(PlayLocation::Renderer(..)) => {
                        self.play_renderer(payload, responder).await?;
                        Ok(())
                    }
                    Some(PlayLocation::RingBuffer(..)) => {
                        self.play_device(payload, responder).await?;
                        Ok(())
                    }
                    Some(..) => Err(anyhow::anyhow!("No PlayLocation variant specified.")),
                    None => Err(anyhow::anyhow!("PlayLocation argument missing. ")),
                }?,

                AudioDaemonRequest::Record { payload, responder } => {
                    match payload.location {
                        Some(RecordLocation::Capturer(..)) => {
                            self.record_capturer(payload, responder).await?;
                            Ok(())
                        }
                        Some(RecordLocation::Loopback(..)) => {
                            self.record_capturer(payload, responder).await?;
                            Ok(())
                        }
                        Some(..) => Err(anyhow::anyhow!("No RecordLocation variant specified.")),
                        None => Err(anyhow::anyhow!("RecordLocation argument missing. ")),
                    }?;
                }
                AudioDaemonRequest::ListDevices { responder } => {
                    let mut input_entries = device::get_entries("/dev/class/audio-input/").await?;
                    let mut output_entries =
                        device::get_entries("/dev/class/audio-output/").await?;

                    input_entries.append(&mut output_entries);

                    let response = AudioDaemonListDevicesResponse {
                        devices: Some(input_entries),
                        ..AudioDaemonListDevicesResponse::EMPTY
                    };
                    responder.send(&mut Ok(response))?;
                }

                AudioDaemonRequest::DeviceInfo { payload, responder } => {
                    let device_selector =
                        payload.device.ok_or(anyhow::anyhow!("No device specified"))?;

                    let path = format!(
                        "/dev/class/audio-{}/{}",
                        if device_selector.is_input.unwrap() { "input" } else { "output" },
                        device_selector.id.unwrap()
                    );
                    let device = device::Device::connect(path);

                    let info = device.get_info().await?;
                    let response = AudioDaemonDeviceInfoResponse {
                        device_info: Some(info),
                        ..AudioDaemonDeviceInfoResponse::EMPTY
                    };

                    responder.send(&mut Ok(response))?
                }
            }
        }
        Ok(())
    }
}

#[fuchsia::main(logging = true)]
async fn main() -> Result<(), anyhow::Error> {
    let mut service_fs = ServiceFs::new_local();

    // Initialize inspect
    inspect_runtime::serve(component::inspector(), &mut service_fs)?;
    component::health().set_starting_up();

    // Add services here. E.g:
    service_fs.dir("svc").add_fidl_service(IncomingRequest::AudioDaemon);
    service_fs.take_and_serve_directory_handle().context("Failed to serve outgoing namespace")?;

    component::health().set_ok();

    service_fs
        .for_each_concurrent(None, |request: IncomingRequest| async {
            // Match on `request` and handle each protocol.
            let mut audio_daemon = AudioDaemon {};

            match request {
                IncomingRequest::AudioDaemon(stream) => {
                    audio_daemon.serve(stream).await.unwrap_or_else(|e: Error| {
                        panic!("Couldn't serve audio daemon requests: {:?}", e)
                    })
                }
            }
        })
        .await;

    Ok(())
}

#[cfg(test)]
mod tests {
    #[fuchsia::test]
    async fn smoke_test() {
        assert!(true);
    }
}
