// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::anyhow;
use fuchsia_audio_device::codec;
use fuchsia_bluetooth::types::{peer_audio_stream_id, PeerId};
use fuchsia_sync::Mutex;
use futures::stream::BoxStream;
use futures::{SinkExt, StreamExt};
use std::sync::Arc;
use tracing::{info, warn};
use {
    fidl_fuchsia_audio_device as audio_device, fidl_fuchsia_hardware_audio as audio,
    fuchsia_async as fasync,
};

use super::{AudioControl, AudioControlEvent, AudioError, HF_INPUT_UUID};
use crate::sco_connector::ScoConnection;
use crate::CodecId;

#[derive(Default)]
struct CodecAudioControlInner {
    start_request:
        Option<Box<dyn FnOnce(std::result::Result<zx::MonotonicInstant, zx::Status>) + Send>>,
    stop_request:
        Option<Box<dyn FnOnce(std::result::Result<zx::MonotonicInstant, zx::Status>) + Send>>,
}

pub struct CodecAudioControl {
    provider: audio_device::ProviderProxy,
    codec_task: Option<fasync::Task<()>>,
    events_sender: futures::channel::mpsc::Sender<AudioControlEvent>,
    events_receiver: Mutex<Option<futures::channel::mpsc::Receiver<AudioControlEvent>>>,
    codec_id: Option<CodecId>,
    connection: Option<ScoConnection>,
    connected_peer: Option<PeerId>,
    inner: Arc<Mutex<CodecAudioControlInner>>,
}

impl AudioControl for CodecAudioControl {
    fn start(
        &mut self,
        id: PeerId,
        connection: ScoConnection,
        codec: crate::features::CodecId,
    ) -> Result<(), AudioError> {
        if self.connection.is_some() {
            return Err(AudioError::AlreadyStarted);
        }
        if Some(codec) != self.codec_id {
            return Err(AudioError::UnsupportedParameters {
                source: anyhow!("CodecId must match connected CodecId"),
            });
        }
        if Some(id) != self.connected_peer {
            return Err(AudioError::UnsupportedParameters {
                source: anyhow!("Can't start a non-connected peer"),
            });
        };
        let Some(start_request) = self.inner.lock().start_request.take() else {
            return Err(AudioError::UnsupportedParameters {
                source: anyhow!("Can only start in response to request"),
            });
        };
        self.connection = Some(connection);
        start_request(Ok(fuchsia_async::MonotonicInstant::now().into()));
        Ok(())
    }

    fn stop(&mut self, id: PeerId) -> Result<(), AudioError> {
        if self.connection.is_none() {
            return Err(AudioError::NotStarted);
        }
        if Some(id) != self.connected_peer {
            return Err(AudioError::UnsupportedParameters {
                source: anyhow!("Can't stop a non-connected peer"),
            });
        }
        let Some(stop_request) = self.inner.lock().stop_request.take() else {
            return Err(AudioError::UnsupportedParameters {
                source: anyhow!("Can only stop in response to request"),
            });
        };
        self.connection = None;
        stop_request(Ok(fuchsia_async::MonotonicInstant::now().into()));
        Ok(())
    }

    fn connect(&mut self, id: PeerId, supported_codecs: &[CodecId]) {
        let supported_formats: audio::DaiSupportedFormats;
        if supported_codecs.contains(&CodecId::MSBC) {
            self.codec_id = Some(CodecId::MSBC);
            supported_formats = CodecId::MSBC.try_into().unwrap();
        } else {
            self.codec_id = Some(CodecId::CVSD);
            supported_formats = CodecId::CVSD.try_into().unwrap();
        };
        let audio_dev_id = peer_audio_stream_id(id, HF_INPUT_UUID);
        let (codec, client) = codec::SoftCodec::create(
            Some(&audio_dev_id),
            "Fuchsia",
            super::DEVICE_NAME,
            codec::CodecDirection::Duplex,
            supported_formats.clone(),
            true,
        );
        self.codec_task = Some(fasync::Task::local(codec_task(
            id,
            self.provider.clone(),
            codec,
            supported_formats,
            client,
            self.events_sender.clone(),
            self.inner.clone(),
        )));
        self.connected_peer = Some(id);
    }

    fn disconnect(&mut self, _id: PeerId) {
        self.codec_task = None;
        self.connected_peer = None;
    }

    fn take_events(&self) -> BoxStream<'static, AudioControlEvent> {
        self.events_receiver.lock().take().unwrap().boxed()
    }

    fn failed_request(&self, request: AudioControlEvent, _error: AudioError) {
        match request {
            AudioControlEvent::RequestStart { id: _ } => {
                let Some(start_request) = self.inner.lock().start_request.take() else {
                    return;
                };
                start_request(Err(zx::Status::INTERNAL));
            }
            AudioControlEvent::RequestStop { id: _ } => {
                let Some(stop_request) = self.inner.lock().start_request.take() else {
                    return;
                };
                stop_request(Err(zx::Status::INTERNAL));
            }
            _ => unreachable!(),
        }
    }
}

