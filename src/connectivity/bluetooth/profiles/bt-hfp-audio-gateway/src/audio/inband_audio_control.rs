// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(fxbug.dev/122267): remove when we add this
#![allow(unused)]

use anyhow::format_err;
use fidl_fuchsia_bluetooth_bredr as bredr;
use fidl_fuchsia_hardware_audio::{self as audio, PcmFormat};
use fidl_fuchsia_media as media;
use fuchsia_async as fasync;
use fuchsia_audio_codec::{StreamProcessor, StreamProcessorOutputStream};
use fuchsia_bluetooth::types::{peer_audio_stream_id, PeerId, Uuid};
use futures::{future::Either, pin_mut, task::Context, AsyncWriteExt, FutureExt, StreamExt};
use media::AudioDeviceEnumeratorProxy;
use tracing::{info, warn};

use super::*;

pub struct InbandAudioControl {
    audio_core: media::AudioDeviceEnumeratorProxy,
    session_task: Option<fasync::Task<()>>,
}

struct AudioSession {
    sco: ScoConnection,
    decoder: StreamProcessor,
}

impl AudioSession {
    fn setup(connection: ScoConnection, codec: CodecId) -> Result<Self, AudioError> {
        // TODO(https://fxbug.dev/122273): make audio input driver
        // TODO(https://fxbug.dev/122268): make audio output driver
        if !codec.is_supported() {
            return Err(AudioError::UnsupportedParameters {
                source: format_err!("unsupported codec {codec}"),
            });
        }
        let decoder = StreamProcessor::create_decoder(codec.mime_type(), Some(codec.oob_bytes()))
            .map_err(|e| {
            AudioError::audio_core(format_err!("issue creating decoder: {e:?}"))
        })?;
        Ok(Self { sco: connection, decoder })
    }

    async fn write_task(proxy: bredr::ScoConnectionProxy) -> AudioError {
        // TODO(https://fxbug.dev/122268): Add the H2 Header to the packets if mSBC coding
        // TODO(https://fxbug.dev/122268): get audio from audio driver
        // TODO(https://fxbug.dev/122269): send audio to the peer
        loop {
            fuchsia_async::Timer::new(fuchsia_async::Time::INFINITE).await;
        }
    }

    async fn decoder_output_task(mut decoded_stream: StreamProcessorOutputStream) -> AudioError {
        let mut decoded_packets = 0;
        loop {
            match decoded_stream.next().await {
                Some(Ok(decoded)) => {
                    decoded_packets += 1;
                    if decoded_packets % 500 == 0 {
                        info!(
                            "Got {} decoded bytes from decoder: {decoded_packets} packets",
                            decoded.len()
                        );
                    }
                    // TODO(https://fxbug.dev/122273): send audio to the audio driver
                }
                Some(Err(e)) => {
                    warn!("Error in decoding: {e:?}");
                    return AudioError::audio_core(format_err!("Couldn't read decoder: {e:?}"));
                }
                None => {
                    warn!("Error in decoding: Stream is ended!");
                    return AudioError::audio_core(format_err!("Decoder stream ended early"));
                }
            }
        }
    }

    async fn read_task(
        proxy: bredr::ScoConnectionProxy,
        mut decoder: StreamProcessor,
    ) -> AudioError {
        let Ok(decoded_stream) = decoder.take_output_stream() else {
            return AudioError::audio_core(format_err!("Couldn't take decoder output stream"));
        };
        let _decoded_stream_task =
            fasync::Task::spawn(AudioSession::decoder_output_task(decoded_stream));
        let mut total_bytes = 0;
        loop {
            let (packet_status, data) = match proxy.read().await {
                Ok(x) => x,
                Err(e) => return e.into(),
            };
            // H2 Header (two octets) is present on packets when WBS is used
            let (header, packet) = data.as_slice().split_at(2);
            if packet[0] != 0xad {
                info!("Packet didn't start with syncword: {:#02x} {}", packet[0], packet.len());
            }
            // Only log once every 60k to reduce spam
            if total_bytes % 60_000 == 0 {
                // TODO(fxbug.dev/122270): verify that the H2 header sequence is correct
                info!("Header was {:#02x} {:#02x}", header[0], header[1]);
                info!(?packet_status, "Read {} bytes ({total_bytes} total) from SCO", data.len());
            }
            total_bytes += data.len();
            if let Err(e) = decoder.write_all(packet).await {
                return AudioError::audio_core(format_err!("Failed to write to decoder: {e:?}"));
            }
            // TODO(fxbug.dev/122271): buffer some packets before flushing instead of flushing on
            // every one.
            if let Err(e) = decoder.flush().await {
                return AudioError::audio_core(format_err!("Failed to flush decoder: {e:?}"));
            }
        }
    }

