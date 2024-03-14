// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    error::ControllerError,
    ring_buffer::{HardwareRingBuffer, RingBuffer},
    socket, stop_listener, SECONDS_PER_NANOSECOND,
};
use anyhow::{anyhow, Context, Error};
use async_trait::async_trait;
use camino::Utf8PathBuf;
use fidl::endpoints::{create_proxy, Proxy, ServerEnd};
use fidl_fuchsia_audio_controller as fac;
use fidl_fuchsia_hardware_audio as fhaudio;
use fidl_fuchsia_io as fio;
use format_utils::Format;
use fuchsia_async as fasync;
use fuchsia_component::client::connect_to_protocol_at_path;
use fuchsia_zircon::{self as zx};
use futures::{AsyncWriteExt, StreamExt};

// TODO(b/317991807) Remove #[async_trait] when supported by compiler.
#[async_trait]
pub trait DeviceControl: Send + Sync {
    async fn get_properties(&mut self) -> Result<Properties, Error>;
    async fn get_supported_formats(&mut self) -> Result<Formats, Error>;
    async fn watch_gain_state(&mut self) -> Result<fhaudio::GainState, Error>;
    async fn watch_plug_state(&mut self) -> Result<fhaudio::PlugState, Error>;
    async fn create_ring_buffer(&mut self, format: Format) -> Result<Box<dyn RingBuffer>, Error>;
    fn set_gain(&mut self, gain_state: fhaudio::GainState) -> Result<(), Error>;
}

pub enum Properties {
    StreamConfig(fhaudio::StreamProperties),
    Composite(fhaudio::CompositeProperties),
}

pub enum Formats {
    StreamConfig(Vec<fhaudio::SupportedFormats>),
    Composite { dai: Vec<fhaudio::DaiSupportedFormats>, stream: Vec<fhaudio::SupportedFormats> },
}

pub struct StreamConfigDevice {
    pub proxy: fhaudio::StreamConfigProxy,
}

#[async_trait]
impl DeviceControl for StreamConfigDevice {
    async fn get_properties(&mut self) -> Result<Properties, Error> {
        let response = self
            .proxy
            .get_properties()
            .await
            .map_err(|e| anyhow!("Failed to get StreamConfig properties: {e}"))?;

        Ok(Properties::StreamConfig(response))
    }

    async fn get_supported_formats(&mut self) -> Result<Formats, Error> {
        let response = self
            .proxy
            .get_supported_formats()
            .await
            .map_err(|e| anyhow!("Could not query streamconfig supported formats: {e}"))?;

        Ok(Formats::StreamConfig(response))
    }

    async fn watch_gain_state(&mut self) -> Result<fhaudio::GainState, Error> {
        let response = self
            .proxy
            .watch_gain_state()
            .await
            .map_err(|e| anyhow!("Could not query streamconfig gain state: {e}"))?;

        Ok(response)
    }

    async fn watch_plug_state(&mut self) -> Result<fhaudio::PlugState, Error> {
        let response = self
            .proxy
            .watch_plug_state()
            .await
            .map_err(|e| anyhow!("Could not query streamconfig plug state: {e}"))?;

        Ok(response)
    }

    async fn create_ring_buffer(&mut self, format: Format) -> Result<Box<dyn RingBuffer>, Error> {
        let (ring_buffer_proxy, ring_buffer_server) =
            create_proxy::<fhaudio::RingBufferMarker>().unwrap();
        self.proxy.create_ring_buffer(&fhaudio::Format::from(&format), ring_buffer_server)?;
        let ring_buffer = HardwareRingBuffer::new(ring_buffer_proxy, format).await?;

        Ok(Box::new(ring_buffer))
    }

    fn set_gain(&mut self, gain_state: fhaudio::GainState) -> Result<(), Error> {
        self.proxy.set_gain(&gain_state).map_err(|e| anyhow!("Error setting gain state: {e}"))
    }
}

pub struct CompositeDevice {
    pub proxy: fhaudio::CompositeProxy,
}

#[async_trait]
impl DeviceControl for CompositeDevice {
    async fn get_properties(&mut self) -> Result<Properties, Error> {
        let response = self
            .proxy
            .get_properties()
            .await
            .map_err(|e| anyhow!("Failed to get Composite properties: {e}"))?;

        Ok(Properties::Composite(response))
    }

    async fn get_supported_formats(&mut self) -> Result<Formats, Error> {
        let _formats = Formats::Composite { dai: vec![], stream: vec![] };
        Err(anyhow!("Supported formats for Composite devices not yet supported yet in ffx audio."))
    }

