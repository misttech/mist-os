// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::format_err,
    async_helpers::maybe_stream::MaybeStream,
    fidl::{endpoints::ClientEnd, prelude::*},
    fidl_fuchsia_hardware_audio::*,
    fidl_fuchsia_media, fuchsia_async as fasync, fuchsia_inspect as inspect,
    fuchsia_inspect_derive::{AttachError, IValue, Inspect},
    fuchsia_zircon::{self as zx, DurationNum},
    futures::{
        select,
        stream::{FusedStream, Stream},
        task::{Context, Poll},
        FutureExt, StreamExt,
    },
    parking_lot::Mutex,
    std::{pin::Pin, sync::Arc},
    tracing::{info, warn},
};

use crate::frame_vmo;
use crate::types::{AudioSampleFormat, Error, Result};

enum OutputOrTask {
    Output(SoftPcmOutput),
    Task(fasync::Task<Result<()>>),
    Complete,
}

impl OutputOrTask {
    fn start(&mut self) {
        *self = match std::mem::replace(self, OutputOrTask::Complete) {
            OutputOrTask::Output(pcm) => {
                OutputOrTask::Task(fasync::Task::spawn(pcm.process_requests()))
            }
            x => x,
        }
    }
}

/// A Stream that produces audio frames.
/// Usually acquired via SoftPcmOutput::take_frame_stream().
pub struct AudioFrameStream {
    /// Handle to the VMO that is receiving the frames.
    frame_vmo: Arc<Mutex<frame_vmo::FrameVmo>>,
    /// The index of the next frame we should retrieve.
    next_frame_index: usize,
    /// Number of frames to return in a packet.
    packet_frames: usize,
    /// Vector that will be filled with a packet.
    /// Replaced when stream produces a packet.
    next_packet: std::cell::RefCell<Vec<u8>>,
    /// SoftPcmOutput this is attached to, or the task currently processing the FIDL requests.
    output: OutputOrTask,
    /// Inspect node
    inspect: inspect::Node,
}

impl AudioFrameStream {
    fn new(output: SoftPcmOutput) -> AudioFrameStream {
        AudioFrameStream {
            frame_vmo: output.frame_vmo.clone(),
            next_frame_index: 0,
            packet_frames: output.packet_frames,
            next_packet: Vec::new().into(),
            output: OutputOrTask::Output(output),
            inspect: Default::default(),
        }
    }

    /// Start the requests task if not started, and poll the task.
    fn poll_task(&mut self, cx: &mut Context<'_>) -> Poll<Result<()>> {
        if let OutputOrTask::Complete = &self.output {
            return Poll::Ready(Err(Error::InvalidState));
        }
        if let OutputOrTask::Task(ref mut task) = &mut self.output {
            return task.poll_unpin(cx);
        }
        self.output.start();
        self.poll_task(cx)
    }
}

impl Stream for AudioFrameStream {
    type Item = Result<Vec<u8>>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Poll::Ready(r) = self.poll_task(cx) {
            self.output = OutputOrTask::Complete;
            return Poll::Ready(r.err().map(Result::Err));
        }
        if self.next_packet.borrow().len() == 0 {
            if let Some(new_len) = self.frame_vmo.lock().bytecount_frames(self.packet_frames) {
                self.next_packet.borrow_mut().resize(new_len, 0);
            }
        }
        let result = {
            let mut lock = self.frame_vmo.lock();
            futures::ready!(lock.poll_frames(
                self.next_frame_index,
                self.next_packet.borrow_mut().as_mut_slice(),
                cx
            ))
        };

        match result {
            Ok((latest, missed)) => {
                if missed > 0 {
                    info!("Missed {} frames due to slow polling", missed);
                }
                self.next_frame_index = latest + 1;
                let vec_mut = self.next_packet.get_mut();
                let bytes = vec_mut.len();
                let frames = std::mem::replace(vec_mut, vec![0; bytes]);
                Poll::Ready(Some(Ok(frames)))
            }
            Err(e) => Poll::Ready(Some(Err(e))),
        }
    }
}

impl FusedStream for AudioFrameStream {
    fn is_terminated(&self) -> bool {
        match self.output {
            OutputOrTask::Complete => true,
            _ => false,
        }
    }
}

impl Inspect for &mut AudioFrameStream {
    fn iattach(
        self,
        parent: &fuchsia_inspect::Node,
        name: impl AsRef<str>,
    ) -> core::result::Result<(), AttachError> {
        self.inspect = parent.create_child(name.as_ref());
        if let OutputOrTask::Output(ref mut o) = &mut self.output {
            return o.iattach(&self.inspect, "soft_pcm_output");
        }
        Ok(())
    }
}

