// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use bt_a2dp::codec::MediaCodecConfig;
use bt_a2dp::media_task::*;
use bt_avdtp::MediaStream;
use fidl_fuchsia_bluetooth_bredr::AudioOffloadExtProxy;
use fidl_fuchsia_media::{AudioChannelId, AudioPcmMode, PcmFormat};
use fuchsia_bluetooth::inspect::DataStreamInspect;
use fuchsia_bluetooth::types::PeerId;
use fuchsia_inspect::Node;
use fuchsia_inspect_derive::{AttachError, Inspect};
use futures::channel::oneshot;
use futures::future::{BoxFuture, Shared, WeakShared};
use futures::{AsyncWriteExt, FutureExt, TryFutureExt, TryStreamExt};
use log::{info, trace, warn};
use std::time::Duration;
use {fuchsia_async as fasync, fuchsia_trace as trace};

use super::sources;
use crate::encoding::EncodedStream;

/// Builder is a MediaTaskBuilder will build `ConfiguredTask`s when configured.
/// `source_type` determines where the source of audio is provided.
/// `aac_available` determines whether AAC is advertised as supported.
/// When configured, a test stream is created to confirm that it is possible to stream audio using
/// the configuration.  This stream is discarded and the stream is restarted when the resulting
/// `ConfiguredTask` is started.
/// TODO(https://fxbug.dev/42145257): Avoid this creation / destruction on configure
#[derive(Clone)]
pub struct Builder {
    /// The type of source audio.
    source_type: sources::AudioSourceType,
    /// Whether AAC has been detected to be available
    aac_available: bool,
}

fn build_sbc_source() -> MediaCodecConfig {
    use bt_a2dp::media_types::*;
    let sbc_codec_info = SbcCodecInfo::new(
        SbcSamplingFrequency::FREQ48000HZ,
        SbcChannelMode::JOINT_STEREO,
        SbcBlockCount::MANDATORY_SRC,
        SbcSubBands::MANDATORY_SRC,
        SbcAllocation::MANDATORY_SRC,
        SbcCodecInfo::BITPOOL_MIN,
        51, // Recommended bitpool value for 48khz Joint Stereo High Quality acccording to A2DP 1.4 Table 4.7
    )
    .unwrap();

    let codec_cap = bt_avdtp::ServiceCapability::MediaCodec {
        media_type: bt_avdtp::MediaType::Audio,
        codec_type: bt_avdtp::MediaCodecType::AUDIO_SBC,
        codec_extra: sbc_codec_info.to_bytes().to_vec(),
    };

    (&codec_cap).try_into().unwrap()
}

fn build_aac_source(bitrate: u32) -> MediaCodecConfig {
    use bt_a2dp::media_types::*;
    let codec_info = AacCodecInfo::new(
        AacObjectType::MANDATORY_SRC,
        AacSamplingFrequency::FREQ48000HZ,
        AacChannels::TWO,
        true,
        bitrate,
    )
    .unwrap();
    (&bt_avdtp::ServiceCapability::MediaCodec {
        media_type: bt_avdtp::MediaType::Audio,
        codec_type: bt_avdtp::MediaCodecType::AUDIO_AAC,
        codec_extra: codec_info.to_bytes().to_vec(),
    })
        .try_into()
        .unwrap()
}

impl MediaTaskBuilder for Builder {
    fn configure(
        &self,
        peer_id: &PeerId,
        codec_config: &MediaCodecConfig,
    ) -> Result<Box<dyn MediaTaskRunner>, MediaTaskError> {
        let res = self.configure_task(peer_id, codec_config);
        Ok::<Box<dyn MediaTaskRunner>, _>(Box::new(res?))
    }

    fn direction(&self) -> bt_avdtp::EndpointType {
        bt_avdtp::EndpointType::Source
    }

    fn supported_configs(
        &self,
        _peer_id: &PeerId,
        _offload: Option<AudioOffloadExtProxy>,
    ) -> BoxFuture<'static, Result<Vec<MediaCodecConfig>, MediaTaskError>> {
        // SBC is required to be supported to use this Builder
        let media_configs = if self.aac_available {
            vec![build_aac_source(crate::MAX_BITRATE_AAC), build_sbc_source()]
        } else {
            vec![build_sbc_source()]
        };
        futures::future::ready(Ok(media_configs)).boxed()
    }
}

