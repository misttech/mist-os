// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::sync::Arc;

use anyhow::{format_err, Error};
use futures::lock::Mutex;
use futures::TryStreamExt;
use log::trace;

/// The OutputWorker is an implementation of an output virtual audio device.
///
/// When registered with the audio system, this device replaces the normal output speaker and
/// receives any audio that would have been played by the device.
///
/// It supports capturing parts of the output stream for later processing.
#[derive(Debug, Default)]
pub(crate) struct OutputWorker {
    extracted_data: Vec<u8>,
    vmo: Option<zx::Vmo>,

    // Whether we should store samples when we receive notification from the VAD
    capturing: bool,

    // How much of the vmo's data we're actually using, in bytes.
    work_space: u64,

    // How often, in frames, we want to be updated on the state of the extraction ring buffer.
    frames_per_notification: u64,

    // How many bytes a frame is.
    frame_size: u64,

    // Offset into vmo where we'll start to read next, in bytes.
    next_read: u64,

    // Offset into vmo where we'll finish reading next, in bytes.
    next_read_end: u64,
}

impl OutputWorker {
    /// Start capturing output audio to an internal buffer.
    ///
    /// Returns an error if capturing is already started.
    pub(crate) fn start_capturing(&mut self) -> Result<(), Error> {
        if self.capturing {
            return Err(format_err!("Failed to start capturing: already capturing audio output"));
        }
        self.capturing = true;
        Ok(())
    }

    /// Stop capturing audio output.
    ///
    /// Returns an error if capturing is not started.
    pub(crate) fn stop_capturing(&mut self) -> Result<(), Error> {
        if !self.capturing {
            return Err(format_err!("Failed to stop capturing: not capturing audio output"));
        }
        self.capturing = false;
        Ok(())
    }

    /// Get the size of the captured audio.
    pub(crate) fn extracted_data_len(&mut self) -> Result<usize, Error> {
        if self.capturing {
            return Err(format_err!("Still capturing audio output"));
        }
        Ok(self.extracted_data.len())
    }

    /// Returns the audio extracted from the last capture.
    ///
    /// The internal captured audio is reset by this function.
    pub(crate) fn take_extracted_data(&mut self) -> Result<Vec<u8>, Error> {
        if self.capturing {
            return Err(format_err!("Still capturing audio output"));
        }
        Ok(std::mem::take(&mut self.extracted_data))
    }

    fn on_set_format(
        &mut self,
        frames_per_second: u32,
        sample_format: u32,
        num_channels: u32,
        _external_delay: i64,
    ) -> Result<(), Error> {
        let sample_size = crate::util::get_sample_size(sample_format)?;
        self.frame_size = u64::from(num_channels) * u64::from(sample_size);

        let frames_per_millisecond = u64::from(frames_per_second / 1000);
        self.frames_per_notification = frames_per_millisecond * 50;
        trace!(
            fps = frames_per_second,
            bpf = self.frame_size;
            "AudioFacade::OutputWorker: configuring"
        );
        Ok(())
    }

    async fn create_buffer_and_get_notification_frequency(
        &mut self,
        ring_buffer: zx::Vmo,
        num_ring_buffer_frames: u32,
        _notifications_per_ring: u32,
    ) -> Result<u32, Error> {
        // Ignore AudioCore's notification cadence (_notifications_per_ring); set up our own.
        let target_notifications_per_ring =
            u64::from(num_ring_buffer_frames) / self.frames_per_notification;
        let target_notifications_per_ring = target_notifications_per_ring.try_into()?;

        trace!(
            "AudioFacade::OutputWorker: created buffer with {:?} frames, {:?} notifications",
            num_ring_buffer_frames,
            target_notifications_per_ring
        );

        self.work_space = u64::from(num_ring_buffer_frames) * self.frame_size;

        // Start reading from the beginning.
        self.next_read = 0;
        self.next_read_end = 0;

        self.vmo = Some(ring_buffer);
        Ok(target_notifications_per_ring)
    }

    fn on_position_notify(
        &mut self,
        _monotonic_time: i64,
        ring_position: u32,
        capturing: bool,
    ) -> Result<(), Error> {
        if capturing && self.next_read != self.next_read_end {
            let vmo = if let Some(vmo) = &self.vmo { vmo } else { return Ok(()) };

            trace!(
                "AudioFacade::OutputWorker read byte {:?} to {:?}",
                self.next_read,
                self.next_read_end
            );

            if self.next_read_end < self.next_read {
                // Wrap-around case, read through the end.
                let len = (self.work_space - self.next_read).try_into()?;
                let mut data = vec![0u8; len];
                let overwrite1 = vec![1u8; len];
                vmo.read(&mut data, self.next_read)?;
                vmo.write(&overwrite1, self.next_read)?;
                self.extracted_data.append(&mut data);

                // Read remaining data.
                let next_read_end = self.next_read_end.try_into()?;
                let mut data = vec![0u8; next_read_end];
                let overwrite2 = vec![1u8; next_read_end];
                vmo.read(&mut data, 0)?;
                vmo.write(&overwrite2, 0)?;

                self.extracted_data.append(&mut data);
            } else {
                // Normal case, just read all the bytes.
                let len = (self.next_read_end - self.next_read).try_into()?;
                let mut data = vec![0u8; len];
                let overwrite = vec![1u8; len];
                vmo.read(&mut data, self.next_read)?;
                vmo.write(&overwrite, self.next_read)?;

                self.extracted_data.append(&mut data);
            }
        }
        // We always stay 1 notification behind, since audio_core writes audio data into
        // our shared buffer based on these same notifications. This avoids audio glitches.
        self.next_read = self.next_read_end;
        self.next_read_end = ring_position.into();
        Ok(())
    }