/// Number of frames within the duration.  This includes frames that end at exactly the duration.
pub(crate) fn frames_from_duration(frames_per_second: usize, duration: fasync::Duration) -> usize {
    assert!(duration >= 0.nanos(), "frames_from_duration is not defined for negative durations");
    let mut frames = duration.into_seconds() * frames_per_second as i64;
    let frames_partial =
        ((duration.into_nanos() % 1_000_000_000) as f64 / 1e9) * frames_per_second as f64;
    frames += frames_partial as i64;
    frames as usize
}

/// A software fuchsia audio output, which implements Audio Driver Streaming Interface
/// as defined in //docs/concepts/drivers/driver_interfaces/audio_streaming.md
#[derive(Inspect)]
pub struct SoftPcmOutput {
    /// The Stream channel handles format negotiation, plug detection, and gain
    stream_config_stream: StreamConfigRequestStream,

    /// The Unique ID that this stream will present to the system
    unique_id: [u8; 16],
    /// The manufacturer of the hardware for this stream
    manufacturer: String,
    /// A product description for the hardware for the stream
    product: String,
    /// The clock domain that this stream will present to the system
    clock_domain: u32,

    /// The supported format of this output.
    /// Currently only support one format per output is supported.
    supported_formats: PcmSupportedFormats,

    /// The size of packet which is produced.
    /// Note that this may be slightly shorter than the requested duration, as it is always an
    /// exact (to nanosecond) duration of the frames in a packet.
    packet_duration: zx::Duration,

    /// The number of audio frames per packet from the frame stream.
    /// Used to calculate audio buffer sizes.
    packet_frames: usize,

    /// The currently set format, in frames per second, audio sample format, and channels.
    current_format: Option<(u32, AudioSampleFormat, u16)>,

    /// The request stream for the ringbuffer.
    ring_buffer_stream: MaybeStream<RingBufferRequestStream>,

    /// A pointer to the ring buffer for this stream
    frame_vmo: Arc<Mutex<frame_vmo::FrameVmo>>,

    /// The current delay that has been communicated exists after the audio is retrieved.
    external_delay: zx::Duration,

    /// Replied to plugged state watch.
    plug_state_replied: bool,

    /// Replied to gain state watch.
    gain_state_replied: bool,

    /// Replied to delay info watch.
    delay_info_replied: bool,

    /// Inspect node
    #[inspect(forward)]
    inspect: SoftPcmInspect,
}

#[derive(Default, Inspect)]
struct SoftPcmInspect {
    inspect_node: inspect::Node,
    ring_buffer_format: IValue<Option<String>>,
    frame_vmo_status: IValue<Option<String>>,
}

impl SoftPcmInspect {
    fn record_current_format(&mut self, current: &(u32, AudioSampleFormat, u16)) {
        self.ring_buffer_format
            .iset(Some(format!("{} rate: {} channels: {}", current.1, current.0, current.2)));
    }

    fn record_vmo_status(&mut self, new: &str) {
        self.frame_vmo_status.iset(Some(new.to_owned()));
    }
}

