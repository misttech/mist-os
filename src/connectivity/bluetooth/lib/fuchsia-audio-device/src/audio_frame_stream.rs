// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_inspect as inspect;
use fuchsia_inspect_derive::{AttachError, Inspect};
use futures::{
    stream::FusedStream,
    task::{Context, Poll},
    FutureExt, Stream,
};
use parking_lot::Mutex;
use std::{pin::Pin, sync::Arc};
use tracing::info;

use crate::driver::{PcmOrTask, SoftPcm};
use crate::frame_vmo;
use crate::types::{Error, Result};

/// A stream that produces audio frames.
/// Frames are of constant length.
/// Usually acquired via SoftPcm::create_output()
pub struct AudioFrameStream {
    /// Handle to the VMO that is receiving the frames.
    frame_vmo: Arc<Mutex<frame_vmo::FrameVmo>>,
    /// The next frame number we should retrieve.
    next_frame: usize,
    /// Number of frames to return in a packet.
    packet_frames: usize,
    /// Vector that will be filled with a packet.
    /// Replaced when stream produces a packet.
    next_packet: std::cell::RefCell<Vec<u8>>,
    /// SoftPcm this is attached to, or the SoftPcm::process_requests task
    pcm: PcmOrTask,
    /// Inspect node
    inspect: inspect::Node,
}

impl AudioFrameStream {
    pub fn new(pcm: SoftPcm) -> AudioFrameStream {
        AudioFrameStream {
            frame_vmo: pcm.frame_vmo(),
            next_frame: 0,
            packet_frames: pcm.packet_frames(),
            next_packet: Vec::new().into(),
            pcm: PcmOrTask::Pcm(pcm),
            inspect: Default::default(),
        }
    }

    /// Start the requests task if not started, and poll the task.
    fn poll_task(&mut self, cx: &mut Context<'_>) -> Poll<Result<()>> {
        if let PcmOrTask::Complete = &self.pcm {
            return Poll::Ready(Err(Error::InvalidState));
        }
        if let PcmOrTask::Task(ref mut task) = &mut self.pcm {
            return task.poll_unpin(cx);
        }
        self.pcm.start();
        self.poll_task(cx)
    }
}

impl Stream for AudioFrameStream {
    type Item = Result<Vec<u8>>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Poll::Ready(r) = self.poll_task(cx) {
            self.pcm = PcmOrTask::Complete;
            return Poll::Ready(r.err().map(Result::Err));
        }
        if self.next_packet.borrow().len() == 0 {
            if let Some(new_len) = self.frame_vmo.lock().bytecount_frames(self.packet_frames) {
                self.next_packet.borrow_mut().resize(new_len, 0);
            }
        }
        let result = {
            let mut lock = self.frame_vmo.lock();
            futures::ready!(lock.poll_read(
                self.next_frame,
                self.next_packet.borrow_mut().as_mut_slice(),
                cx
            ))
        };

        match result {
            Ok((next_frame, missed)) => {
                if missed > 0 {
                    info!("Missed {missed} frames due to slow polling");
                }
                self.next_frame = next_frame;
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
        match self.pcm {
            PcmOrTask::Complete => true,
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
        if let PcmOrTask::Pcm(ref mut o) = &mut self.pcm {
            return o.iattach(&self.inspect, "soft_pcm");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use async_utils::PollExt;
    use fidl_fuchsia_hardware_audio::StreamConfigProxy;
    use fidl_fuchsia_hardware_audio::*;
    use fixture::fixture;
    use fuchsia_async as fasync;
    use fuchsia_zircon as zx;
    use futures::{task::Poll, StreamExt};

    use crate::driver::tests::with_audio_frame_stream;

    const TEST_UNIQUE_ID: &[u8; 16] = &[5; 16];
    const TEST_CLOCK_DOMAIN: u32 = 0x00010203;

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
            ..Default::default()
        };

        stream_config.create_ring_buffer(&format, server).expect("ring buffer error");

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
}