    async fn run(self) {
        let write = AudioSession::write_task(self.sco.proxy.clone());
        pin_mut!(write);
        let read = AudioSession::read_task(self.sco.proxy.clone(), self.decoder);
        pin_mut!(read);

        match futures::future::select(write, read).await {
            Either::Left((e, _)) => warn!("Error on SCO write path: {e:?}"),
            Either::Right((e, _)) => warn!("Error on SCO read path: {e:?}"),
        }
    }

    fn start(self) -> fasync::Task<()> {
        fasync::Task::spawn(self.run())
    }
}

impl InbandAudioControl {
    pub fn create(proxy: AudioDeviceEnumeratorProxy) -> Result<Self, AudioError> {
        Ok(Self { audio_core: proxy, session_task: None })
    }

    fn is_running(&mut self) -> bool {
        if let Some(task) = self.session_task.as_mut() {
            let mut cx = Context::from_waker(futures::task::noop_waker_ref());
            return task.poll_unpin(&mut cx).is_pending();
        }
        false
    }
}

impl AudioControl for InbandAudioControl {
    fn start(
        &mut self,
        id: PeerId,
        connection: ScoConnection,
        codec: CodecId,
    ) -> Result<(), AudioError> {
        if self.is_running() {
            return Err(AudioError::AlreadyStarted);
        }
        let session = AudioSession::setup(connection, codec)?;
        self.session_task = Some(session.start());
        Ok(())
    }

    fn stop(&mut self) -> Result<(), AudioError> {
        match self.session_task.take() {
            Some(_) => Ok(()),
            None => Err(AudioError::NotStarted),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use fidl_fuchsia_bluetooth_bredr::ScoConnectionRequestStream;
    use futures::StreamExt;

    use crate::sco_connector::tests::connection_for_codec;

    const ZERO_DATA_PACKET: [u8; 60] = [0; 60];

    // Processes one sco request.  Returns true if the stream was ended.
    async fn process_sco_request(sco_request_stream: &mut ScoConnectionRequestStream) -> bool {
        match sco_request_stream.next().await {
            Some(Ok(bredr::ScoConnectionRequest::Read { responder })) => responder
                .send(bredr::RxPacketStatus::CorrectlyReceivedData, &ZERO_DATA_PACKET)
                .expect("sends okay"),
            Some(Ok(bredr::ScoConnectionRequest::Write { responder, data })) => {
                responder.send().expect("response to write")
            }
            None => return true,
            x => panic!("Expected read or write requests, got {x:?}"),
        }
        false
    }

    #[fuchsia::test]
    async fn reads_audio_from_connection() {
        let (proxy, _audio_enumerator_requests) =
            fidl::endpoints::create_proxy_and_stream::<media::AudioDeviceEnumeratorMarker>()
                .unwrap();
        let mut control = InbandAudioControl::create(proxy).unwrap();

        let (connection, mut sco_request_stream) = connection_for_codec(CodecId::MSBC, true);

        control.start(PeerId(1), connection, CodecId::MSBC).expect("should be able to start");

        let (connection2, _request_stream) = connection_for_codec(CodecId::MSBC, true);
        let _ = control
            .start(PeerId(1), connection2, CodecId::MSBC)
            .expect_err("Starting twice shouldn't be allowed");

        for _ in 1..1_000 {
            assert!(!process_sco_request(&mut sco_request_stream).await);
        }

        control.stop().expect("should be able to stop");
        let _ = control.stop().expect_err("can't stop a stopped thing");

        // Should be able to drain the requests.
        let mut extra_requests = 0;
        loop {
            if process_sco_request(&mut sco_request_stream).await {
                break;
            }
            extra_requests += 1;
        }

        info!("Got {extra_requests} extra ScoConnectionProxy Requests after stop");
    }
}