impl SoftPcmOutput {
    /// Create a new software audio device, returning a client channel which can be supplied
    /// to the AudioCore and will act correctly as an audio output driver channel which can
    /// render audio in the `pcm_format` format, and an AudioFrameStream which produces the
    /// audio frames delivered to the audio output.
    /// Spawns a task to handle messages from the Audio Core and setup of internal VMO buffers
    /// required for audio output.  See AudioFrameStream for more information on timing
    /// requirements for audio output.
    /// `packet_duration`: desired duration of an audio packet returned by the stream. Rounded down to
    /// end on a audio frame boundary.
    /// `initial_external_delay`: delay that is added after packets have been returned from the stream
    pub fn build(
        unique_id: &[u8; 16],
        manufacturer: &str,
        product: &str,
        clock_domain: u32,
        pcm_format: fidl_fuchsia_media::PcmFormat,
        packet_duration: zx::Duration,
        initial_external_delay: zx::Duration,
    ) -> Result<(ClientEnd<StreamConfigMarker>, AudioFrameStream)> {
        if pcm_format.bits_per_sample % 8 != 0 {
            // Non-byte-aligned format not allowed.
            return Err(Error::InvalidArgs);
        }
        let (client, request_stream) =
            fidl::endpoints::create_request_stream::<StreamConfigMarker>()
                .expect("Error creating stream config endpoint");

        let number_of_channels = pcm_format.channel_map.len();
        let attributes = vec![ChannelAttributes::EMPTY; number_of_channels];
        let channel_set = ChannelSet { attributes: Some(attributes), ..ChannelSet::EMPTY };
        let supported_formats = PcmSupportedFormats {
            channel_sets: Some(vec![channel_set]),
            sample_formats: Some(vec![SampleFormat::PcmSigned]),
            bytes_per_sample: Some(vec![(pcm_format.bits_per_sample / 8) as u8]),
            valid_bits_per_sample: Some(vec![pcm_format.bits_per_sample as u8]),
            frame_rates: Some(vec![pcm_format.frames_per_second]),
            ..PcmSupportedFormats::EMPTY
        };

        let packet_frames =
            frames_from_duration(pcm_format.frames_per_second as usize, packet_duration);
        let packet_duration = zx::Duration::from_nanos(
            (i64::try_from(packet_frames).unwrap() * 1_000_000_000)
                / pcm_format.frames_per_second as i64,
        );

        let stream = SoftPcmOutput {
            stream_config_stream: request_stream,
            unique_id: unique_id.clone(),
            manufacturer: manufacturer.to_string(),
            product: product.to_string(),
            clock_domain,
            supported_formats,
            packet_duration,
            packet_frames,
            current_format: None,
            ring_buffer_stream: Default::default(),
            frame_vmo: Arc::new(Mutex::new(frame_vmo::FrameVmo::new()?)),
            external_delay: initial_external_delay,
            plug_state_replied: false,
            gain_state_replied: false,
            delay_info_replied: false,
            inspect: Default::default(),
        };

        Ok((client, AudioFrameStream::new(stream)))
    }

    async fn process_requests(mut self) -> Result<()> {
        loop {
            select! {
                stream_config_request = self.stream_config_stream.next() => {
                    match stream_config_request {
                        Some(Ok(r)) => {
                            if let Err(e) = self.handle_stream_request(r) {
                                warn!("stream config request error: {:?}", e)
                            }
                        },
                        Some(Err(e)) => {
                            warn!("stream config error: {:?}, stopping", e);
                            return Err(e.into());
                        },
                        None => {
                            warn!("stream config disconnected, stopping");
                            return Ok(());
                        },
                    }
                }
                ring_buffer_request = self.ring_buffer_stream.next() => {
                    match ring_buffer_request {
                        Some(Ok(r)) => {
                            if let Err(e) = self.handle_ring_buffer_request(r) {
                                warn!("ring buffer request error: {:?}", e)
                            }
                        },
                        Some(Err(e)) => {
                            warn!("ring buffer error: {:?}, dropping ringbuffer", e);
                            let _ = MaybeStream::take(&mut self.ring_buffer_stream);
                        },
                        None => {
                            warn!("ring buffer finished, dropping");
                            let _ = MaybeStream::take(&mut self.ring_buffer_stream);
                        },
                    }
                }
            }
        }
    }