impl Builder {
    /// Make a new builder that will source audio from `source_type`.  See `sources::build_stream`
    /// for documentation on the types of streams that are available.
    pub fn new(source_type: sources::AudioSourceType, aac_available: bool) -> Self {
        Self { source_type, aac_available }
    }

    pub(crate) fn configure_task(
        &self,
        peer_id: &PeerId,
        codec_config: &MediaCodecConfig,
    ) -> Result<ConfiguredTask, MediaTaskError> {
        let channel_map = match codec_config.channel_count() {
            Ok(1) => vec![AudioChannelId::Cf],
            Ok(2) => vec![AudioChannelId::Lf, AudioChannelId::Rf],
            Ok(_) | Err(_) => return Err(MediaTaskError::NotSupported),
        };
        let pcm_format = PcmFormat {
            pcm_mode: AudioPcmMode::Linear,
            bits_per_sample: 16,
            frames_per_second: codec_config.sampling_frequency()?,
            channel_map,
        };
        let source_stream = self
            .source_type
            .build(&peer_id, pcm_format.clone(), Duration::ZERO, &mut Default::default())
            .map_err(|_e| MediaTaskError::NotSupported)?;
        if let Err(e) = EncodedStream::build(pcm_format.clone(), source_stream, codec_config) {
            trace!("inband_source::Builder: can't build encoded stream: {e:?}");
            return Err(MediaTaskError::Other(format!("Can't build encoded stream: {e}")));
        }
        Ok(ConfiguredTask::build(pcm_format, self.source_type, peer_id.clone(), codec_config))
    }
}

/// Provides audio from this to the MediaStream when started.  Streams are created and started when
/// this task is started, and destroyed when stopped.
pub(crate) struct ConfiguredTask {
    /// The type of source audio.
    source_type: sources::AudioSourceType,
    /// Format the source audio should be produced in.
    pub(crate) pcm_format: PcmFormat,
    /// Id of the peer that will be receiving the stream.  Used to distinguish sources for Fuchsia
    /// Media.
    peer_id: PeerId,
    /// Configuration providing the format of encoded audio requested by the peer.
    codec_config: MediaCodecConfig,
    /// Delay reported from the peer. Defaults to zero. Passed on to the Audio source.
    delay: Duration,
    /// Future if the task is running or has ran and and the shared future has not been dropped.
    /// Used to indicate errors for set_delay as we currently do not support updating delays dynamically.
    running: Option<WeakShared<BoxFuture<'static, Result<(), MediaTaskError>>>>,
    /// Inspect node
    inspect: fuchsia_inspect::Node,
}

impl ConfiguredTask {
    /// Build a new ConfiguredTask.  Usually only called by Builder.
    /// `ConfiguredTask::start` will only return errors if the settings here cannot produce a
    /// stream.  No checks are done when building.
    pub(crate) fn build(
        pcm_format: PcmFormat,
        source_type: sources::AudioSourceType,
        peer_id: PeerId,
        codec_config: &MediaCodecConfig,
    ) -> Self {
        Self {
            pcm_format,
            source_type,
            peer_id,
            codec_config: codec_config.clone(),
            delay: Duration::ZERO,
            running: None,
            inspect: Default::default(),
        }
    }

    fn update_inspect(&self) {
        self.inspect.record_string("source_type", &format!("{}", self.source_type));
        self.inspect.record_string("codec_config", &format!("{:?}", self.codec_config));
    }
}

impl Inspect for &mut ConfiguredTask {
    fn iattach(
        self,
        parent: &fuchsia_inspect::Node,
        name: impl AsRef<str>,
    ) -> Result<(), AttachError> {
        self.inspect = parent.create_child(name.as_ref());
        self.update_inspect();
        Ok(())
    }
}

