// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{socket, RingBuffer, SECONDS_PER_NANOSECOND},
    anyhow::{self, Context, Error},
    fdio,
    fidl::endpoints::Proxy,
    fidl_fuchsia_audio_ffxdaemon::DeviceInfo,
    fidl_fuchsia_hardware_audio::StreamConfigProxy,
    fidl_fuchsia_io as fio, fuchsia_async as fasync,
    fuchsia_zircon::{self as zx},
};

pub struct Device {
    stream_config_client: StreamConfigProxy,
}

impl Device {
    pub fn connect(path: String) -> Self {
        // Connect to a StreamConfig channel.
        let (connector_client, connector_server) = fidl::endpoints::create_proxy::<
            fidl_fuchsia_hardware_audio::StreamConfigConnectorMarker,
        >()
        .expect("failed to create streamconfig");

        let (stream_config_client, stream_config_connector) =
            fidl::endpoints::create_proxy::<fidl_fuchsia_hardware_audio::StreamConfigMarker>()
                .expect("failed to create streamconfig ");

        fdio::service_connect(&path, connector_server.into_channel()).unwrap();
        // Using StreamConfigConnector client, pass the server end of StreamConfig
        // channel to the device so that device can respond to StreamConfig requests.
        connector_client.connect(stream_config_connector).unwrap();

        Self { stream_config_client }
    }

    pub async fn get_info(&self) -> Result<DeviceInfo, Error> {
        let stream_properties = self.stream_config_client.get_properties().await?;
        let supported_formats = self.stream_config_client.get_supported_formats().await?;
        let gain_state = self.stream_config_client.watch_gain_state().await?;
        let plug_state = self.stream_config_client.watch_plug_state().await?;

        Ok(DeviceInfo {
            stream_properties: Some(stream_properties),
            supported_formats: Some(supported_formats),
            gain_state: Some(gain_state),
            plug_state: Some(plug_state),
            ..DeviceInfo::EMPTY
        })
    }

    pub async fn play(self, mut data_socket: fasync::Socket) -> Result<String, Error> {
        let mut socket = socket::Socket { socket: &mut data_socket };
        let spec = socket.read_wav_header().await?;
        let format = format_utils::Format::from(&spec);

        let supported_formats = self.stream_config_client.get_supported_formats().await?;
        if !format.is_supported_by(supported_formats) {
            panic!("Requested format not supported");
        }

        // Create ring buffer channel.
        let (ring_buffer_client, ring_buffer_server) =
            fidl::endpoints::create_proxy::<fidl_fuchsia_hardware_audio::RingBufferMarker>()
                .expect("failed to create ring buffer channel");

        self.stream_config_client.create_ring_buffer(
            fidl_fuchsia_hardware_audio::Format::from(&format),
            ring_buffer_server,
        )?;
        let ring_buffer_wrapper = RingBuffer::new(&format, ring_buffer_client).await?;

        let mut silenced_frames = 0u64;
        let mut late_wakeups = 0;
        let mut last_frame_written = 0u64;

        let nanos_per_wakeup_interval = 10e6f64; // 10 milliseconds
        let wakeup_interval = zx::Duration::from_millis(10);

        let frames_per_nanosecond = format.frames_per_second as f64 * SECONDS_PER_NANOSECOND;
        let producer_bytes = (frames_per_nanosecond * nanos_per_wakeup_interval).floor() as u64
            / format.bytes_per_frame() as u64;
        let bytes_in_rb = ring_buffer_wrapper.num_frames * format.bytes_per_frame() as u64;
        let consumer_bytes = ring_buffer_wrapper.consumer_bytes;
        let bytes_per_wakeup_interval =
            (nanos_per_wakeup_interval * frames_per_nanosecond * format.bytes_per_frame() as f64)
                .floor() as u64;

        if consumer_bytes + producer_bytes > bytes_in_rb {
            panic!(
                "Ring buffer not large enough for internal delay. Ring buffer bytes: {}, 
            consumer + producer bytes: {}",
                bytes_in_rb,
                consumer_bytes + producer_bytes
            )
        }

        let t_zero = zx::Time::from_nanos(ring_buffer_wrapper.start().await?);

        println!(
            "Time between present and t0 returned by start {}",
            (zx::Time::get_monotonic() - t_zero).into_nanos()
        );

        // Running counter representing the next time we'll wake up and write to ring buffer.
        // To start, sleep until at least t0 + (wakeup_interval) so we can start writing at
        // the first bytes in the ring buffer.
        let mut next_wakeup = t_zero + wakeup_interval;

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

        loop {
            next_wakeup.sleep(); // Wraps zx::nanosleep(wakeup_time);

            // Check that we woke up on time. Approximate ring buffer pointer position based on
            // clock time. Ring buffer pointer should be ahead of last byte written.
            let now = zx::Time::get_monotonic();

            let duration_since_last_wakeup = now - last_wakeup;
            last_wakeup = now;
            next_wakeup = now + wakeup_interval;

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

            if (rb_frames_elapsed_since_last_wakeup.floor() as i64
                - new_frames_available_to_write as i64)
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

            ring_buffer_wrapper.write_to_frame(last_frame_written, &mut buf)?;
            last_frame_written += new_frames_available_to_write;

            // We want entire ring buffer to be silenced.
            if silenced_frames * format.bytes_per_frame() as u64 >= bytes_in_rb {
                break;
            }
        }

        ring_buffer_wrapper.stop().await?;

        let output_message = format!(
            "Succesfully processed all audio data. \n Woke up late {} times.\n ",
            late_wakeups
        );

        Ok(output_message)
    }
}

// Helper function to enumerate on device directories to get information about available drivers.
pub async fn get_entries(path: &str) -> Result<Vec<String>, Error> {
    let (control_client, control_server) = zx::Channel::create();

    // Creates a connection to a FIDL service at path.
    fdio::service_connect(path, control_server)
        .context(format!("failed to connect to {:?}", path))?;

    let directory_proxy =
        fio::DirectoryProxy::from_channel(fasync::Channel::from_channel(control_client).unwrap());

    let (status, mut buf) =
        directory_proxy.read_dirents(fio::MAX_BUF).await.expect("Failure calling read dirents");

    if status != 0 {
        return Err(anyhow::anyhow!("Unable to call read dirents, status returned: {}", status));
    }

    let entry_names = fuchsia_fs::directory::parse_dir_entries(&mut buf);
    let full_paths: Vec<String> = entry_names
        .into_iter()
        .filter_map(|s| Some(path.to_owned() + &s.ok().unwrap().name))
        .collect();

    Ok(full_paths)
}
