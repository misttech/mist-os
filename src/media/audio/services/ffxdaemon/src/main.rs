// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{self, Context, Error},
    async_lock as _, audio_daemon_utils, fdio,
    fidl::endpoints::Proxy,
    fidl::HandleBased,
    fidl_fuchsia_audio_ffxdaemon::{
        AudioDaemonDeviceInfoResponse, AudioDaemonListDevicesResponse, AudioDaemonPlayRequest,
        AudioDaemonPlayResponder, AudioDaemonPlayResponse, AudioDaemonRecordRequest,
        AudioDaemonRecordResponder, AudioDaemonRecordResponse, AudioDaemonRequest,
        AudioDaemonRequestStream, DeviceInfo, PlayLocation, RecordLocation,
    },
    fidl_fuchsia_hardware_audio::PcmFormat,
    fidl_fuchsia_io as fio,
    fidl_fuchsia_media::{AudioRendererProxy, AudioStreamType},
    fidl_fuchsia_media_audio, fuchsia as _, fuchsia_async as fasync,
    fuchsia_component::server::ServiceFs,
    fuchsia_inspect::{component, health::Reporter},
    fuchsia_runtime::vmar_root_self,
    fuchsia_zircon::{self as zx},
    futures::future::{BoxFuture, FutureExt},
    futures::prelude::*,
    futures::StreamExt,
    hound,
    std::cmp,
    std::io::{Cursor, Seek, SeekFrom, Write},
    std::rc::Rc,
};

const SECONDS_PER_NANOSECOND: f64 = 1.0 / 10_u64.pow(9) as f64;

/// Wraps all hosted protocols into a single type that can be matched against
/// and dispatched.
enum IncomingRequest {
    AudioDaemon(AudioDaemonRequestStream),
}

struct RingBuffer {
    vmo: zx::Vmo,
    base_address: usize,
    num_frames: u64,
    bytes_per_frame: u64,
}

impl RingBuffer {
    pub fn new(vmo: zx::Vmo, num_frames: u64, bytes_per_frame: u64) -> Self {
        let base_address = vmar_root_self()
            .map(
                0,
                &vmo,
                0,
                vmo.get_size().unwrap() as usize,
                zx::VmarFlags::PERM_READ | zx::VmarFlags::PERM_WRITE,
            )
            .unwrap();
        Self { vmo, base_address, num_frames, bytes_per_frame }
    }

    pub fn write_to_frame(&self, frame: u64, buf: &mut Vec<u8>) -> Result<(), Error> {
        if buf.len() % self.bytes_per_frame as usize != 0 {
            panic!("Must pass buffer with complete frames.")
        }
        let frame_offset = frame % self.num_frames;
        let byte_offset = frame_offset * self.bytes_per_frame;
        let num_frames_in_buf = buf.len() as u64 / self.bytes_per_frame;

        // Check whether buffer can be written continuously or needs to be split into
        // two writes, one to the end of the buffer and one starting from the beginning.
        if (frame_offset + num_frames_in_buf) <= self.num_frames {
            self.vmo.write(&buf[..], byte_offset)?;

            // Flush cache so that hardware reads most recent write.
            unsafe {
                // SAFETY: The flushed range is guaranteed to be in-bounds of the VMO since
                // frame_offset + num_frames_in_buf <= self.num_frames.
                zx::Status::ok(zx::sys::zx_cache_flush(
                    (self.base_address as u64 + byte_offset) as *mut u8,
                    buf.len(),
                    zx::sys::ZX_CACHE_FLUSH_DATA,
                ))?;
            }
        } else {
            let frames_to_write_until_end = self.num_frames - frame_offset;
            let bytes_until_buffer_end =
                (frames_to_write_until_end * self.bytes_per_frame) as usize;

            self.vmo.write(&buf[..bytes_until_buffer_end], byte_offset)?;
            // Flush cache so that hardware reads most recent write.
            unsafe {
                // SAFETY: The flushed range is guaranteed to be in-bounds of the VMO since
                // frame_offset + num_frames_in_buf <= self.num_frames.
                zx::Status::ok(zx::sys::zx_cache_flush(
                    (self.base_address as u64 + byte_offset) as *mut u8,
                    bytes_until_buffer_end,
                    zx::sys::ZX_CACHE_FLUSH_DATA,
                ))?;
            }

            // Write what remains to the beginning of the buffer.
            self.vmo.write(&buf[bytes_until_buffer_end..], 0)?;
            unsafe {
                // SAFETY: The flushed range is guaranteed to be in-bounds of the VMO since
                // frame_offset + num_frames_in_buf <= self.num_frames.
                zx::Status::ok(zx::sys::zx_cache_flush(
                    self.base_address as *mut u8,
                    buf.len() - bytes_until_buffer_end as usize,
                    zx::sys::ZX_CACHE_FLUSH_DATA,
                ))?;
            }
        }
        Ok(())
    }
}

