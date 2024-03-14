// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod device;
mod error;
mod ring_buffer;
mod socket;

use anyhow::{anyhow, Context, Error};
use error::ControllerError;
use fidl::endpoints::{create_endpoints, ServerEnd};
use fidl_fuchsia_audio_controller as fac;
use fidl_fuchsia_hardware_audio as fhaudio;
use fidl_fuchsia_media as fmedia;
use fidl_fuchsia_media_audio as fmedia_audio;
use fidl_fuchsia_ultrasound as fultrasound;
use format_utils::Format;
use fuchsia_async as fasync;
use fuchsia_component::client::connect_to_protocol;
use fuchsia_component::server::ServiceFs;
use fuchsia_inspect::{component, health::Reporter};
use fuchsia_zircon::{self as zx, HandleBased};
use futures::future::{BoxFuture, FutureExt};
use futures::{AsyncWriteExt, StreamExt, TryStreamExt};
use std::cmp;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tracing::error;

const SECONDS_PER_NANOSECOND: f64 = 1.0 / 10_u64.pow(9) as f64;

/// Wraps all hosted protocols into a single type that can be matched against
/// and dispatched.
enum IncomingRequest {
    DeviceControl(fac::DeviceControlRequestStream),
    Player(fac::PlayerRequestStream),
    Recorder(fac::RecorderRequestStream),
}

struct AudioDaemon {}