    fn handle_stream_request(
        &mut self,
        request: StreamConfigRequest,
    ) -> std::result::Result<(), anyhow::Error> {
        match request {
            StreamConfigRequest::GetHealthState { responder } => {
                responder.send(HealthState::EMPTY)?;
            }
            StreamConfigRequest::SignalProcessingConnect { protocol: _, control_handle } => {
                control_handle.shutdown_with_epitaph(zx::Status::NOT_SUPPORTED);
            }
            StreamConfigRequest::GetProperties { responder } => {
                #[rustfmt::skip]
                let prop = StreamProperties {
                    unique_id:                Some(self.unique_id),
                    is_input:                 Some(false),
                    can_mute:                 Some(false),
                    can_agc:                  Some(false),
                    min_gain_db:              Some(0f32),
                    max_gain_db:              Some(0f32),
                    gain_step_db:             Some(0f32),
                    plug_detect_capabilities: Some(PlugDetectCapabilities::Hardwired),
                    clock_domain:             Some(self.clock_domain),
                    manufacturer:             Some(self.manufacturer.to_string()),
                    product:                  Some(self.product.to_string()),
                    ..StreamProperties::EMPTY
                };
                responder.send(prop)?;
            }
            StreamConfigRequest::GetSupportedFormats { responder } => {
                let pcm_formats = self.supported_formats.clone();
                let formats_vector = vec![SupportedFormats {
                    pcm_supported_formats: Some(pcm_formats),
                    ..SupportedFormats::EMPTY
                }];
                responder.send(&mut formats_vector.into_iter())?;
            }
            StreamConfigRequest::CreateRingBuffer { format, ring_buffer, control_handle: _ } => {
                let pcm = (format.pcm_format.ok_or(format_err!("No pcm_format included")))?;
                self.ring_buffer_stream.set(ring_buffer.into_stream()?);
                let current = (pcm.frame_rate, pcm.into(), pcm.number_of_channels.into());
                self.inspect.record_current_format(&current);
                self.current_format = Some(current);
                self.delay_info_replied = false;
            }
            StreamConfigRequest::WatchGainState { responder } => {
                if self.gain_state_replied {
                    // We will never change gain state.
                    responder.drop_without_shutdown();
                    return Ok(());
                }
                let gain_state = GainState {
                    muted: Some(false),
                    agc_enabled: Some(false),
                    gain_db: Some(0.0f32),
                    ..GainState::EMPTY
                };
                responder.send(gain_state)?;
                self.gain_state_replied = true
            }
            StreamConfigRequest::WatchPlugState { responder } => {
                if self.plug_state_replied {
                    // We will never change plug state.
                    responder.drop_without_shutdown();
                    return Ok(());
                }
                let time = fasync::Time::now();
                let plug_state = PlugState {
                    plugged: Some(true),
                    plug_state_time: Some(time.into_nanos() as i64),
                    ..PlugState::EMPTY
                };
                responder.send(plug_state)?;
                self.plug_state_replied = true;
            }
            StreamConfigRequest::SetGain { target_state, control_handle: _ } => {
                if let Some(true) = target_state.muted {
                    warn!("Mute is not supported");
                }
                if let Some(true) = target_state.agc_enabled {
                    warn!("AGC is not supported");
                }
                if let Some(gain) = target_state.gain_db {
                    if gain != 0.0 {
                        warn!("Non-zero gain setting not supported");
                    }
                }
            }
        }
        Ok(())
    }