async fn codec_task(
    id: PeerId,
    provider: audio_device::ProviderProxy,
    mut codec: codec::SoftCodec,
    supported_formats: audio::DaiSupportedFormats,
    client: fidl::endpoints::ClientEnd<audio::CodecMarker>,
    mut event_sender: futures::channel::mpsc::Sender<AudioControlEvent>,
    inner: Arc<Mutex<CodecAudioControlInner>>,
) {
    let result = provider
        .add_device(audio_device::ProviderAddDeviceRequest {
            device_name: Some(super::DEVICE_NAME.into()),
            device_type: Some(audio_device::DeviceType::Codec),
            driver_client: Some(audio_device::DriverClient::Codec(client)),
            ..Default::default()
        })
        .await;
    match result {
        Err(e) => {
            warn!("FIDL Error adding device: {e:?}");
            return;
        }
        Ok(Err(e)) => {
            warn!("Failed to add device: {e:?}");
            return;
        }
        Ok(Ok(_)) => {}
    };
    info!("Added Codec device!");
    while let Some(event) = codec.next().await {
        let Ok(event) = event else {
            let _ = event_sender
                .send(AudioControlEvent::Stopped {
                    id,
                    error: Some(AudioError::audio_core(event.err().unwrap().into())),
                })
                .await;
            return;
        };
        use fuchsia_audio_device::codec::CodecRequest;
        info!("Codec request: {event:?}");
        let audio_event = match event {
            CodecRequest::SetFormat { format, responder } => {
                if supported_formats.number_of_channels.contains(&format.number_of_channels)
                    && supported_formats.frame_formats.contains(&format.frame_format)
                    && supported_formats.sample_formats.contains(&format.sample_format)
                    && supported_formats.frame_rates.contains(&format.frame_rate)
                    && supported_formats.bits_per_slot.contains(&format.bits_per_slot)
                    && supported_formats.bits_per_sample.contains(&format.bits_per_sample)
                {
                    responder(Ok(()));
                } else {
                    responder(Err(zx::Status::NOT_SUPPORTED));
                }
                continue;
            }
            CodecRequest::Start { responder } => {
                if inner.lock().start_request.is_some() {
                    responder(Err(zx::Status::ALREADY_EXISTS));
                    continue;
                }
                inner.lock().start_request = Some(responder);
                AudioControlEvent::RequestStart { id }
            }
            CodecRequest::Stop { responder } => {
                if inner.lock().stop_request.is_some() {
                    responder(Err(zx::Status::ALREADY_EXISTS));
                    continue;
                }
                inner.lock().stop_request = Some(responder);
                AudioControlEvent::RequestStop { id }
            }
        };
        let _ = event_sender.send(audio_event).await;
    }
    warn!("Codec device finished, dropping..!");
}