impl AudioDaemon {
    async fn record_capturer(
        &self,
        request: fac::RecorderRecordRequest,
    ) -> Result<fac::RecorderRecordResponse, ControllerError> {
        let location = request
            .source
            .ok_or(ControllerError::new(fac::Error::ArgumentsMissing, format!("Input missing.")))?;
        let wav_socket = request.wav_data.ok_or(ControllerError::new(
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

        let mut socket = socket::Socket {
            socket: &mut fasync::Socket::from_socket(
                wav_socket.duplicate_handle(zx::Rights::SAME_RIGHTS)?,
            ),
        };

        let capturer_proxy = Self::create_capturer_from_location(
            location,
            &format,
            stream_type,
            request.gain_settings,
        )
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

        let mut async_wav_writer = fidl::AsyncSocket::from_socket(wav_socket);

        socket.write_wav_header(duration, &format).await?;
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

    fn setup_reference_clock(clock_type: fac::ClockType) -> Result<Option<zx::Clock>, Error> {
        match clock_type {
            fac::ClockType::Flexible(_) => Ok(None),
            fac::ClockType::SystemMonotonic(_) => {
                let clock =
                    zx::Clock::create(zx::ClockOpts::CONTINUOUS | zx::ClockOpts::AUTO_START, None)
                        .map_err(|e| anyhow!("Creating reference clock failed: {}", e))?;
                let rights_clock = clock
                    .replace_handle(zx::Rights::READ | zx::Rights::DUPLICATE | zx::Rights::TRANSFER)
                    .map_err(|e| anyhow!("Replace handle for reference clock failed: {}", e))?;
                Ok(Some(rights_clock))
            }
            fac::ClockType::Custom(info) => {
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
                    .map_err(|e| anyhow!("Creating reference clock failed: {}", e))?;

                clock
                    .update(update_builder.build())
                    .map_err(|e| anyhow!("Updating reference clock failed: {}", e))?;

                Ok(Some(
                    clock
                        .replace_handle(
                            zx::Rights::READ | zx::Rights::DUPLICATE | zx::Rights::TRANSFER,
                        )
                        .map_err(|e| anyhow!("Replace handle for reference clock failed: {}", e))?,
                ))
            }
            fac::ClockTypeUnknown!() => Ok(None),
        }
    }

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
                        let reference_clock = Self::setup_reference_clock(clock_type)?;
                        capturer_proxy.set_reference_clock(reference_clock)?;
                    }
                    Ok(capturer_proxy)
                }
                fac::CapturerConfig::UltrasoundCapturer(_) => {
                    let component = connect_to_protocol::<fultrasound::FactoryMarker>()
                        .context("Failed to connect to fuchsia.ultrasound.Factory")?;
                    let (_reference_clock, stream_type) =
                        component.create_capturer(server_end).await?;
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

    async fn create_renderer_from_location(
        location: fac::PlayDestination,
        format: &Format,
        gain_settings: Option<fac::GainSettings>,
    ) -> Result<fmedia::AudioRendererProxy, Error> {
        let (client_end, server_end) = create_endpoints::<fmedia::AudioRendererMarker>();

        let audio_renderer_proxy = client_end
            .into_proxy()
            .map_err(|e| anyhow!("Error getting AudioRendererProxy: {e}"))?;

        if let fac::PlayDestination::Renderer(renderer_config) = location {
            match renderer_config {
                fac::RendererConfig::UltrasoundRenderer(_) => {
                    let component = connect_to_protocol::<fultrasound::FactoryMarker>()
                        .context("Failed to connect to fuchsia.ultrasound.Factory")?;
                    let (_reference_clock, stream_type) =
                        component.create_renderer(server_end).await?;

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
                        let reference_clock = Self::setup_reference_clock(clock_type)?;
                        audio_renderer_proxy.set_reference_clock(reference_clock)?;
                    }

                    if let Some(usage) = renderer_config.usage {
                        audio_renderer_proxy.set_usage(usage)?;
                    }

                    audio_renderer_proxy
                        .set_pcm_stream_type(&fmedia::AudioStreamType::from(format))?;

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

    fn send_next_packet<'b>(
        payload_offset: u64,
        mut socket: fidl::AsyncSocket,
        vmo: zx::Vmo,
        audio_renderer_proxy: &'b fmedia::AudioRendererProxy,
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
                Self::send_next_packet(
                    payload_offset,
                    socket,
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

    async fn play_renderer(
        &self,
        request: fac::PlayerPlayRequest,
    ) -> Result<fac::PlayerPlayResponse, Error> {
        let data_socket = request.wav_source.ok_or(anyhow!("Socket argument missing."))?;

        let mut socket = socket::Socket {
            socket: &mut fasync::Socket::from_socket(
                data_socket.duplicate_handle(zx::Rights::SAME_RIGHTS)?,
            ),
        };
        let spec = socket.read_wav_header().await?;
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

        let audio_renderer_proxy = Rc::new(
            Self::create_renderer_from_location(location, &format, request.gain_settings).await?,
        );

        let vmo_size_bytes = format.frames_per_second as usize * format.bytes_per_frame() as usize;
        let vmo = zx::Vmo::create(vmo_size_bytes as u64)?;

        let bytes_per_packet = cmp::min(vmo_size_bytes / packet_count, 32000 as usize);

        audio_renderer_proxy
            .add_payload_buffer(0, vmo.duplicate_handle(zx::Rights::SAME_RIGHTS)?)?;

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
            Self::send_next_packet(
                offset.to_owned() as u64,
                fasync::Socket::from_socket(data_socket.duplicate_handle(zx::Rights::SAME_RIGHTS)?),
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

    async fn play_device(
        &self,
        request: fac::PlayerPlayRequest,
    ) -> Result<fac::PlayerPlayResponse, Error> {
        let device_selector = request
            .destination
            .ok_or(anyhow!("Device id argument missing."))
            .and_then(|play_location| match play_location {
                fac::PlayDestination::DeviceRingBuffer(device_selector) => Ok(device_selector),
                _ => Err(anyhow!("Expected Ring Buffer play location")),
            })?;

        let data_socket = request.wav_source.ok_or(anyhow!("Socket argument missing."))?;
        let async_socket = fasync::Socket::from_socket(data_socket);

        let mut device = device::Device::new_from_selector(&device_selector)?;

        device.play(async_socket).await
    }

    async fn record_device(
        &self,
        request: fac::RecorderRecordRequest,
    ) -> Result<fac::RecorderRecordResponse, error::ControllerError> {
        let stream_type = request.stream_type.ok_or(error::ControllerError::new(
            fac::Error::ArgumentsMissing,
            format!("Stream type missing."),
        ))?;
        let wav_socket = request.wav_data.ok_or(error::ControllerError::new(
            fac::Error::ArgumentsMissing,
            format!("Socket for wav data missing."),
        ))?;

        let device_selector = request
            .source
            .ok_or(error::ControllerError::new(
                fac::Error::ArgumentsMissing,
                format!("Record source missing."),
            ))
            .and_then(|location| match location {
                fac::RecordSource::DeviceRingBuffer(device_selector) => Ok(device_selector),
                unknown_source => Err(error::ControllerError::new(
                    fac::Error::InvalidArguments,
                    format!("Expected ring buffer source, found {unknown_source:?}"),
                )),
            })?;

        let cancel_server = request.canceler;
        let duration = request.duration.map(|duration| Duration::from_nanos(duration as u64));

        let mut device = device::Device::new_from_selector(&device_selector).map_err(|err| {
            error::ControllerError::new(
                fac::Error::DeviceNotReachable,
                format!("Failed to connect to device with error: {err}"),
            )
        })?;

        device
            .record(
                Format::from(&stream_type),
                fasync::Socket::from_socket(wav_socket),
                duration,
                cancel_server,
            )
            .await
    }

    async fn serve_player(&mut self, mut stream: fac::PlayerRequestStream) -> Result<(), Error> {
        while let Ok(Some(request)) = stream.try_next().await {
            let request_name = request.method_name();
            let result = match request {
                fac::PlayerRequest::Play { payload, responder } => {
                    let response = match payload.destination {
                        Some(fac::PlayDestination::Renderer(..)) => {
                            self.play_renderer(payload).await.map_err(|e| {
                                println!("Error trying to play to AudioRenderer {e}");
                                fac::Error::UnknownFatal
                            })
                        }
                        Some(fac::PlayDestination::DeviceRingBuffer(..)) => {
                            self.play_device(payload).await.map_err(|e| {
                                println!("Error trying to play to device ring buffer {e}");
                                fac::Error::UnknownFatal
                            })
                        }
                        Some(unknown_destination) => {
                            println!("Unsupported PlayDestination variant specified. Requested: {:?} not supported.", unknown_destination);
                            Err(fac::Error::InvalidArguments)
                        }
                        None => {
                            println!("Missing destination argument.");
                            Err(fac::Error::ArgumentsMissing)
                        }
                    };

                    responder.send(response).map_err(|e| anyhow!("Could not send reponse: {}", e))
                }
                _ => Err(anyhow!("Request {request_name} not supported.")),
            };

            match result {
                Ok(_) => println!("Request succeeded."),
                Err(e) => {
                    let error_msg = format!("Request {request_name} failed with error {e} \n");
                    println!("{}", &error_msg);
                }
            }
        }
        Ok(())
    }

    async fn serve_recorder(
        &mut self,
        mut stream: fac::RecorderRequestStream,
    ) -> Result<(), Error> {
        while let Ok(Some(request)) = stream.try_next().await {
            let request_name = request.method_name();
            match request {
                fac::RecorderRequest::Record { payload, responder } => {
                    let record_result = match payload.source {
                        Some(fac::RecordSource::Capturer(..))
                        | Some(fac::RecordSource::Loopback(..)) => {
                            self.record_capturer(payload).await
                        }
                        Some(fac::RecordSource::DeviceRingBuffer(..)) => {
                            self.record_device(payload).await
                        }
                        Some(unknown_source) => {
                            let error_msg = format!("Unsupported RecordSource variant specified. Requested: {unknown_source:?} not supported.");
                            println!("{error_msg}");
                            Err(ControllerError::new(fac::Error::InvalidArguments, error_msg))
                        }
                        None => {
                            println!("RecordSource argument missing.");
                            Err(ControllerError::new(
                                fac::Error::ArgumentsMissing,
                                format!("RecordSource argument missing"),
                            ))
                        }
                    };
                    match record_result {
                        Ok(response) => {
                            println!("Request succeeded.");
                            responder
                                .send(Ok(response))
                                .map_err(|e| anyhow!("Could not send reponse: {e}"))
                        }
                        Err(e) => {
                            println!("Request {request_name} failed with error {e} \n");
                            responder
                                .send(Err(e.inner))
                                .map_err(|e| anyhow!("Could not send reponse: {e}"))
                        }
                    }
                }
                _ => Err(anyhow!("Request {request_name} not supported.")),
            }?;
        }
        Ok(())
    }

    // TODO(b/298683668) this will be removed, replaced by client direct calls.
    async fn serve_device_control(
        &mut self,
        mut stream: fac::DeviceControlRequestStream,
    ) -> Result<(), Error> {
        while let Ok(Some(request)) = stream.try_next().await {
            let request_name = request.method_name();
            let request_result = match request {
                fac::DeviceControlRequest::ListDevices { responder } => {
                    let mut entries = Vec::<fac::DeviceSelector>::new();
                    let devfs_devices = [
                        (fhaudio::DeviceType::StreamConfig, "/dev/class/audio-input/", Some(true)),
                        (
                            fhaudio::DeviceType::StreamConfig,
                            "/dev/class/audio-output/",
                            Some(false),
                        ),
                        (fhaudio::DeviceType::Composite, "/dev/class/audio-composite/", None),
                    ];

                    for (device_type, path, is_input) in devfs_devices {
                        match device::get_entries(path, device_type, is_input).await {
                            Ok(mut device_entries) => entries.append(&mut device_entries),
                            Err(e) => {
                                println!("Failed to get {device_type:?} entries: {e}")
                            }
                        }
                    }

                    let response = fac::DeviceControlListDevicesResponse {
                        devices: Some(entries),
                        ..Default::default()
                    };
                    responder.send(Ok(response)).map_err(|e| anyhow!("Error sending response: {e}"))
                }
                fac::DeviceControlRequest::GetDeviceInfo { payload, responder } => {
                    let device_selector = payload.device.ok_or(anyhow!("No device specified"))?;

                    let mut device = device::Device::new_from_selector(&device_selector)?;

                    let info = device.get_info().await;
                    match info {
                        Ok(info) => {
                            let response = fac::DeviceControlGetDeviceInfoResponse {
                                device_info: Some(info),
                                ..Default::default()
                            };
                            responder
                                .send(Ok(response))
                                .map_err(|e| anyhow!("Error sending response: {e}"))
                        }
                        Err(e) => {
                            println!("Could not connect to device. {e}");
                            responder
                                .send(Err(zx::Status::INTERNAL.into_raw()))
                                .map_err(|e| anyhow!("Error sending response: {e}"))
                        }
                    }
                }
                fac::DeviceControlRequest::DeviceSetGainState { payload, responder } => {
                    let (device_selector, gain_state) = (
                        payload.device.ok_or(anyhow!("No device specified"))?,
                        payload.gain_state.ok_or(anyhow!("No gain state specified"))?,
                    );

                    let mut device = device::Device::new_from_selector(&device_selector)?;

                    device.set_gain(gain_state)?;
                    responder.send(Ok(())).map_err(|e| anyhow!("Error sending response: {e}"))
                }
                _ => Err(anyhow!("Request {request_name} not supported.")),
            };
            match request_result {
                Ok(_) => println!("Request succeeded."),
                Err(e) => {
                    let error_msg = format!("Request {request_name} failed with error {e} \n");
                    println!("{}", &error_msg);
                }
            }
        }
        Ok(())
    }
}

pub async fn stop_listener(
    canceler: ServerEnd<fac::RecordCancelerMarker>,
    stop_signal: &AtomicBool,
) -> Result<(), Error> {
    let mut stream = canceler
        .into_stream()
        .map_err(|e| anyhow!("Error turning canceler server into stream {}", e))?;

    match stream.try_next().await {
        Ok(Some(request)) => match request {
            fac::RecordCancelerRequest::Cancel { responder } => {
                stop_signal.store(true, Ordering::SeqCst);
                responder.send(Ok(())).context("FIDL error with stop request")
            }
            _ => Err(anyhow!("Request not supported.")),
        },
        Ok(None) | Err(_) => {
            stop_signal.store(true, Ordering::SeqCst);
            Err(anyhow!("FIDL error with stop request"))
        }
    }
}

#[fuchsia::main(logging = true)]
async fn main() -> Result<(), Error> {
    let mut service_fs = ServiceFs::new_local();

    // Initialize inspect
    let _inspect_server_task = inspect_runtime::publish(
        component::inspector(),
        inspect_runtime::PublishOptions::default(),
    );
    component::health().set_starting_up();

    // Add services here. E.g:
    service_fs.dir("svc").add_fidl_service(IncomingRequest::DeviceControl);
    service_fs.dir("svc").add_fidl_service(IncomingRequest::Player);
    service_fs.dir("svc").add_fidl_service(IncomingRequest::Recorder);
    service_fs.take_and_serve_directory_handle().context("Failed to serve outgoing namespace")?;

    component::health().set_ok();

    service_fs
        .for_each_concurrent(None, |request: IncomingRequest| async {
            // Match on `request` and handle each protocol.
            let mut audio_daemon = AudioDaemon {};

            match request {
                IncomingRequest::DeviceControl(stream) => {
                    if let Err(err) = audio_daemon.serve_device_control(stream).await {
                        error!(%err, "Failed to serve DeviceControl protocol");
                    }
                }
                IncomingRequest::Player(stream) => {
                    if let Err(err) = audio_daemon.serve_player(stream).await {
                        error!(%err, "Failed to serve Player protocol");
                    }
                }
                IncomingRequest::Recorder(stream) => {
                    if let Err(err) = audio_daemon.serve_recorder(stream).await {
                        error!(%err, "Failed to serve Recorder protocol");
                    }
                }
            }
        })
        .await;

    Ok(())
}