impl MediaTaskRunner for ConfiguredTask {
    fn start(
        &mut self,
        stream: MediaStream,
        _offload: Option<AudioOffloadExtProxy>,
    ) -> Result<Box<dyn MediaTask>, MediaTaskError> {
        let source_stream = self
            .source_type
            .build(&self.peer_id, self.pcm_format.clone(), self.delay.into(), &mut self.inspect)
            .map_err(|e| MediaTaskError::Other(format!("Building stream: {}", e)))?;
        let encoded_stream =
            EncodedStream::build(self.pcm_format.clone(), source_stream, &self.codec_config)
                .map_err(|e| MediaTaskError::Other(format!("Can't build encoded stream: {}", e)))?;
        let mut data_stream_inspect = DataStreamInspect::default();
        let _ = data_stream_inspect.iattach(&self.inspect, "data_stream");
        let stream_task = RunningTask::build(
            self.codec_config.clone(),
            encoded_stream,
            stream,
            data_stream_inspect,
        );
        self.running = stream_task.result_fut.downgrade();
        Ok(Box::new(stream_task))
    }

    fn set_delay(&mut self, delay: Duration) -> Result<(), MediaTaskError> {
        if let Some(fut) = self.running.as_ref().and_then(WeakShared::upgrade) {
            // If the Shared isn't done, we are still running and can't update the delay.
            if fut.now_or_never().is_none() {
                return Err(MediaTaskError::NotSupported);
            }
        }
        self.delay = delay;
        Ok(())
    }

    /// the running media task to the tree (i.e. data transferred, jitter, etc)
    fn iattach(&mut self, parent: &Node, name: &str) -> Result<(), AttachError> {
        fuchsia_inspect_derive::Inspect::iattach(self, parent, name)
    }
}

struct RunningTask {
    stream_task: Option<fasync::Task<()>>,
    result_fut: Shared<BoxFuture<'static, Result<(), MediaTaskError>>>,
}

impl RunningTask {
    /// The main streaming task. Reads encoded audio from the encoded_stream and packages into RTP
    /// packets, sending the resulting RTP packets using `media_stream`.
    async fn stream_task(
        codec_config: MediaCodecConfig,
        mut encoded_stream: EncodedStream,
        mut media_stream: MediaStream,
        mut data_stream_inspect: DataStreamInspect,
    ) -> Result<(), Error> {
        data_stream_inspect.start();
        let frames_per_encoded = codec_config.pcm_frames_per_encoded_frame() as u32;
        let max_tx_size = media_stream.max_tx_size()?;
        let mut packet_builder = codec_config.make_packet_builder(max_tx_size)?;
        loop {
            let encoded = match encoded_stream.try_next().await? {
                None => continue,
                Some(encoded) => encoded,
            };
            let packets = match packet_builder.add_frame(encoded, frames_per_encoded) {
                Err(e) => {
                    warn!("Can't add packet to RTP packet: {:?}", e);
                    continue;
                }
                Ok(packets) => packets,
            };

            for packet in packets {
                trace::duration_begin!(c"bt-a2dp", c"Media:PacketSent");
                if let Err(e) = media_stream.write(&packet).await {
                    info!("Failed sending packet to peer: {}", e);
                    trace::duration_end!(c"bt-a2dp", c"Media:PacketSent");
                    return Ok(());
                }
                data_stream_inspect
                    .record_transferred(packet.len(), fasync::MonotonicInstant::now());
                trace::duration_end!(c"bt-a2dp", c"Media:PacketSent");
            }
        }
    }

    fn build(
        codec_config: MediaCodecConfig,
        encoded_stream: EncodedStream,
        media_stream: MediaStream,
        inspect: DataStreamInspect,
    ) -> Self {
        let (sender, receiver) = oneshot::channel();
        let stream_task_fut =
            Self::stream_task(codec_config, encoded_stream, media_stream, inspect);
        let wrapped_task = fasync::Task::spawn(async move {
            trace::instant!(c"bt-a2dp", c"Media:Start", trace::Scope::Thread);
            let result = stream_task_fut
                .await
                .map_err(|e| MediaTaskError::Other(format!("Error in streaming audio: {}", e)));
            let _ = sender.send(result);
        });
        let result_fut = receiver.map_ok_or_else(|_err| Ok(()), |result| result).boxed().shared();
        Self { stream_task: Some(wrapped_task), result_fut }
    }
}