impl CodecAudioControl {
    pub fn new(provider: audio_device::ProviderProxy) -> Self {
        let (events_sender, receiver) = futures::channel::mpsc::channel(1);
        Self {
            provider,
            codec_task: None,
            events_sender,
            events_receiver: Mutex::new(Some(receiver)),
            inner: Default::default(),
            codec_id: None,
            connection: None,
            connected_peer: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use fidl::endpoints::Proxy;
    use fixture::fixture;
    use futures::task::{Context, Poll};
    use futures::FutureExt;

    use crate::sco_connector::tests::connection_for_codec;

    async fn codec_setup_connected<F, Fut>(_test_name: &str, test: F)
    where
        F: FnOnce(audio::CodecProxy, CodecAudioControl) -> Fut,
        Fut: futures::Future<Output = ()>,
    {
        let (provider_proxy, mut provider_requests) =
            fidl::endpoints::create_proxy_and_stream::<audio_device::ProviderMarker>().unwrap();
        let mut codec = CodecAudioControl::new(provider_proxy);

        codec.connect(PeerId(1), &[CodecId::MSBC]);

        let Some(Ok(audio_device::ProviderRequest::AddDevice {
            payload:
                audio_device::ProviderAddDeviceRequest {
                    driver_client: Some(client),
                    device_name: Some(_name),
                    device_type: Some(device_type),
                    ..
                },
            responder,
        })) = provider_requests.next().await
        else {
            panic!("Expected a request from the connect");
        };

        assert_eq!(device_type, audio_device::DeviceType::Codec);

        responder.send(Ok(&Default::default())).expect("response to succeed");

        let audio_device::DriverClient::Codec(codec_client) = client else {
            panic!("Should have provided a codec client");
        };

        let codec_proxy = codec_client.into_proxy();

        test(codec_proxy, codec).await
    }

    #[fixture(codec_setup_connected)]
    #[fuchsia::test]
    async fn publishes_on_connect(codec_client: audio::CodecProxy, codec: CodecAudioControl) {
        let _properties = codec_client.get_properties().await.unwrap();
        let audio::CodecGetDaiFormatsResult::Ok(formats) =
            codec_client.get_dai_formats().await.unwrap()
        else {
            panic!("Expected formats from get_dai_formats");
        };

        assert_eq!(formats.len(), 1);
        // MSBC has a frame-rate of 16khz
        assert_eq!(formats[0].frame_rates[0], 16000);
        drop(codec);
    }

    #[fixture(codec_setup_connected)]
    #[fuchsia::test]
    async fn removed_on_disconnect(codec_client: audio::CodecProxy, mut codec: CodecAudioControl) {
        codec.disconnect(PeerId(1));
        let _ = codec_client.on_closed().await;
    }

    #[fixture(codec_setup_connected)]
    #[fuchsia::test]
    async fn start_request_lifetime(codec_client: audio::CodecProxy, mut codec: CodecAudioControl) {
        let mut event_stream = codec.take_events();
        // start without a request should fail
        let (connection, _stream) = connection_for_codec(PeerId(1), CodecId::MSBC, false);
        let start_result = codec.start(PeerId(1), connection, CodecId::MSBC);
        let Err(AudioError::UnsupportedParameters { .. }) = start_result else {
            panic!("Expected error from start before request");
        };

        // request comes in
        let mut start_fut = codec_client.start();
        let (waker, wake_count) = futures_test::task::new_count_waker();
        let Poll::Pending = start_fut.poll_unpin(&mut Context::from_waker(&waker)) else {
            panic!("Expected start to be pending");
        };

        let Some(AudioControlEvent::RequestStart { id }) = event_stream.next().await else {
            panic!("Expected start request from event stream");
        };
        assert_eq!(id, PeerId(1));

        // starting with a non-MSBC codec fails, and doesn't complete the future.
        let (connection, _stream) = connection_for_codec(PeerId(1), CodecId::CVSD, false);
        let start_result = codec.start(PeerId(1), connection, CodecId::CVSD);
        let Err(AudioError::UnsupportedParameters { .. }) = start_result else {
            panic!("Expected error from start before request");
        };
        assert_eq!(wake_count.get(), 0);

        // starting after works and then completes the request.
        let (connection, _stream) = connection_for_codec(PeerId(1), CodecId::MSBC, false);
        codec.start(PeerId(1), connection, CodecId::MSBC).expect("should start ok");

        let Poll::Ready(_) = start_fut.poll_unpin(&mut Context::from_waker(&waker)) else {
            panic!("Expected to get response back from start");
        };

        // Starting after started is no good either.
        let (connection, _stream) = connection_for_codec(PeerId(1), CodecId::MSBC, false);
        let start_result = codec.start(PeerId(1), connection, CodecId::MSBC);
        let Err(AudioError::AlreadyStarted) = start_result else {
            panic!("Expected error from start while started");
        };
    }

    #[fixture(codec_setup_connected)]
    #[fuchsia::test]
    async fn stop_request_lifetime(codec_client: audio::CodecProxy, mut codec: CodecAudioControl) {
        let mut event_stream = codec.take_events();
        // can't stop before we are started
        let Err(AudioError::NotStarted) = codec.stop(PeerId(1)) else {
            panic!("Expected to not be able tp start when stopped");
        };

        // request comes in
        let start_fut = codec_client.start();

        let Some(AudioControlEvent::RequestStart { .. }) = event_stream.next().await else {
            panic!("Expected start request from event stream");
        };

        // starting after works and then completes the request.
        let (connection, _stream) = connection_for_codec(PeerId(1), CodecId::MSBC, false);
        codec.start(PeerId(1), connection, CodecId::MSBC).expect("should start ok");
        let _ = start_fut.await.expect("start to succeed");

        // can't stop without a request
        let Err(AudioError::UnsupportedParameters { .. }) = codec.stop(PeerId(1)) else {
            panic!("expected to not be able to stop without a request");
        };

        // request to stop comes in
        let mut stop_fut = codec_client.stop();
        let (waker, _wake_count) = futures_test::task::new_count_waker();
        let Poll::Pending = stop_fut.poll_unpin(&mut Context::from_waker(&waker)) else {
            panic!("Expected stop to be pending");
        };

        let Some(AudioControlEvent::RequestStop { id }) = event_stream.next().await else {
            panic!("Expected stop request from event stream");
        };
        assert_eq!(id, PeerId(1));

        // Can't stop a peer that's not started.
        let _ = codec.stop(PeerId(2)).expect_err("shouldn't be able to stop a different peer");

        // can stop the one requested
        codec.stop(PeerId(1)).expect("should be able to stop");

        let Poll::Ready(_) = stop_fut.poll_unpin(&mut Context::from_waker(&waker)) else {
            panic!("Expected to get response back from start");
        };
        // back to being able to not stop it again

        let Err(AudioError::NotStarted) = codec.stop(PeerId(1)) else {
            panic!("Expected to not be able tp start when stopped");
        };
    }
}