    fn handle_ring_buffer_request(
        &mut self,
        request: RingBufferRequest,
    ) -> std::result::Result<(), anyhow::Error> {
        match request {
            RingBufferRequest::GetProperties { responder } => {
                let prop = RingBufferProperties {
                    needs_cache_flush_or_invalidate: Some(false),
                    ..RingBufferProperties::EMPTY
                };
                responder.send(prop)?;
            }
            RingBufferRequest::GetVmo {
                min_frames,
                clock_recovery_notifications_per_ring,
                responder,
            } => {
                let (fps, format, channels) = match &self.current_format {
                    None => {
                        let mut error = Err(GetVmoError::InternalError);
                        if let Err(e) = responder.send(&mut error) {
                            warn!("Error on get vmo error send: {:?}", e);
                        }
                        return Ok(());
                    }
                    Some(x) => x.clone(),
                };
                // Require a minimum amount of frames for three packets.
                let min_frames_from_duration = 3 * self.packet_frames as u32;
                let ring_buffer_frames = min_frames.max(min_frames_from_duration);
                self.inspect.record_vmo_status("gotten");
                match self.frame_vmo.lock().set_format(
                    fps,
                    format,
                    channels,
                    ring_buffer_frames as usize,
                    clock_recovery_notifications_per_ring,
                ) {
                    Err(e) => {
                        warn!("Error on vmo set format: {:?}", e);
                        let mut error = Err(GetVmoError::InternalError);
                        responder.send(&mut error)?;
                    }
                    Ok(vmo_handle) => {
                        let mut result = Ok((min_frames, vmo_handle.into()));
                        responder.send(&mut result)?;
                    }
                }
            }
            RingBufferRequest::Start { responder } => {
                let time = fasync::Time::now();
                self.inspect.record_vmo_status(&format!("started @ {time:?}"));
                match self.frame_vmo.lock().start(time.into()) {
                    Ok(()) => responder.send(time.into_nanos() as i64)?,
                    Err(e) => {
                        warn!("Error on frame vmo start: {:?}", e);
                        responder.control_handle().shutdown_with_epitaph(zx::Status::BAD_STATE);
                    }
                }
            }
            RingBufferRequest::Stop { responder } => match self.frame_vmo.lock().stop() {
                Ok(stopped) => {
                    if !stopped {
                        info!("Stopping an unstarted ring buffer");
                    }
                    self.inspect.record_vmo_status(&format!("stopped @ {:?}", fasync::Time::now()));
                    responder.send()?;
                }
                Err(e) => {
                    warn!("Error on frame vmo stop: {:?}", e);
                    responder.control_handle().shutdown_with_epitaph(zx::Status::BAD_STATE);
                }
            },
            RingBufferRequest::WatchClockRecoveryPositionInfo { responder } => {
                self.frame_vmo.lock().set_position_responder(responder);
            }
            RingBufferRequest::SetActiveChannels { active_channels_bitmask: _, responder } => {
                responder.send(&mut Err(zx::Status::NOT_SUPPORTED.into_raw()))?;
            }
            RingBufferRequest::WatchDelayInfo { responder } => {
                if self.delay_info_replied {
                    // We will never change delay state.
                    // TODO(fxbug.dev/51726): Reply again when the external_delay changes from
                    // outside instead of just on startup.
                    responder.drop_without_shutdown();
                    return Ok(());
                }
                // internal_delay is at least our packet duration (we buffer at least that much)
                // plus whatever delay has been communicated from the client.
                let delay_info = DelayInfo {
                    internal_delay: Some(
                        self.packet_duration.into_nanos() + self.external_delay.into_nanos(),
                    ),
                    ..DelayInfo::EMPTY
                };
                responder.send(delay_info)?;
                self.delay_info_replied = true;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use fidl_fuchsia_media::{AudioChannelId, AudioPcmMode, PcmFormat};

    use async_utils::PollExt;
    use fixture::fixture;
    use futures::future;

    const TEST_UNIQUE_ID: &[u8; 16] = &[5; 16];
    const TEST_CLOCK_DOMAIN: u32 = 0x00010203;

    fn with_audio_frame_stream<F>(_name: &str, test: F)
    where
        F: FnOnce(fasync::TestExecutor, StreamConfigProxy, AudioFrameStream) -> (),
    {
        let exec = fasync::TestExecutor::new_with_fake_time();
        let format = PcmFormat {
            pcm_mode: AudioPcmMode::Linear,
            bits_per_sample: 16,
            frames_per_second: 44100,
            channel_map: vec![AudioChannelId::Lf, AudioChannelId::Rf],
        };
        let (client, frame_stream) = SoftPcmOutput::build(
            TEST_UNIQUE_ID,
            "Google",
            "UnitTest",
            TEST_CLOCK_DOMAIN,
            format,
            zx::Duration::from_millis(100),
            zx::Duration::from_millis(50),
        )
        .expect("should always build");
        test(exec, client.into_proxy().expect("channel should be available"), frame_stream)
    }

    #[fuchsia::test]
    fn test_frames_from_duration() {
        const FPS: usize = 48000;
        // At 48kHz, each frame is 20833 and 1/3 nanoseconds. We add one nanosecond
        // because frames need to be completely within the duration.
        const ONE_FRAME_NANOS: i64 = 20833 + 1;
        const THREE_FRAME_NANOS: i64 = 20833 * 3 + 1;

        assert_eq!(0, frames_from_duration(FPS, 0.nanos()));

        assert_eq!(0, frames_from_duration(FPS, (ONE_FRAME_NANOS - 1).nanos()));
        assert_eq!(1, frames_from_duration(FPS, ONE_FRAME_NANOS.nanos()));

        // Three frames is an exact number of nanoseconds, we should be able to get an exact number
        // of frames from the duration.
        assert_eq!(2, frames_from_duration(FPS, (THREE_FRAME_NANOS - 1).nanos()));
        assert_eq!(3, frames_from_duration(FPS, THREE_FRAME_NANOS.nanos()));
        assert_eq!(3, frames_from_duration(FPS, (THREE_FRAME_NANOS + 1).nanos()));

        assert_eq!(FPS, frames_from_duration(FPS, 1.second()));
        assert_eq!(72000, frames_from_duration(FPS, 1500.millis()));

        assert_eq!(10660, frames_from_duration(FPS, 222084000.nanos()));
    }

    #[fuchsia::test]
    fn soft_pcm_audio_should_end_when_stream_dropped() {
        let format = PcmFormat {
            pcm_mode: AudioPcmMode::Linear,
            bits_per_sample: 16,
            frames_per_second: 48000,
            channel_map: vec![AudioChannelId::Lf, AudioChannelId::Rf],
        };

        let mut exec = fasync::TestExecutor::new_with_fake_time();
        let (client, frame_stream) = SoftPcmOutput::build(
            TEST_UNIQUE_ID,
            &"Google".to_string(),
            &"UnitTest".to_string(),
            TEST_CLOCK_DOMAIN,
            format,
            zx::Duration::from_millis(100),
            zx::Duration::from_millis(50),
        )
        .expect("should always build");

        drop(frame_stream);

        assert_eq!(Poll::Pending, exec.run_until_stalled(&mut future::pending::<()>()));

        // The audio client should be dropped (normally this causes audio to remove the device)
        assert_eq!(Err(zx::Status::PEER_CLOSED), client.channel().write(&[0], &mut Vec::new()));
    }

    #[fixture(with_audio_frame_stream)]
    #[fuchsia::test]
    #[rustfmt::skip]
    fn soft_pcm_audio_out(mut exec: fasync::TestExecutor, stream_config: StreamConfigProxy, mut frame_stream: AudioFrameStream) {
        let mut frame_fut = frame_stream.next();
        // Poll the frame stream, which should start the processing of proxy requests.
        exec.run_until_stalled(&mut frame_fut).expect_pending("no frames yet");

        let result = exec.run_until_stalled(&mut stream_config.get_properties());
        assert!(result.is_ready());
        let props1 = match result {
            Poll::Ready(Ok(v)) => v,
            _ => panic!("stream config get properties error"),
        };

        assert_eq!(props1.unique_id.unwrap(),                *TEST_UNIQUE_ID);
        assert_eq!(props1.is_input.unwrap(),                 false);
        assert_eq!(props1.can_mute.unwrap(),                 false);
        assert_eq!(props1.can_agc.unwrap(),                  false);
        assert_eq!(props1.min_gain_db.unwrap(),              0f32);
        assert_eq!(props1.max_gain_db.unwrap(),              0f32);
        assert_eq!(props1.gain_step_db.unwrap(),             0f32);
        assert_eq!(props1.plug_detect_capabilities.unwrap(), PlugDetectCapabilities::Hardwired);
        assert_eq!(props1.manufacturer.unwrap(),             "Google");
        assert_eq!(props1.product.unwrap(),                  "UnitTest");
        assert_eq!(props1.clock_domain.unwrap(),             TEST_CLOCK_DOMAIN);

        let result = exec.run_until_stalled(&mut stream_config.get_supported_formats());
        assert!(result.is_ready());

        let formats = match result {
            Poll::Ready(Ok(v)) => v,
            _ => panic!("get supported formats error"),
        };

        let first = formats.first().to_owned().expect("supported formats to be present");
        let pcm = first.pcm_supported_formats.to_owned().expect("pcm format to be present");
        assert_eq!(pcm.channel_sets.unwrap()[0].attributes.as_ref().unwrap().len(), 2usize);
        assert_eq!(pcm.sample_formats.unwrap()[0],        SampleFormat::PcmSigned);
        assert_eq!(pcm.bytes_per_sample.unwrap()[0],      2u8);
        assert_eq!(pcm.valid_bits_per_sample.unwrap()[0], 16u8);
        assert_eq!(pcm.frame_rates.unwrap()[0],           44100);

        let (ring_buffer, server) = fidl::endpoints::create_proxy::<RingBufferMarker>()
            .expect("creating ring buffer endpoint error");

        let format = Format {
            pcm_format: Some(fidl_fuchsia_hardware_audio::PcmFormat {
                number_of_channels:      2u8,
                sample_format:           SampleFormat::PcmSigned,
                bytes_per_sample:        2u8,
                valid_bits_per_sample:   16u8,
                frame_rate:              44100,
            }),
            ..Format::EMPTY
        };

        stream_config.create_ring_buffer(format, server).expect("ring buffer error");

        let props2 = match exec.run_until_stalled(&mut ring_buffer.get_properties()) {
            Poll::Ready(Ok(v)) => v,
            x => panic!("expected Ready Ok from get_properties, got {:?}", x),
        };
        assert_eq!(props2.needs_cache_flush_or_invalidate, Some(false));

        let result = exec.run_until_stalled(&mut ring_buffer.get_vmo(88200, 0)); // 2 seconds.
        assert!(result.is_ready());
        let reply = match result {
            Poll::Ready(Ok(Ok(v))) => v,
            _ => panic!("ring buffer get vmo error"),
        };
        let audio_vmo = reply.1;

        // Frames * bytes per sample * channels per sample.
        let bytes_per_second: usize = 44100 * 2 * 2;
        let vmo_size = audio_vmo.get_size().expect("size after getbuffer");
        assert!(bytes_per_second <= vmo_size as usize);

        // Put "audio" in buffer.
        let mut sent_audio = Vec::new();
        let mut x: u8 = 0x01;
        sent_audio.resize_with(bytes_per_second, || {
            x = x.wrapping_add(2);
            x
        });

        assert_eq!(Ok(()), audio_vmo.write(&sent_audio, 0));

        exec.set_fake_time(fasync::Time::from_nanos(42));
        let _ = exec.wake_expired_timers();
        let start_time = exec.run_until_stalled(&mut ring_buffer.start());
        if let Poll::Ready(s) = start_time {
            assert_eq!(s.expect("start time error"), 42);
        } else {
            panic!("start error");
        }

        exec.run_until_stalled(&mut frame_fut).expect_pending("no frames until time passes");

        // Run the ring buffer for a bit over half a second.
        exec.set_fake_time(fasync::Time::after(zx::Duration::from_millis(500)));
        let _ = exec.wake_expired_timers();

        let result = exec.run_until_stalled(&mut frame_fut);
        assert!(result.is_ready());
        let audio_recv = match result {
            Poll::Ready(Some(Ok(v))) => v,
            x => panic!("expected Ready Ok from frame stream, got {:?}", x),
        };

        // We should receive exactly 100ms of audio
        let expect_recv_bytes = bytes_per_second / 10;
        assert_eq!(expect_recv_bytes, audio_recv.len());
        assert_eq!(&sent_audio[0..expect_recv_bytes], &audio_recv);

        let result = exec.run_until_stalled(&mut frame_fut);
        assert!(result.is_ready());
        let audio_recv = match result {
            Poll::Ready(Some(Ok(v))) => v,
            x => panic!("expected Ready Ok from frame stream, got {:?}", x),
        };

        // We should receive exactly the next 100ms of audio
        let expect_recv_bytes = bytes_per_second / 10;
        assert_eq!(expect_recv_bytes, audio_recv.len());
        assert_eq!(&sent_audio[expect_recv_bytes..expect_recv_bytes*2], &audio_recv);


        let result = exec.run_until_stalled(&mut ring_buffer.stop());
        assert!(result.is_ready());

        // Watch gain only replies once.
        let result = exec.run_until_stalled(&mut stream_config.watch_gain_state());
        assert!(result.is_ready());
        let result = exec.run_until_stalled(&mut stream_config.watch_gain_state());
        assert!(!result.is_ready());

        // Watch plug state only replies once.
        let result = exec.run_until_stalled(&mut stream_config.watch_plug_state());
        assert!(result.is_ready());
        let result = exec.run_until_stalled(&mut stream_config.watch_plug_state());
        assert!(!result.is_ready());
    }

    // Returns the number of frames that were ready in the stream, draining the stream.
    fn frames_ready(exec: &mut fasync::TestExecutor, frame_stream: &mut AudioFrameStream) -> usize {
        let mut frames = 0;
        while exec.run_until_stalled(&mut frame_stream.next()).is_ready() {
            frames += 1;
        }
        frames
    }

    #[fixture(with_audio_frame_stream)]
    #[fuchsia::test]
    fn send_positions(
        mut exec: fasync::TestExecutor,
        stream_config: StreamConfigProxy,
        mut frame_stream: AudioFrameStream,
    ) {
        // Poll the frame stream, which should start the processing of proxy requests.
        assert_eq!(0, frames_ready(&mut exec, &mut frame_stream));
        let _stream_config_properties = exec.run_until_stalled(&mut stream_config.get_properties());
        let _formats = exec.run_until_stalled(&mut stream_config.get_supported_formats());
        let (ring_buffer, server) = fidl::endpoints::create_proxy::<RingBufferMarker>()
            .expect("creating ring buffer endpoint error");

        #[rustfmt::skip]
        let format = Format {
            pcm_format: Some(fidl_fuchsia_hardware_audio::PcmFormat {
                number_of_channels:      2u8,
                sample_format:           SampleFormat::PcmSigned,
                bytes_per_sample:        2u8,
                valid_bits_per_sample:   16u8,
                frame_rate:              44100,
            }),
            ..Format::EMPTY
        };

        let result = stream_config.create_ring_buffer(format, server);
        assert!(result.is_ok());

        let _ring_buffer_properties = exec.run_until_stalled(&mut ring_buffer.get_properties());

        let some_active_channels_mask = 0xc3u64;
        let result =
            exec.run_until_stalled(&mut ring_buffer.set_active_channels(some_active_channels_mask));
        assert!(result.is_ready());
        let _ = match result {
            Poll::Ready(Ok(Err(e))) => assert_eq!(e, zx::Status::NOT_SUPPORTED.into_raw()),
            x => panic!("Expected error reply to set_active_channels, got {:?}", x),
        };

        let clock_recovery_notifications_per_ring = 10u32;
        let _ = exec.run_until_stalled(
            &mut ring_buffer.get_vmo(88200, clock_recovery_notifications_per_ring),
        ); // 2 seconds.

        exec.set_fake_time(fasync::Time::from_nanos(42));
        let _ = exec.wake_expired_timers();
        let start_time = exec.run_until_stalled(&mut ring_buffer.start());
        if let Poll::Ready(s) = start_time {
            assert_eq!(s.expect("start time error"), 42);
        } else {
            panic!("start error");
        }

        // Watch number 1.
        let mut position_info = ring_buffer.watch_clock_recovery_position_info();
        let result = exec.run_until_stalled(&mut position_info);
        assert!(!result.is_ready());

        // Now advance in between notifications, with a 2 seconds total in the ring buffer
        // and 10 notifications per ring we can get watch notifications every 200 msecs.
        exec.set_fake_time(fasync::Time::after(zx::Duration::from_millis(201)));
        let _ = exec.wake_expired_timers();
        // Each frame is 100ms, there should be two of them ready now.
        assert_eq!(2, frames_ready(&mut exec, &mut frame_stream));
        let result = exec.run_until_stalled(&mut position_info);
        assert!(result.is_ready());

        // Watch number 2.
        let mut position_info = ring_buffer.watch_clock_recovery_position_info();
        let result = exec.run_until_stalled(&mut position_info);
        assert!(!result.is_ready());
        exec.set_fake_time(fasync::Time::after(zx::Duration::from_millis(201)));
        let _ = exec.wake_expired_timers();
        assert_eq!(2, frames_ready(&mut exec, &mut frame_stream));
        let result = exec.run_until_stalled(&mut position_info);
        assert!(result.is_ready());

        // Watch number 3.
        let mut position_info = ring_buffer.watch_clock_recovery_position_info();
        let result = exec.run_until_stalled(&mut position_info);
        assert!(!result.is_ready());
        exec.set_fake_time(fasync::Time::after(zx::Duration::from_millis(201)));
        let _ = exec.wake_expired_timers();
        assert_eq!(2, frames_ready(&mut exec, &mut frame_stream));
        let result = exec.run_until_stalled(&mut position_info);
        assert!(result.is_ready());

        let result = exec.run_until_stalled(&mut ring_buffer.stop());
        assert!(result.is_ready());
    }

    #[fixture(with_audio_frame_stream)]
    #[fuchsia::test]
    fn watch_delay_info(
        mut exec: fasync::TestExecutor,
        stream_config: StreamConfigProxy,
        mut frame_stream: AudioFrameStream,
    ) {
        let mut frame_fut = frame_stream.next();
        // Poll the frame stream, which should start the processing of proxy requests.
        exec.run_until_stalled(&mut frame_fut).expect_pending("no frames at the start");
        let _stream_config_properties = exec.run_until_stalled(&mut stream_config.get_properties());
        let _formats = exec.run_until_stalled(&mut stream_config.get_supported_formats());
        let (ring_buffer, server) = fidl::endpoints::create_proxy::<RingBufferMarker>()
            .expect("creating ring buffer endpoint error");

        #[rustfmt::skip]
        let format = Format {
            pcm_format: Some(fidl_fuchsia_hardware_audio::PcmFormat {
                number_of_channels:      2u8,
                sample_format:           SampleFormat::PcmSigned,
                bytes_per_sample:        2u8,
                valid_bits_per_sample:   16u8,
                frame_rate:              44100,
            }),
            ..Format::EMPTY
        };

        let result = stream_config.create_ring_buffer(format, server);
        assert!(result.is_ok());

        let result = exec.run_until_stalled(&mut ring_buffer.watch_delay_info());

        // Should account for the external_delay here.
        match result {
            Poll::Ready(Ok(DelayInfo { internal_delay: Some(x), .. })) => {
                assert_eq!(150.millis().into_nanos(), x)
            }
            other => panic!("Expected the correct delay info, got {other:?}"),
        }
    }
}