    /// Asynchronously serve the virtual output device.
    pub(crate) async fn run(
        worker: Arc<Mutex<OutputWorker>>,
        va_output: fidl_fuchsia_virtualaudio::DeviceProxy,
    ) -> Result<(), Error> {
        let mut output_events = va_output.take_event_stream();

        // Monotonic timestamp returned by the most-recent OnStart/OnPositionNotify/OnStop response.
        let mut last_timestamp = zx::MonotonicInstant::from_nanos(0);
        // Observed monotonic time that OnStart/OnPositionNotify/OnStop messages actually arrived.
        let mut last_event_time = zx::MonotonicInstant::from_nanos(0);

        while let Some(output_msg) = output_events.try_next().await? {
            match output_msg {
                fidl_fuchsia_virtualaudio::DeviceEvent::OnSetFormat {
                    frames_per_second,
                    sample_format,
                    num_channels,
                    external_delay,
                } => {
                    worker.lock().await.on_set_format(
                        frames_per_second,
                        sample_format,
                        num_channels,
                        external_delay,
                    )?;
                }
                fidl_fuchsia_virtualaudio::DeviceEvent::OnBufferCreated {
                    ring_buffer,
                    num_ring_buffer_frames,
                    notifications_per_ring,
                } => {
                    let target_notifications_per_ring = worker
                        .lock()
                        .await
                        .create_buffer_and_get_notification_frequency(
                            ring_buffer,
                            num_ring_buffer_frames,
                            notifications_per_ring,
                        )
                        .await?;

                    va_output
                        .set_notification_frequency(target_notifications_per_ring)
                        .await?
                        .map_err(|status| {
                            format_err!("SetNotificationFrequency returned error {:?}", status)
                        })?;
                }
                fidl_fuchsia_virtualaudio::DeviceEvent::OnStart { start_time } => {
                    if worker.lock().await.capturing
                        && last_timestamp > zx::MonotonicInstant::from_nanos(0)
                    {
                        trace!("AudioFacade::OutputWorker: Extraction OnPositionNotify received before OnStart");
                    }
                    last_timestamp = zx::MonotonicInstant::from_nanos(start_time);
                    last_event_time = zx::MonotonicInstant::get();
                }
                fidl_fuchsia_virtualaudio::DeviceEvent::OnStop {
                    stop_time: _,
                    ring_position: _,
                } => {
                    if last_timestamp == zx::MonotonicInstant::from_nanos(0) {
                        trace!(
                                    "AudioFacade::OutputWorker: Extraction OnPositionNotify timestamp cleared before OnStop");
                    }
                    last_timestamp = zx::MonotonicInstant::from_nanos(0);
                    last_event_time = zx::MonotonicInstant::from_nanos(0);
                }
                fidl_fuchsia_virtualaudio::DeviceEvent::OnPositionNotify {
                    monotonic_time,
                    ring_position,
                } => {
                    let monotonic_zx_time = zx::MonotonicInstant::from_nanos(monotonic_time);
                    let now = zx::MonotonicInstant::get();

                    let mut worker = worker.lock().await;
                    let capturing = worker.capturing;

                    // To minimize logspam, log glitches only when capturing.
                    if capturing {
                        if last_timestamp == zx::MonotonicInstant::from_nanos(0) {
                            trace!(
                                        "AudioFacade::OutputWorker: Extraction OnStart not received before OnPositionNotify");
                        }

                        // Log if our timestamps had a gap of more than 100ms. This is highly
                        // abnormal and indicates possible glitching while receiving playback
                        // audio from the system and/or extracting it for analysis.
                        let timestamp_interval = monotonic_zx_time - last_timestamp;
                        if timestamp_interval > zx::MonotonicDuration::from_millis(100) {
                            trace!(
                                        "AudioFacade::OutputWorker: Extraction position timestamp jumped by more than 100ms ({:?}ms). Expect glitches.",
                                        timestamp_interval.into_millis());
                        }
                        if monotonic_zx_time < last_timestamp {
                            trace!(
                                        "AudioFacade::OutputWorker: Extraction position timestamp moved backwards ({:?}ms). Expect glitches.",
                                        timestamp_interval.into_millis());
                        }

                        // Log if there was a gap in position notification arrivals of more
                        // than 150ms. This is highly abnormal and indicates possible glitching
                        // while receiving playback audio from the system and/or extracting it
                        // for analysis.
                        let observed_interval = now - last_event_time;
                        if observed_interval > zx::MonotonicDuration::from_millis(150) {
                            trace!(
                                        "AudioFacade::OutputWorker: Extraction position not updated for 150ms ({:?}ms). Expect glitches.",
                                        observed_interval.into_millis());
                        }
                    }

                    last_timestamp = monotonic_zx_time;
                    last_event_time = now;

                    worker.on_position_notify(monotonic_time, ring_position, capturing)?;
                }
                evt => {
                    trace!("Unhandled event {evt:?}");
                }
            }
        }
        Ok(())
    }
}