impl MediaTask for RunningTask {
    fn finished(&mut self) -> BoxFuture<'static, Result<(), MediaTaskError>> {
        self.result_fut.clone().boxed()
    }

    fn stop(&mut self) -> Result<(), MediaTaskError> {
        if let Some(task) = self.stream_task.take() {
            trace::instant!(c"bt-a2dp", c"Media:Stopped", trace::Scope::Thread);
            drop(task);
        }
        // Either a result already happened, or we will just have sent an Ok(()) by dropping the result
        // sender
        self.result().unwrap_or(Ok(()))
    }
}

#[cfg(all(test, feature = "test_encoding"))]
mod tests {
    use super::*;

    use bt_a2dp::media_types::*;
    use bt_avdtp::MediaCodecType;
    use fuchsia_bluetooth::types::Channel;
    use fuchsia_inspect as inspect;
    use fuchsia_sync::Mutex;
    use futures::StreamExt;
    use std::sync::{Arc, RwLock};
    use test_util::assert_gt;

    #[fuchsia::test]
    fn configures_source_from_codec_config() {
        let _exec = fasync::TestExecutor::new();
        let builder = Builder::new(sources::AudioSourceType::BigBen, false);

        // Minimum SBC requirements are mono, 48kHz
        let mono_config = MediaCodecConfig::min_sbc();
        let task = builder.configure_task(&PeerId(1), &mono_config).expect("should build okay");
        assert_eq!(48000, task.pcm_format.frames_per_second);
        assert_eq!(1, task.pcm_format.channel_map.len());

        // A standard SBC audio config which is stereo and 44.1kHz
        let sbc_codec_info = SbcCodecInfo::new(
            SbcSamplingFrequency::FREQ44100HZ,
            SbcChannelMode::JOINT_STEREO,
            SbcBlockCount::SIXTEEN,
            SbcSubBands::EIGHT,
            SbcAllocation::LOUDNESS,
            SbcCodecInfo::BITPOOL_MIN,
            SbcCodecInfo::BITPOOL_MAX,
        )
        .unwrap();
        let stereo_config =
            MediaCodecConfig::build(MediaCodecType::AUDIO_SBC, &sbc_codec_info.to_bytes().to_vec())
                .unwrap();

        let task = builder.configure_task(&PeerId(1), &stereo_config).expect("should build okay");
        assert_eq!(44100, task.pcm_format.frames_per_second);
        assert_eq!(2, task.pcm_format.channel_map.len());
    }

    #[fuchsia::test]
    fn source_media_stream_stats() {
        let mut exec = fasync::TestExecutor::new();
        let builder = Builder::new(sources::AudioSourceType::BigBen, false);

        let inspector = inspect::component::inspector();
        let root = inspector.root();

        // Minimum SBC requirements are mono, 48kHz
        let mono_config = MediaCodecConfig::min_sbc();
        let mut task = builder.configure_task(&PeerId(1), &mono_config).expect("should build okay");
        MediaTaskRunner::iattach(&mut task, &root, "source_task").expect("should attach okay");

        let (mut remote, local) = Channel::create();
        let local = Arc::new(RwLock::new(local));
        let weak_local = Arc::downgrade(&local);
        let stream = MediaStream::new(Arc::new(Mutex::new(true)), weak_local);

        let _running_task = task.start(stream, None).expect("media should start");

        let _ = exec.run_singlethreaded(remote.next()).expect("some packet");

        let hierarchy =
            exec.run_singlethreaded(inspect::reader::read(inspector)).expect("got hierarchy");

        // We don't know exactly how many were sent at this point, but make sure we got at
        // least some recorded.
        let total_bytes = hierarchy
            .get_property_by_path(&vec!["source_task", "data_stream", "total_bytes"])
            .expect("missing property");
        assert_gt!(total_bytes.uint().expect("uint"), 0);

        let bytes_per_second_current = hierarchy
            .get_property_by_path(&vec!["source_task", "data_stream", "bytes_per_second_current"])
            .expect("missing property");
        assert_gt!(bytes_per_second_current.uint().expect("uint"), 0);
    }
}