    async fn watch_gain_state(&mut self) -> Result<fhaudio::GainState, Error> {
        Err(anyhow!("watch gain state for Composite devices not supported yet in ffx audio."))
    }

    async fn watch_plug_state(&mut self) -> Result<fhaudio::PlugState, Error> {
        Err(anyhow!("watch plug state for Composite devices not supported yet in ffx audio."))
    }

    async fn create_ring_buffer(&mut self, _format: Format) -> Result<Box<dyn RingBuffer>, Error> {
        Err(anyhow!("Creating ring buffers for Composite devices not supported yet in ffx audio."))
    }

    fn set_gain(&mut self, _gain_state: fhaudio::GainState) -> Result<(), Error> {
        Err(anyhow!(
            "Setting gain not supported for Composite devices not supported yet in ffx audio."
        ))
    }
}

fn validate_format(
    requested_format: &format_utils::Format,
    supported_formats: Formats,
) -> Result<(), ControllerError> {
    match supported_formats {
        Formats::Composite { stream: _stream, dai: _dai } => {
            // TODO(b/310275209) Implement record for composite device type.
            return Err(ControllerError::new(
                fac::Error::NotSupported,
                format!("Playing to composite drivers not yet supported."),
            ));
        }
        Formats::StreamConfig(stream_config_formats) => {
            if !requested_format.is_supported_by(&stream_config_formats) {
                return Err(ControllerError::new(
                    fac::Error::InvalidArguments,
                    format!("Requested format not supported."),
                ));
            }
            Ok(())
        }
    }
}

/// Connects to the device protocol for the device of the provided type at the namespace `path`.
pub fn connect_to_device_controller(
    device_path: Utf8PathBuf,
    device_type: fhaudio::DeviceType,
) -> Result<Box<dyn DeviceControl>, Error> {
    let protocol_path = device_path.join("device_protocol");

    match device_type {
        fhaudio::DeviceType::StreamConfig => {
            let connector_proxy =
                connect_to_protocol_at_path::<fhaudio::StreamConfigConnectorMarker>(
                    protocol_path.as_str(),
                )
                .context("Failed to connect to StreamConfigConnector")?;
            let (proxy, server_end) = create_proxy()?;
            connector_proxy.connect(server_end).context("Failed to call Connect")?;

            Ok(Box::new(StreamConfigDevice { proxy }))
        }
        fhaudio::DeviceType::Composite => {
            // DFv2 Composite drivers do not use a connector/trampoline as StreamConfig above.
            // TODO(https://fxbug.dev/326339971): Fall back to CompositeConnector for DFv1 drivers
            let proxy =
                connect_to_protocol_at_path::<fhaudio::CompositeMarker>(protocol_path.as_str())
                    .context("Failed to connect to Composite")?;

            Ok(Box::new(CompositeDevice { proxy }))
        }
        _ => Err(anyhow!("Unsupported DeviceType for connect_to_device_controller()")),
    }
}

pub struct Device {
    pub device_controller: Box<dyn DeviceControl>,
}

impl Device {
    pub fn new_from_selector(selector: &fac::DeviceSelector) -> Result<Self, Error> {
        let device_controller = connect_to_device_controller(
            format_utils::path_for_selector(&selector)?,
            selector.device_type.ok_or(anyhow!("Device type not specified"))?,
        )?;
        Ok(Self { device_controller })
    }

    pub async fn get_info(&mut self) -> Result<fac::DeviceInfo, Error> {
        let properties = self.device_controller.get_properties().await?;

        match properties {
            Properties::StreamConfig(stream_properties) => {
                let supported_formats =
                    match self.device_controller.get_supported_formats().await.ok() {
                        Some(Formats::StreamConfig(supported_formats)) => Some(supported_formats),
                        _ => None,
                    };

                let gain_state = self.device_controller.watch_gain_state().await.ok();
                let plug_state = self.device_controller.watch_plug_state().await.ok();
                Ok(fac::DeviceInfo::StreamConfig(fac::StreamConfigDeviceInfo {
                    stream_properties: Some(stream_properties),
                    supported_formats,
                    gain_state,
                    plug_state,
                    ..Default::default()
                }))
            }
            Properties::Composite(composite_properties) => {
                let (supported_dai_formats, supported_ring_buffer_formats) =
                    match self.device_controller.get_supported_formats().await.ok() {
                        Some(Formats::Composite { dai, stream }) => (Some(dai), Some(stream)),
                        _ => (None, None),
                    };

                Ok(fac::DeviceInfo::Composite(fac::CompositeDeviceInfo {
                    composite_properties: Some(composite_properties),
                    supported_dai_formats,
                    supported_ring_buffer_formats,
                    ..Default::default()
                }))
            }
        }
    }