impl Drop for RingBuffer {
    fn drop(&mut self) {
        // Safety:
        //
        // base_address is private to self, so no other code can observe that this mapping
        // has been removed.
        unsafe {
            vmar_root_self()
                .unmap(self.base_address, self.vmo.get_size().unwrap() as usize)
                .unwrap();
        }
    }
}

struct AudioDaemon {}
impl AudioDaemon {
    pub fn new() -> Self {
        Self {}
    }

    async fn record_capturer(
        &self,
        request: AudioDaemonRecordRequest,
        responder: AudioDaemonRecordResponder,
    ) -> Result<(), anyhow::Error> {
        let audio_component =
            fuchsia_component::client::connect_to_protocol::<fidl_fuchsia_media::AudioMarker>()
                .context("Failed to connect to fuchsia.media.Audio")?;

        let location = request.location.ok_or(anyhow::anyhow!("Input missing."))?;

        let (capturer_usage, loopback) = match location {
            RecordLocation::Capturer(capturer_info) => (capturer_info.usage, false),
            RecordLocation::Loopback(..) => (None, true),
            _ => panic!("Expected Capturer RecordLocation"),
        };

        let mut stream_type = request.stream_type.ok_or(anyhow::anyhow!("Stream type missing"))?;

        let (stdout_remote, stdout_local) = zx::Socket::create(zx::SocketOpts::STREAM)?;
        let (stderr_remote, _stderr_local) = zx::Socket::create(zx::SocketOpts::STREAM)?;

        let (client_end, server_end) =
            fidl::endpoints::create_endpoints::<fidl_fuchsia_media::AudioCapturerMarker>()?;

        let response = AudioDaemonRecordResponse {
            stdout: Some(stdout_remote),
            stderr: Some(stderr_remote),
            ..AudioDaemonRecordResponse::EMPTY
        };
        responder.send(&mut Ok(response)).expect("Failed to send play response.");

        let spec = audio_daemon_utils::spec_from_stream_type(stream_type);

        // TODO(fxbug.dev/109807): Support capture until stop.
        let frames_to_capture = {
            let duration_nanos =
                request.duration.ok_or(anyhow::anyhow!("Duration argument missing."))? as u64;
            let duration = std::time::Duration::from_nanos(duration_nanos);

            (stream_type.frames_per_second as f64 * duration.as_secs_f64()).ceil() as u64
        };

        let bytes_per_frame = audio_daemon_utils::stream_type_bytes_per_frame(stream_type);
        let buffer_size_bytes = stream_type.frames_per_second as u64 * bytes_per_frame as u64;
        let vmo = zx::Vmo::create(buffer_size_bytes)?;
        let num_packets = 4;
        let bytes_per_packet = buffer_size_bytes / num_packets;

        // A valid Wav File Header must have the data format and data length fields.
        // We need all values corresponding to wav header fields set on the cursor_writer before
        // writing to stdout.
        let mut cursor_writer = Cursor::new(Vec::<u8>::new());
        {
            // Creation of WavWriter writes the Wav File Header to cursor_writer.
            // This written header has the file size field and data chunk size field both set to 0,
            // since the number of samples (and resulting file and chunk sizes) are unknown to
            // the WavWriter at this point.
            let _writer = hound::WavWriter::new(&mut cursor_writer, spec).unwrap();
        }

        // The file and chunk size fields are set to 0 as placeholder values by the construction of
        // the WavWriter above. We can compute the actual values based on the command arguments
        // for format and duration, and set the file size and chunk size fields to the computed
        // values in the cursor_writer before writing to stdout.

        let bytes_to_capture: u32 = frames_to_capture as u32
            * (audio_daemon_utils::stream_type_bytes_per_frame(stream_type)) as u32;
        let total_header_bytes = 44usize;
        // The File Size field of a WAV header. 32-bit int starting at position 4, represents
        // the size of the overall file minus 8 bytes (exclude RIFF description and file size description)
        let file_size_bytes: u32 = bytes_to_capture as u32 + total_header_bytes as u32 - 8;
        let packets_to_capture = (bytes_to_capture as f64 / bytes_per_packet as f64).ceil() as u64;
        cursor_writer.seek(SeekFrom::Start(4))?;
        cursor_writer.write_all(&file_size_bytes.to_le_bytes()[..])?;

        // Data size field of a WAV header. For PCM, this is a 32-bit int starting at position 40,
        // and represents the size of the data section.
        cursor_writer.seek(SeekFrom::Start(40))?;
        cursor_writer.write_all(&bytes_to_capture.to_le_bytes()[..])?;

        // Write the completed WAV header to stdout. We then write the raw sample values from the
        // packets received directly to stdout.

        let mut header_bytes_written = 0usize;
        let header = cursor_writer.into_inner();
        while header_bytes_written < total_header_bytes {
            header_bytes_written += stdout_local.write(&header)?;
        }

        audio_component.create_audio_capturer(server_end, loopback)?;

        let capturer_proxy = client_end.into_proxy()?;

        capturer_proxy.set_pcm_stream_type(&mut stream_type)?;
        if !(loopback) {
            match capturer_usage {
                Some(capturer_usage) => capturer_proxy.set_usage(capturer_usage)?,
                None => panic!("No usage specified how to capture audio."),
            }
        }

        let frames_per_packet = bytes_per_packet / bytes_per_frame;
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

    async fn read_socket_into_buf(
        socket: &mut fidl::AsyncSocket,
        buf: &mut Vec<u8>,
    ) -> Result<u64, Error> {
        let mut bytes_read_so_far = 0;

        loop {
            let bytes_read = socket.read(&mut buf[bytes_read_so_far..]).await?;
            bytes_read_so_far += bytes_read;

            if bytes_read == 0 || bytes_read_so_far == buf.len() as usize {
                break;
            }
        }

        Ok(bytes_read_so_far as u64)
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
            let mut buf = vec![0u8; bytes_per_packet];
            let total_bytes_read =
                Self::read_socket_into_buf(&mut socket, &mut buf).await? as usize;

            if total_bytes_read == 0 {
                return Ok(());
            }
            vmo.write(&buf[..total_bytes_read], payload_offset as u64)?;

            let packet_fut =
                audio_renderer_proxy.send_packet(&mut fidl_fuchsia_media::StreamPacket {
                    pts: fidl_fuchsia_media::NO_TIMESTAMP,
                    payload_buffer_id: 0,
                    payload_offset: payload_offset as u64,
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
        let (stdout_remote, stdout_local) = zx::Socket::create(zx::SocketOpts::STREAM)?;
        let (stderr_remote, _stderr_local) = zx::Socket::create(zx::SocketOpts::STREAM)?;

        let (client_end, server_end) =
            fidl::endpoints::create_endpoints::<fidl_fuchsia_media::AudioRendererMarker>()?;

        let (gain_control_client_end, gain_control_server_end) =
            fidl::endpoints::create_endpoints::<fidl_fuchsia_media_audio::GainControlMarker>()?;
        let data_socket = request.socket.ok_or(anyhow::anyhow!("Socket argument missing."))?;

        audio_component.create_audio_renderer(server_end)?;
        let audio_renderer_proxy = Rc::new(client_end.into_proxy()?);

        let response = AudioDaemonPlayResponse {
            stdout: Some(stdout_remote),
            stderr: Some(stderr_remote),
            ..AudioDaemonPlayResponse::EMPTY
        };

        let spec = {
            let mut header_buf = vec![0u8; 44];
            fasync::Socket::from_socket(data_socket.duplicate_handle(zx::Rights::SAME_RIGHTS)?)?
                .read_exact(&mut header_buf)
                .await?;
            let cursor_header = Cursor::new(header_buf);
            let reader = hound::WavReader::new(cursor_header.clone())?;
            reader.spec()
        };

        let (bytes_per_sample, sample_format) = match spec.sample_format {
            hound::SampleFormat::Int => match spec.bits_per_sample {
                0..=8 => (1, fidl_fuchsia_media::AudioSampleFormat::Unsigned8),
                9..=16 => (2, fidl_fuchsia_media::AudioSampleFormat::Signed16),
                17..=32 => (4, fidl_fuchsia_media::AudioSampleFormat::Signed24In32),
                33.. => panic!("Unsupported bits per sample."),
            },
            hound::SampleFormat::Float => (4, fidl_fuchsia_media::AudioSampleFormat::Float),
        };

        let mut pcm_stream_type = AudioStreamType {
            sample_format,
            channels: spec.channels as u32,
            frames_per_second: spec.sample_rate,
        };

        let vmo_size_bytes = spec.sample_rate as usize * bytes_per_sample as usize;
        let vmo = zx::Vmo::create(vmo_size_bytes as u64)?;

        let bytes_per_packet = cmp::min(vmo_size_bytes / num_packets as usize, 32000 as usize);
        let location = request.location.ok_or(anyhow::anyhow!("Location missing"))?;

        let usage = match location {
            PlayLocation::Renderer(renderer_info) => {
                renderer_info.usage.ok_or(anyhow::anyhow!("No usage provided."))?
            }
            _ => panic!("Expected Renderer PlayLocation"),
        };

        audio_renderer_proxy.set_usage(usage)?;

        audio_renderer_proxy.set_pcm_stream_type(&mut pcm_stream_type)?;

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
        let (stdout_remote, stdout_local) = zx::Socket::create(zx::SocketOpts::STREAM)?;
        let (stderr_remote, _stderr_local) = zx::Socket::create(zx::SocketOpts::STREAM)?;

        let device_id = request
            .location
            .ok_or(anyhow::anyhow!("Device id argument missing."))
            .and_then(|play_location| match play_location {
                PlayLocation::RingBuffer(device_selector) => Ok(device_selector.id.unwrap()),
                _ => Err(anyhow::anyhow!("Expected Ring Buffer play location")),
            })?;

        let response = AudioDaemonPlayResponse {
            stdout: Some(stdout_remote),
            stderr: Some(stderr_remote),
            ..AudioDaemonPlayResponse::EMPTY
        };
        responder.send(&mut Ok(response)).expect("Failed to send play response.");

        // Connect to a StreamConfig channel.
        let (connector_client, connector_server) = fidl::endpoints::create_proxy::<
            fidl_fuchsia_hardware_audio::StreamConfigConnectorMarker,
        >()
        .expect("failed to create streamconfig");

        let (stream_config_client, stream_config_connector) =
            fidl::endpoints::create_proxy::<fidl_fuchsia_hardware_audio::StreamConfigMarker>()
                .expect("failed to create streamconfig ");

        let device_path = format!("/dev/class/audio-output/{}", device_id);
        fdio::service_connect(&device_path, connector_server.into_channel())
            .context(format!("failed to connect to {}", &device_path))?;

        // Using StreamConfigConnector client, pass the server end of StreamConfig
        // channel to the device so that device can respond to StreamConfig requests.
        connector_client.connect(stream_config_connector)?;
        let supported_formats = stream_config_client.get_supported_formats().await?;

        // Create ring buffer channel.
        let (ring_buffer_client, ring_buffer_server) =
            fidl::endpoints::create_proxy::<fidl_fuchsia_hardware_audio::RingBufferMarker>()
                .expect("failed to create ring buffer channel");

        let data_socket = request.socket.ok_or(anyhow::anyhow!("Socket argument missing."))?;

        let spec = {
            let mut header_buf = vec![0u8; 44];
            fasync::Socket::from_socket(data_socket.duplicate_handle(zx::Rights::SAME_RIGHTS)?)?
                .read_exact(&mut header_buf)
                .await?;
            let cursor_header = Cursor::new(header_buf);
            let reader = hound::WavReader::new(cursor_header.clone())?;
            reader.spec()
        };

        let (bytes_per_sample, valid_bits_per_sample, sample_format, silence_value) = match spec
            .sample_format
        {
            hound::SampleFormat::Int => match spec.bits_per_sample {
                0..=8 => Ok((1, 8, fidl_fuchsia_hardware_audio::SampleFormat::PcmUnsigned, 128)),
                9..=16 => Ok((2, 16, fidl_fuchsia_hardware_audio::SampleFormat::PcmSigned, 0)),
                17..=32 => Ok((4, 24, fidl_fuchsia_hardware_audio::SampleFormat::PcmSigned, 0)),
                33.. => Err(anyhow::anyhow!("Unsupported bits per sample.")),
            },
            hound::SampleFormat::Float => {
                Ok((4, 32, fidl_fuchsia_hardware_audio::SampleFormat::PcmFloat, 0))
            }
        }?;

        let requested_format = fidl_fuchsia_hardware_audio::Format {
            pcm_format: Some(PcmFormat {
                number_of_channels: spec.channels as u8,
                sample_format,
                bytes_per_sample,
                valid_bits_per_sample,
                frame_rate: spec.sample_rate,
            }),
            ..fidl_fuchsia_hardware_audio::Format::EMPTY
        };

        let mut is_format_supported = false;

        for format in supported_formats {
            let pcm_formats = format.pcm_supported_formats.unwrap();

            if pcm_formats
                .frame_rates
                .unwrap()
                .contains(&requested_format.pcm_format.unwrap().frame_rate)
                && pcm_formats
                    .bytes_per_sample
                    .unwrap()
                    .contains(&requested_format.pcm_format.unwrap().bytes_per_sample)
                && pcm_formats
                    .sample_formats
                    .unwrap()
                    .contains(&requested_format.pcm_format.unwrap().sample_format)
                && pcm_formats
                    .valid_bits_per_sample
                    .unwrap()
                    .contains(&requested_format.pcm_format.unwrap().valid_bits_per_sample)
                && pcm_formats.channel_sets.unwrap().into_iter().any(|channel_set| {
                    channel_set.attributes.unwrap().len()
                        == requested_format.pcm_format.unwrap().number_of_channels as usize
                })
            {
                is_format_supported = true;
                break;
            }
        }

        if !is_format_supported {
            panic!("Requested format not supported");
        }

        let bytes_per_frame = (spec.channels * spec.bits_per_sample / 8) as u64;
        stream_config_client.create_ring_buffer(requested_format, ring_buffer_server)?;

        let mut data_socket = fasync::Socket::from_socket(data_socket)?;

        let res = ring_buffer_client
            .get_vmo(spec.sample_rate / 10, 0 /* ring buffer notifications unused */)
            .await?;

        let (num_frames_in_rb, vmo) = match res {
            Ok((num_frames, vmo)) => (num_frames as u64, vmo),
            Err(_) => panic!("couldn't receive vmo "),
        };

        let properties = ring_buffer_client.get_properties().await?;

        // Hardware might not use all bytes in vmo. Only want to write to frames hardware will read from.
        let bytes_in_rb = num_frames_in_rb as u64 * bytes_per_frame as u64;
        let bytes_in_vmo = vmo.get_size()?;
        let frames_per_second = spec.sample_rate; // hound WavSpec uses sample_rate == frame rate.

        if bytes_in_rb > bytes_in_vmo {
            println!("Bad ring buffer size returned by audio driver! \n (kernel size = {} bytes, driver size = {} bytes. ", bytes_in_vmo, bytes_in_rb);
            panic!();
        }

        /*
        High-water approach
            - sleep for time equivalent to a small portion of ring buffer
            - on wake up, write from last byte written until point where driver has just
                finished reading
                    - if driver read region has wrapped around, this will take two writes:
                        - one to end of ring buffer
                        - one from beginning of ring buffer

            - sleep again

            repeat above steps until all bytes have been written back to silence.

                                        driver read
                                            region
                                    ┌───────────────────────┐
                                    ▼ internal delay bytes  ▼
        +-----------------------------------------------------------------------+
        |                              (rb pointer in here)                     |
        +-----------------------------------------------------------------------+
                ▲                   ▲
                |                   |
            last frame            write up to
            written                 here
                └─────────┬─────────┘
                    this length will
                    vary depending
                    on wakeup time

        */

        let mut silenced_frames = 0u64;
        let mut late_wakeups = 0;
        let mut last_frame_written = 0u64;

        let nanos_per_wakeup_interval = 10e6f64; // 10 milliseconds

        let frames_per_nanosecond = frames_per_second as f64 * SECONDS_PER_NANOSECOND;
        let producer_bytes =
            (frames_per_nanosecond * nanos_per_wakeup_interval).floor() as u64 / bytes_per_frame;

        let consumer_bytes =
            properties.fifo_depth.ok_or(anyhow::anyhow!("No fifo depth available."))? as u64;

        if consumer_bytes + producer_bytes > bytes_in_rb {
            panic!("Ring buffer not large enough for internal delay")
        }

        let ring_buffer = RingBuffer::new(vmo, num_frames_in_rb, bytes_per_frame);

        let t_zero_nanos = ring_buffer_client.start().await?;
        println!(
            "Time between present and t0 returned by start {}",
            (zx::Time::get_monotonic() - zx::Time::from_nanos(t_zero_nanos)).into_nanos()
        );

        // Running counter representing the next time we'll wake up and write to ring buffer.
        // To start, sleep until at least t0 + (wakeupinterval) so we can start writing at
        // the first bytes in the ring buffer.
        let mut next_wakeup_nanos = t_zero_nanos as f64 + nanos_per_wakeup_interval;

        let mut time_of_last_wakeup = zx::Time::from_nanos(t_zero_nanos);

        loop {
            let wakeup_time = zx::Time::from_nanos(next_wakeup_nanos.round() as i64);
            wakeup_time.sleep(); // Wraps zx::nanosleep(wakeup_time);

            // Check that we woke up on time. Approximate ring buffer pointer position based on
            // clock time. Ring buffer pointer should be ahead of last byte written.
            let now = zx::Time::get_monotonic();

            let duration_since_last_wakeup = now - time_of_last_wakeup;
            time_of_last_wakeup = now;
            next_wakeup_nanos = now.into_nanos() as f64 + nanos_per_wakeup_interval;

            let total_time_elapsed = now - zx::Time::from_nanos(t_zero_nanos);
            let total_rb_frames_elapsed =
                frames_per_nanosecond * total_time_elapsed.into_nanos() as f64;

            let rb_frames_elapsed_since_last_wakeup =
                frames_per_nanosecond * duration_since_last_wakeup.into_nanos() as f64;

            let new_frames_available_to_write =
                total_rb_frames_elapsed.floor() as u64 - last_frame_written;
            let num_bytes_to_write = new_frames_available_to_write * bytes_per_frame;

            // We stay rb_bytes - internal_delay_bytes ahead of where the driver is reading from.
            // If the difference in elapsed frames and what we expect to write is greater than
            // that distance, we've woken up too late and not all data will be read by driver.
            if (rb_frames_elapsed_since_last_wakeup.floor() as i64
                - new_frames_available_to_write as i64)
                .abs() as u64
                > bytes_in_rb - consumer_bytes
            // Calculate time taken to write as well?
            {
                println!(
                    "Woke up {} ns late",
                    duration_since_last_wakeup.into_nanos() as f64 - nanos_per_wakeup_interval
                );
                late_wakeups += 1;
            }

            let mut buf = vec![silence_value as u8; num_bytes_to_write as usize];

            let bytes_read_from_socket =
                Self::read_socket_into_buf(&mut data_socket, &mut buf).await?;

            if bytes_read_from_socket == 0 {
                silenced_frames += new_frames_available_to_write;
            }

            if bytes_read_from_socket < num_bytes_to_write {
                let partial_silence_bytes =
                    new_frames_available_to_write * bytes_per_frame - bytes_read_from_socket as u64;
                silenced_frames += partial_silence_bytes / bytes_per_frame;
            }

            ring_buffer.write_to_frame(last_frame_written, &mut buf)?;
            last_frame_written += new_frames_available_to_write;

            // We want entire ring buffer to be silenced.
            if silenced_frames * bytes_per_frame as u64 >= bytes_in_rb {
                break;
            }
        }

        ring_buffer_client.stop().await?;

        let mut async_stdout =
            fasync::Socket::from_socket(stdout_local).expect("Async socket create failed.");

        let output_message = format!(
            "Succesfully processed all audio data. \n Woke up late {} times.\n ",
            late_wakeups
        );
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
                    async fn get_entries(path: &str) -> Result<Vec<String>, Error> {
                        let (control_client, control_server) = zx::Channel::create();

                        // Creates a connection to a FIDL service at path.
                        fdio::service_connect(path, control_server)
                            .context(format!("failed to connect to {:?}", path))?;

                        let directory_proxy = fio::DirectoryProxy::from_channel(
                            fasync::Channel::from_channel(control_client).unwrap(),
                        );

                        let (status, mut buf) = directory_proxy
                            .read_dirents(fio::MAX_BUF)
                            .await
                            .expect("Failure calling read dirents");

                        if status != 0 {
                            return Err(anyhow::anyhow!(
                                "Unable to call read dirents, status returned: {}",
                                status
                            ));
                        }

                        let entry_names = fuchsia_fs::directory::parse_dir_entries(&mut buf);
                        let full_paths: Vec<String> = entry_names
                            .into_iter()
                            .filter_map(|s| Some(path.to_owned() + &s.ok().unwrap().name))
                            .collect();

                        Ok(full_paths)
                    }

                    let mut input_entries = get_entries("/dev/class/audio-input/").await?;
                    let mut output_entries = get_entries("/dev/class/audio-output/").await?;
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

                    // Connect to a StreamConfig channel.
                    let (connector_client, connector_server) = fidl::endpoints::create_proxy::<
                        fidl_fuchsia_hardware_audio::StreamConfigConnectorMarker,
                    >()
                    .expect("failed to create streamconfig");

                    let (stream_config_client, stream_config_connector) =
                        fidl::endpoints::create_proxy::<
                            fidl_fuchsia_hardware_audio::StreamConfigMarker,
                        >()
                        .expect("failed to create streamconfig ");

                    let is_input_device = device_selector
                        .is_input
                        .ok_or(anyhow::anyhow!("Input/output not specified"))?;

                    // Connect to either /dev/class/audio-output/{id} or /dev/class/audio-input/{id}
                    // depending on args.
                    let id = device_selector.id.ok_or(anyhow::anyhow!("No device ID provided"))?;

                    let device_path = if is_input_device {
                        format!("/dev/class/audio-input/{}", id)
                    } else {
                        format!("/dev/class/audio-output/{}", id)
                    };

                    // Creates a connection to a FIDL service at "/dev/class/audio-output/" or
                    // "/dev/class/audio-input/", passing the server end of StreamConfigConnector
                    // channel.
                    fdio::service_connect(&device_path, connector_server.into_channel())
                        .context(format!("failed to connect to {}", &device_path))?;

                    // Using StreamConfigConnector client, pass the server end of StreamConfig
                    // channel to the device so that device can respond to StreamConfig requests.
                    connector_client.connect(stream_config_connector)?;

                    let stream_properties = stream_config_client.get_properties().await?;
                    let supported_formats = stream_config_client.get_supported_formats().await?;
                    let gain_state = stream_config_client.watch_gain_state().await?;
                    let plug_state = stream_config_client.watch_plug_state().await?;

                    let response = AudioDaemonDeviceInfoResponse {
                        device_info: Some(DeviceInfo {
                            stream_properties: Some(stream_properties),
                            supported_formats: Some(supported_formats),
                            gain_state: Some(gain_state),
                            plug_state: Some(plug_state),
                            ..DeviceInfo::EMPTY
                        }),
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
            // match on `request` and handle each protocol.
            let mut audio_daemon = AudioDaemon::new();

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
