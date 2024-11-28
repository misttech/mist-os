// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::error::ControllerError;
use crate::ring_buffer::{AudioDeviceRingBuffer, HardwareRingBuffer, RingBuffer};
use crate::wav_socket::WavSocket;
use anyhow::{anyhow, Context, Error};
use async_trait::async_trait;
use fidl::endpoints::{create_proxy, ServerEnd};
use fuchsia_audio::device::{DevfsSelector, RegistrySelector, Selector};
use fuchsia_audio::{stop_listener, Format};
use fuchsia_component::client::{connect_to_protocol, connect_to_protocol_at_path};

use futures::{AsyncWriteExt, StreamExt};
use std::collections::{btree_map, BTreeMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use {
    fidl_fuchsia_audio_controller as fac, fidl_fuchsia_audio_device as fadevice,
    fidl_fuchsia_hardware_audio as fhaudio,
};

const NANOSECONDS_PER_SECOND: u64 = 10_u64.pow(9);
// TODO(https://fxbug.dev/332322792): Replace with an integer conversion.
const SECONDS_PER_NANOSECOND: f64 = 1.0 / NANOSECONDS_PER_SECOND as f64;

// TODO(https://fxbug.dev/317991807): Remove #[async_trait] when supported by compiler.
#[async_trait]
pub trait DeviceControl {
    async fn create_ring_buffer(
        &mut self,
        element_id: fadevice::ElementId,
        format: Format,
    ) -> Result<Box<dyn RingBuffer>, Error>;
    fn set_gain(&mut self, gain_state: fhaudio::GainState) -> Result<(), Error>;
}

pub struct StreamConfigDevice {
    proxy: fhaudio::StreamConfigProxy,
}

#[async_trait]
impl DeviceControl for StreamConfigDevice {
    async fn create_ring_buffer(
        &mut self,
        _element_id: fadevice::ElementId,
        format: Format,
    ) -> Result<Box<dyn RingBuffer>, Error> {
        let (ring_buffer_proxy, ring_buffer_server) = create_proxy::<fhaudio::RingBufferMarker>();
        self.proxy.create_ring_buffer(&fhaudio::Format::from(format), ring_buffer_server)?;
        let ring_buffer = HardwareRingBuffer::new(ring_buffer_proxy, format).await?;

        Ok(Box::new(ring_buffer))
    }

    fn set_gain(&mut self, gain_state: fhaudio::GainState) -> Result<(), Error> {
        self.proxy.set_gain(&gain_state).map_err(|e| anyhow!("Error setting gain state: {e}"))
    }
}

pub struct CompositeDevice {
    pub _proxy: fhaudio::CompositeProxy,
}

#[async_trait]
impl DeviceControl for CompositeDevice {
    async fn create_ring_buffer(
        &mut self,
        _element_id: fadevice::ElementId,
        _format: Format,
    ) -> Result<Box<dyn RingBuffer>, Error> {
        Err(anyhow!("Creating ring buffers for Composite devices not supported yet in ffx audio."))
    }

    fn set_gain(&mut self, _gain_state: fhaudio::GainState) -> Result<(), Error> {
        Err(anyhow!(
            "Setting gain not supported for Composite devices not supported yet in ffx audio."
        ))
    }
}

pub struct RegistryDevice {
    proxy: fadevice::ControlProxy,
}

impl RegistryDevice {
    fn new(proxy: fadevice::ControlProxy) -> Self {
        Self { proxy }
    }
}

#[async_trait]
impl DeviceControl for RegistryDevice {
    async fn create_ring_buffer(
        &mut self,
        element_id: fadevice::ElementId,
        format: Format,
    ) -> Result<Box<dyn RingBuffer>, Error> {
        let (ring_buffer_proxy, ring_buffer_server) = create_proxy::<fadevice::RingBufferMarker>();

        // Request at least 100ms worth of frames (rounding up).
        let min_frames = format.frames_per_second.div_ceil(10);
        let min_bytes = min_frames * format.bytes_per_frame();

        let options = fadevice::RingBufferOptions {
            format: Some(format.into()),
            ring_buffer_min_bytes: Some(min_bytes),
            ..Default::default()
        };

        let response = self
            .proxy
            .create_ring_buffer(fadevice::ControlCreateRingBufferRequest {
                element_id: Some(element_id),
                options: Some(options),
                ring_buffer_server: Some(ring_buffer_server),
                ..Default::default()
            })
            .await
            .context("Failed to call CreateRingBuffer")?
            .map_err(|err| anyhow!("Failed to create ring buffer: {err:?}"))?;
        let properties = response.properties.ok_or_else(|| anyhow!("missing 'properties'"))?;
        let ring_buffer = response.ring_buffer.ok_or_else(|| anyhow!("missing 'ring_buffer'"))?;

        let ring_buffer =
            AudioDeviceRingBuffer::new(ring_buffer_proxy, ring_buffer, properties, format).await?;

        Ok(Box::new(ring_buffer))
    }

    fn set_gain(&mut self, _gain_state: fhaudio::GainState) -> Result<(), Error> {
        Err(anyhow!("set gain is not supported for Registry devices"))
    }
}

/// Connects to the device protocol for a device in devfs.
fn connect_to_devfs_device(devfs: DevfsSelector) -> Result<Box<dyn DeviceControl>, Error> {
    let protocol_path = devfs.path().join("device_protocol");

    match devfs.0.device_type {
        fadevice::DeviceType::Input | fadevice::DeviceType::Output => {
            let connector_proxy =
                connect_to_protocol_at_path::<fhaudio::StreamConfigConnectorMarker>(
                    protocol_path.as_str(),
                )
                .context("Failed to connect to StreamConfigConnector")?;
            let (proxy, server_end) = create_proxy();
            connector_proxy.connect(server_end).context("Failed to call Connect")?;

            Ok(Box::new(StreamConfigDevice { proxy }))
        }
        fadevice::DeviceType::Composite => {
            // DFv2 Composite drivers do not use a connector/trampoline as StreamConfig above.
            // TODO(https://fxbug.dev/326339971): Fall back to CompositeConnector for DFv1 drivers
            let proxy =
                connect_to_protocol_at_path::<fhaudio::CompositeMarker>(protocol_path.as_str())
                    .context("Failed to connect to Composite")?;

            Ok(Box::new(CompositeDevice { _proxy: proxy }))
        }
        _ => Err(anyhow!("Unsupported DeviceType for connect_to_device_controller()")),
    }
}

async fn connect_to_registry_device(
    selector: RegistrySelector,
) -> Result<Box<dyn DeviceControl>, Error> {
    let control_creator = connect_to_protocol::<fadevice::ControlCreatorMarker>()
        .context("Failed to connect to ControlCreator")?;

    let (proxy, server_end) = create_proxy::<fadevice::ControlMarker>();

    control_creator
        .create(fadevice::ControlCreatorCreateRequest {
            token_id: Some(selector.token_id()),
            control_server: Some(server_end),
            ..Default::default()
        })
        .await
        .context("failed to call ControlCreator.Create")?
        .map_err(|err| anyhow!("failed to create Control: {:?}", err))?;

    Ok(Box::new(RegistryDevice::new(proxy)))
}

async fn connect_device_control(
    selector: Selector,
) -> Result<Box<dyn DeviceControl>, ControllerError> {
    match selector {
        Selector::Devfs(selector) => connect_to_devfs_device(selector),
        Selector::Registry(selector) => connect_to_registry_device(selector).await,
    }
    .map_err(|err| {
        ControllerError::new(
            fac::Error::DeviceNotReachable,
            format!("Failed to connect to device with error: {err}"),
        )
    })
}

pub struct DeviceControlConnector {
    device_controls: BTreeMap<Selector, Weak<Mutex<Box<dyn DeviceControl>>>>,
}

impl DeviceControlConnector {
    pub fn new() -> Self {
        Self { device_controls: BTreeMap::new() }
    }

    /// Returns a [DeviceControl] connection to the device identified by `selector`.
    ///
    /// If the connection already exists from a previous call to `connect`, returns
    /// the existing shared [DeviceControl]. Otherwise, creates a new connection,
    /// stores it for subsequent calls, and return it.
    pub async fn connect(
        &mut self,
        selector: Selector,
    ) -> Result<Arc<Mutex<Box<dyn DeviceControl>>>, ControllerError> {
        match self.device_controls.entry(selector) {
            btree_map::Entry::Vacant(entry) => {
                let selector = entry.key().clone();
                let device_control = Arc::new(Mutex::new(connect_device_control(selector).await?));
                let _ = entry.insert(Arc::downgrade(&device_control));
                Ok(device_control)
            }
            btree_map::Entry::Occupied(mut entry) => {
                match entry.get().upgrade() {
                    Some(device_control) => Ok(device_control),
                    None => {
                        let selector = entry.key().clone();
                        let device_control =
                            Arc::new(Mutex::new(connect_device_control(selector).await?));
                        // Replace with a live connection.
                        let _ = entry.insert(Arc::downgrade(&device_control));
                        Ok(device_control)
                    }
                }
            }
        }
    }
}

pub struct Device {
    pub device_controller: Arc<Mutex<Box<dyn DeviceControl>>>,
}

impl Device {
    pub fn new(device_controller: Arc<Mutex<Box<dyn DeviceControl>>>) -> Self {
        Self { device_controller }
    }

    pub fn set_gain(&mut self, gain_state: fhaudio::GainState) -> Result<(), Error> {
        self.device_controller
            .lock()
            .unwrap()
            .set_gain(gain_state)
            .map_err(|e| anyhow!("Error setting device gain state: {e}"))
    }

    pub async fn play(
        &mut self,
        element_id: fadevice::ElementId,
        mut socket: WavSocket,
        active_channels_bitmask: Option<u64>,
    ) -> Result<fac::PlayerPlayResponse, ControllerError> {
        let spec = socket.read_header().await?;
        let format = Format::from(spec);

        let ring_buffer =
            self.device_controller.lock().unwrap().create_ring_buffer(element_id, format).await?;

        let mut silenced_frames = 0u64;
        let mut late_wakeups = 0;
        let mut next_frame_to_write = 0u64; // The number of frames written to the ring buffer.

        let nanos_per_wakeup_interval = 10e6f64; // 10 milliseconds
        let wakeup_interval = zx::MonotonicDuration::from_millis(10);

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
            )
            .into());
        }

        let mut active_channels_set_time = None;
        if let Some(active_channels_bitmask) = active_channels_bitmask {
            active_channels_set_time =
                Some(ring_buffer.set_active_channels(active_channels_bitmask).await?);
        }

        let start_time = ring_buffer.start().await?;

        let t_zero = zx::MonotonicInstant::from_nanos(std::cmp::max(
            active_channels_set_time.unwrap_or(zx::MonotonicInstant::from_nanos(0)).into_nanos(),
            start_time.into_nanos(),
        ));

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
            let now = zx::MonotonicInstant::get();

            let duration_since_last_wakeup = now - last_wakeup;
            last_wakeup = now;

            let total_time_elapsed = now - t_zero;
            let total_rb_frames_elapsed =
                frames_per_nanosecond * total_time_elapsed.into_nanos() as f64;

            let rb_frames_elapsed_since_last_wakeup =
                frames_per_nanosecond * duration_since_last_wakeup.into_nanos() as f64;

            let new_frames_available_to_write =
                total_rb_frames_elapsed.floor() as u64 - next_frame_to_write;
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

            // We copy from socket to this intermediate buffer, and from there to ring buffer.
            let mut buf = vec![format.sample_type.silence_value(); num_bytes_to_write as usize];

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

            ring_buffer
                .vmo_buffer()
                .write_to_frame(next_frame_to_write, &buf)
                .context("Failed to write to buffer")?;
            next_frame_to_write += new_frames_available_to_write;

            // We want entire ring buffer to be silenced.
            if silenced_frames * format.bytes_per_frame() as u64 >= bytes_in_rb {
                break;
            }
        }

        ring_buffer.stop().await?;

        println!("Successfully played all audio data. Woke up late {} times.", late_wakeups);

        Ok(fac::PlayerPlayResponse { bytes_processed: None, ..Default::default() })
    }

    pub async fn record(
        &mut self,
        element_id: fadevice::ElementId,
        format: Format,
        mut socket: WavSocket,
        duration: Option<Duration>,
        cancel_server: Option<ServerEnd<fac::RecordCancelerMarker>>,
    ) -> Result<fac::RecorderRecordResponse, ControllerError> {
        let ring_buffer =
            self.device_controller.lock().unwrap().create_ring_buffer(element_id, format).await?;

        // Hardware might not use all bytes in vmo. Only want to read frames hardware will write to.
        let bytes_in_rb = ring_buffer.vmo_buffer().data_size_bytes();
        let producer_bytes = ring_buffer.producer_bytes();
        let wakeup_interval = zx::MonotonicDuration::from_millis(10);

        // We multiply (with u64) before dividing to preserve precision. We intentionally round-up
        // the result, so this byte amount is worst-case.
        let bytes_per_wakeup_interval = (wakeup_interval.into_nanos() as u64
            * format.frames_per_second as u64
            * format.bytes_per_frame() as u64)
            .div_ceil(NANOSECONDS_PER_SECOND);

        if producer_bytes + bytes_per_wakeup_interval > bytes_in_rb {
            return Err(ControllerError::new(
                fac::Error::UnknownFatal,
                format!("Ring buffer not large enough for driver internal delay and plugin wakeup interval. Ring buffer bytes: {}, bytes_per_wakeup_interval + producer bytes: {}",
                    bytes_in_rb,
                    bytes_per_wakeup_interval + producer_bytes
                ),
            ));
        }

        let safe_bytes_in_rb = bytes_in_rb - producer_bytes;

        let mut late_wakeups = 0;

        // The device writes the ring buffer in chunks that could be as large as `producer_bytes`.
        // We must stay at least this far behind the ring buffer capture position at all times.
        let producer_frames = producer_bytes / format.bytes_per_frame() as u64;
        // If we want to read approx. wakeup_interval of data each time we awaken, we should delay
        // our first wakeup by the duration needed for the device to advance by `producer_bytes`.
        // We intentionally round-up the result, so this delay duration is conservative.
        let first_capture_delay = zx::MonotonicDuration::from_nanos(
            (producer_frames * NANOSECONDS_PER_SECOND).div_ceil(format.frames_per_second as u64)
                as i64,
        );

        // To start, sleep until (ring_buffer_start + first_capture_delay + wakeup_interval), so we
        // can start reading from the beginning of the ring buffer and have approx (wakeup_inverval)
        // worth of audio data ready for us to read. We define t0 as the moment when "safe to read"
        // finally reaches ring buffer position 0 and starts advancing.
        let ring_buffer_start = ring_buffer.start().await?;
        let t0_for_recording = ring_buffer_start + first_capture_delay;
        let mut last_wakeup = t0_for_recording;
        let mut next_frame_to_read = 0u64; // The number of frames read from the ring buffer so far.

        fuchsia_async::Timer::new(t0_for_recording).await;

        // This periodic alarm represents the next time we wake up and read the ring buffer.
        let mut timer = fuchsia_async::Interval::new(wakeup_interval);
        // We copy from ring buffer to this intermediate buffer, and from there to socket.
        let mut buf = vec![format.sample_type.silence_value(); bytes_per_wakeup_interval as usize];
        let stop_signal = AtomicBool::new(false);

        socket.write_header(duration, format).await?;
        let packet_fut = async {
            loop {
                timer.next().await;
                // Check that we woke up on time. Determine the ring buffer pointer position based
                // on the current time and the rate at which it moves.
                // Ring buffer pointer should be ahead of last byte read.
                let now = zx::MonotonicInstant::get();

                if stop_signal.load(Ordering::SeqCst) {
                    break;
                }

                let elapsed_since_start = now - t0_for_recording;
                // We multiply (with u64) before dividing to preserve precision. We intentionally
                // drop any partial frame position, so this running counter is conservative.
                let elapsed_frames_since_start = (format.frames_per_second as u64
                    * elapsed_since_start.into_nanos() as u64)
                    / NANOSECONDS_PER_SECOND;

                let available_frames_to_read = elapsed_frames_since_start - next_frame_to_read;
                let bytes_to_read = available_frames_to_read * format.bytes_per_frame() as u64;
                if buf.len() < bytes_to_read as usize {
                    buf.resize(bytes_to_read as usize, 0);
                }

                // Check for late wakeup to know whether we have missed reading some audio signal.
                //
                // In any given wakeup period, it is "unsafe" to read bytes that the driver is
                // currently updating, since we could be reading stale data.
                // There are (producer_bytes + bytes_per_wakeup_interval) unsafe bytes, since reads
                // can take up to one period in the worst case. The remaining ring buffer bytes are
                // safe to read. The amount of bytes available to be read is the difference between
                // the last position we read from, and the current elapsed position. If that amount
                // exceeds the amount of safe bytes, then we've awakened too late and the audio
                // device overwrote some audio frames before we could read them. In that case, we
                // span those missed bytes with silence, so that we still transfer the correct
                // number of frames to the socket.
                let bytes_missed = if bytes_to_read > safe_bytes_in_rb {
                    (bytes_to_read - safe_bytes_in_rb) as usize
                } else {
                    0usize
                };
                let elapsed_since_last_wakeup = now - last_wakeup;
                if bytes_missed > 0 {
                    println!(
                        "Woke up {} ns late",
                        elapsed_since_last_wakeup.into_nanos() - wakeup_interval.into_nanos()
                    );
                    late_wakeups += 1;
                }

                buf[..bytes_missed].fill(format.sample_type.silence_value());
                let _ = ring_buffer
                    .vmo_buffer()
                    .read_from_frame(next_frame_to_read, &mut buf[bytes_missed..])?;

                next_frame_to_read += available_frames_to_read;

                // This will be true until we reach the end of the intended capture duration.
                let write_all_elapsed_frames = duration.is_none()
                    || (format.frames_in_duration(duration.unwrap_or_default())
                        - next_frame_to_read
                        > available_frames_to_read);

                if write_all_elapsed_frames {
                    socket.0.write_all(&buf[..bytes_to_read as usize]).await?;
                    last_wakeup = now;
                } else {
                    let bytes_to_write = (format.frames_in_duration(duration.unwrap_or_default())
                        - next_frame_to_read) as usize
                        * format.bytes_per_frame() as usize;
                    socket.0.write_all(&buf[..bytes_to_write]).await?;
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