    pub fn set_gain(&mut self, gain_state: fhaudio::GainState) -> Result<(), Error> {
        self.device_controller
            .set_gain(gain_state)
            .map_err(|e| anyhow!("Error setting device gain state: {e}"))
    }

    pub async fn play(
        &mut self,
        mut data_socket: fasync::Socket,
    ) -> Result<fac::PlayerPlayResponse, Error> {
        let mut socket = socket::Socket { socket: &mut data_socket };
        let spec = socket.read_wav_header().await?;
        let format = format_utils::Format::from(&spec);

        let supported_formats = self.device_controller.get_supported_formats().await?;
        validate_format(&format, supported_formats)?;

        let ring_buffer = self.device_controller.create_ring_buffer(format).await?;

        let mut silenced_frames = 0u64;
        let mut late_wakeups = 0;
        let mut last_frame_written = 0u64;

        let nanos_per_wakeup_interval = 10e6f64; // 10 milliseconds
        let wakeup_interval = zx::Duration::from_millis(10);

        let frames_per_nanosecond = format.frames_per_second as f64 * SECONDS_PER_NANOSECOND;

        let bytes_in_rb = ring_buffer.vmo_buffer().data_size_bytes();
        let consumer_bytes = ring_buffer.consumer_bytes();
        let bytes_per_wakeup_interval =
            (nanos_per_wakeup_interval * frames_per_nanosecond * format.bytes_per_frame() as f64)
                .floor() as u64;

        if consumer_bytes + bytes_per_wakeup_interval > bytes_in_rb {
            return Err(anyhow!(
                "Ring buffer not large enough for internal delay. Ring buffer bytes: {}, 
            consumer + producer bytes: {}",
                bytes_in_rb,
                consumer_bytes + bytes_per_wakeup_interval
            ));
        }

        let t_zero = ring_buffer.start().await?;

        // To start, wait until at least t0 + (wakeup_interval) so we can start writing at
        // the first bytes in the ring buffer.
        fuchsia_async::Timer::new(t_zero).await;

        let mut last_wakeup = t_zero;

        /*
            - Sleep for time equivalent to a small portion of ring buffer
            - On wake up, write from last byte written until point where driver has just
              finished reading.
                If the driver read region has wrapped around the ring buffer, split the
                write into two steps:
                    1. Write to the end of the ring buffer.
                    2. Write the remaining bytes from beginning of ring buffer.


            Repeat above steps until the socket has no more data to read. Then continue
            writing the silence value until all bytes in the ring buffer have been written
            back to silence.

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

        let mut timer = fuchsia_async::Interval::new(wakeup_interval);

        loop {
            timer.next().await;

            // Check that we woke up on time. Approximate ring buffer pointer position based on
            // the current time and the expected rate of how fast it moves.
            // Ring buffer pointer should be ahead of last byte written.
            let now = zx::Time::get_monotonic();

            let duration_since_last_wakeup = now - last_wakeup;
            last_wakeup = now;

            let total_time_elapsed = now - t_zero;
            let total_rb_frames_elapsed =
                frames_per_nanosecond * total_time_elapsed.into_nanos() as f64;

            let rb_frames_elapsed_since_last_wakeup =
                frames_per_nanosecond * duration_since_last_wakeup.into_nanos() as f64;

            let new_frames_available_to_write =
                total_rb_frames_elapsed.floor() as u64 - last_frame_written;
            let num_bytes_to_write =
                new_frames_available_to_write * format.bytes_per_frame() as u64;

            // In a given wakeup period, the "unsafe bytes" we avoid writing to
            // are the range of bytes that the driver will read from during that period,
            // since the written data wouldn't update in time.
            // There are (consumer_bytes + bytes_per_wakeup_interval) unsafe bytes since writes
            // can take up to one period in the worst case. The remaining bytes in the ring buffer
            // are safe to write to since the driver will read the updated data.
            // If the difference in elapsed frames and what we expect to write is
            // greater than the range of safe bytes, we've woken up too late and
            // some of the audio data will not be read by the driver.

            if ((rb_frames_elapsed_since_last_wakeup.floor() as i64
                - new_frames_available_to_write as i64)
                * format.bytes_per_frame() as i64)
                .abs() as u64
                > bytes_in_rb - (consumer_bytes + bytes_per_wakeup_interval)
            {
                println!(
                    "Woke up {} ns late",
                    duration_since_last_wakeup.into_nanos() as f64 - nanos_per_wakeup_interval
                );
                late_wakeups += 1;
            }

            let mut buf = vec![format.silence_value(); num_bytes_to_write as usize];

            let bytes_read_from_socket = socket.read_until_full(&mut buf).await?;

            if bytes_read_from_socket == 0 {
                silenced_frames += new_frames_available_to_write;
            }

            if bytes_read_from_socket < num_bytes_to_write {
                let partial_silence_bytes = new_frames_available_to_write
                    * format.bytes_per_frame() as u64
                    - bytes_read_from_socket as u64;
                silenced_frames += partial_silence_bytes / format.bytes_per_frame() as u64;
            }

            ring_buffer.vmo_buffer().write_to_frame(last_frame_written, &buf)?;
            last_frame_written += new_frames_available_to_write;

            // We want entire ring buffer to be silenced.
            if silenced_frames * format.bytes_per_frame() as u64 >= bytes_in_rb {
                break;
            }
        }

        ring_buffer.stop().await?;

        println!(
            "Successfully processed all audio data. \n Woke up late {} times.\n ",
            late_wakeups
        );

        Ok(fac::PlayerPlayResponse { bytes_processed: None, ..Default::default() })
    }

    pub async fn record(
        &mut self,
        format: format_utils::Format,
        mut data_socket: fasync::Socket,
        duration: Option<std::time::Duration>,
        cancel_server: Option<ServerEnd<fac::RecordCancelerMarker>>,
    ) -> Result<fac::RecorderRecordResponse, ControllerError> {
        let mut socket = socket::Socket { socket: &mut data_socket };

        let supported_formats = self.device_controller.get_supported_formats().await?;
        validate_format(&format, supported_formats)?;

        let ring_buffer = self.device_controller.create_ring_buffer(format).await?;

        // Hardware might not use all bytes in vmo. Only want to read frames hardware will write to.
        let bytes_in_rb = ring_buffer.vmo_buffer().data_size_bytes();
        let producer_bytes = ring_buffer.producer_bytes();
        let wakeup_interval = zx::Duration::from_millis(10);
        let frames_per_nanosecond = format.frames_per_second as f64 * SECONDS_PER_NANOSECOND;

        let bytes_per_wakeup_interval = (wakeup_interval.into_nanos() as f64
            * frames_per_nanosecond
            * format.bytes_per_frame() as f64)
            .floor() as u64;

        if producer_bytes + bytes_per_wakeup_interval > bytes_in_rb {
            return Err(ControllerError::new(
                fac::Error::UnknownFatal,
                format!("Ring buffer not large enough for driver internal delay and plugin wakeup interval.
                 Ring buffer bytes: {}, bytes_per_wakeup_interval + producer bytes: {}",
                bytes_in_rb,
                bytes_per_wakeup_interval + producer_bytes
            )));
        }

        let safe_bytes_in_rb = bytes_in_rb - producer_bytes;

        let mut late_wakeups = 0;

        // Running counter representing the next time we'll wake up and read from ring buffer.
        // To start, sleep until at least t0 + (wakeup_interval) so we can start reading from
        // the first bytes in the ring buffer.

        let t_zero = ring_buffer.start().await?;
        fuchsia_async::Timer::new(t_zero).await;

        let mut last_wakeup = t_zero;
        let mut last_frame_read = 0u64;

        let mut timer = fuchsia_async::Interval::new(wakeup_interval);
        let mut buf = vec![format.silence_value(); bytes_per_wakeup_interval as usize];
        let stop_signal = std::sync::atomic::AtomicBool::new(false);

        socket.write_wav_header(duration, &format).await?;
        let packet_fut = async {
            loop {
                timer.next().await;
                // Check that we woke up on time. Approximate ring buffer pointer position based on
                // the current time and the expected rate of how fast it moves.
                // Ring buffer pointer should be ahead of last byte read.
                let now = zx::Time::get_monotonic();

                if stop_signal.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }

                let elapsed_since_last_wakeup = now - last_wakeup;
                let elapsed_since_start = now - t_zero;

                let elapsed_frames_since_start = (frames_per_nanosecond
                    * elapsed_since_start.into_nanos() as f64)
                    .floor() as u64;

                let available_frames_to_read = elapsed_frames_since_start - last_frame_read;
                let bytes_to_read = available_frames_to_read * format.bytes_per_frame() as u64;
                if buf.len() < bytes_to_read as usize {
                    buf.resize(bytes_to_read as usize, 0);
                }

                // Check for late wakeup to know whether we have missed reading some audio signal.
                // In a given wakeup period, the "unsafe bytes" we avoid reading from
                // are the range of bytes that the driver will write to during that period,
                // since we'd be reading stale data.
                // There are (producer_bytes + bytes_per_wakeup_interval) unsafe bytes since reads
                // can take up to one period in the worst case. The remaining bytes in the ring
                // buffer are safe to read from since the data will be up to date.
                // The amount of bytes we can read from is the difference between the last position
                // we read from and the current elapsed position. If that amount is greater than
                // the amount of safe bytes, we've woken up too late and will miss some of the
                // audio signal. In that case, we span the missed bytes with silence value.

                let bytes_missed = if bytes_to_read > safe_bytes_in_rb {
                    (bytes_to_read - safe_bytes_in_rb) as usize
                } else {
                    0usize
                };
                if bytes_missed > 0 {
                    println!(
                        "Woke up {} ns late",
                        elapsed_since_last_wakeup.into_nanos() - wakeup_interval.into_nanos()
                    );
                    late_wakeups += 1;
                }

                buf[..bytes_missed].fill(format.silence_value());
                let _ = ring_buffer
                    .vmo_buffer()
                    .read_from_frame(last_frame_read, &mut buf[bytes_missed..])?;

                last_frame_read += available_frames_to_read;

                let write_full_buffer = duration.is_none()
                    || (format.frames_in_duration(duration.unwrap_or_default()) - last_frame_read
                        > available_frames_to_read);

                if write_full_buffer {
                    data_socket.write_all(&buf).await?;
                    last_wakeup = now;
                } else {
                    let bytes_to_write = (format.frames_in_duration(duration.unwrap_or_default())
                        - last_frame_read) as usize
                        * format.bytes_per_frame() as usize;
                    data_socket.write_all(&buf[..bytes_to_write]).await?;
                    break;
                }
            }
            Ok(fac::RecorderRecordResponse {
                bytes_processed: None,
                packets_processed: None,
                late_wakeups: Some(late_wakeups),
                ..Default::default()
            })
        };

        let result = if let Some(cancel_server) = cancel_server {
            let (_cancel_res, packet_res) =
                futures::future::try_join(stop_listener(cancel_server, &stop_signal), packet_fut)
                    .await?;

            Ok(packet_res)
        } else {
            packet_fut.await
        };
        result.map_err(|e| ControllerError::new(fac::Error::UnknownCanRetry, format!("{e}")))
    }
}

// Helper function to enumerate on device directories to get information about available drivers.
pub async fn get_entries(
    path: &str,
    device_type: fhaudio::DeviceType,
    is_input: Option<bool>,
) -> Result<Vec<fac::DeviceSelector>, Error> {
    let (control_client, control_server) = zx::Channel::create();

    // Creates a connection to a FIDL service at path.
    fdio::service_connect(path, control_server)
        .context(format!("failed to connect to {:?}", path))?;

    let directory_proxy =
        fio::DirectoryProxy::from_channel(fasync::Channel::from_channel(control_client));

    let (status, buf) = directory_proxy
        .read_dirents(fio::MAX_BUF)
        .await
        .map_err(|e| anyhow!("Failure calling read dirents: {}", e))?;

    if status != 0 {
        return Err(anyhow!("Unable to call read dirents, status returned: {}", status));
    }

    let entry_names = fuchsia_fs::directory::parse_dir_entries(&buf);

    let full_paths = entry_names.into_iter().filter_map(|s| match s {
        Ok(entry) => match entry.kind {
            fio::DirentType::Directory => {
                if entry.name.len() > 1 {
                    Some(path.to_owned() + entry.name.as_str())
                } else {
                    None
                }
            }
            _ => None,
        },
        Err(_) => None,
    });

    let device_selectors = full_paths
        .map(|path| fac::DeviceSelector {
            is_input: is_input,
            id: format_utils::device_id_for_path(std::path::Path::new(&path)).ok(),
            device_type: Some(device_type),
            ..Default::default()
        })
        .collect();

    Ok(device_selectors)
}
