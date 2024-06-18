// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::anyhow;
use fidl_fuchsia_hardware_audio::CodecMarker;
use fuchsia_audio_device::codec;
use fuchsia_bluetooth::types::{peer_audio_stream_id, PeerId};
use futures::StreamExt;
use tracing::{info, warn};
use {fidl_fuchsia_audio_device as audio_device, fuchsia_async as fasync};

use super::{AudioControl, AudioError, HF_INPUT_UUID};
use crate::CodecId;

pub struct CodecAudioControl {
    _provider: audio_device::ProviderProxy,
    _codec_task: fasync::Task<()>,
}

impl AudioControl for CodecAudioControl {
    fn start(
        &mut self,
        _id: fuchsia_bluetooth::types::PeerId,
        _connection: crate::sco_connector::ScoConnection,
        _codec: crate::features::CodecId,
    ) -> Result<(), AudioError> {
        Err(AudioError::UnsupportedParameters { source: anyhow!("Not implemented") })
    }

    fn stop(&mut self) -> Result<(), AudioError> {
        Err(AudioError::NotStarted)
    }
}

async fn codec_task(
    provider: audio_device::ProviderProxy,
    mut codec: codec::SoftCodec,
    client: fidl::endpoints::ClientEnd<CodecMarker>,
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
    while let Some(event) = codec.next().await {
        let Ok(event) = event else {
            // Report error from event.err()
            return;
        };
        use fuchsia_audio_device::codec::CodecRequest;
        info!("Codec request: {event:?}");
        match event {
            CodecRequest::SetFormat { format: _, responder } => {
                responder(Ok(()));
            }
            CodecRequest::Start { responder } => {
                responder(Ok(fasync::Time::now().into()));
            }
            CodecRequest::Stop { responder } => {
                responder(Ok(fasync::Time::now().into()));
            }
        }
    }
}

impl CodecAudioControl {
    #[allow(unused)]
    fn new(provider: audio_device::ProviderProxy) -> Self {
        Self { _provider: provider, _codec_task: fasync::Task::spawn(futures::future::ready(())) }
    }

    #[allow(unused)]
    fn setup(&mut self, peer_id: PeerId, codec_id: CodecId) -> Result<(), AudioError> {
        let audio_dev_id = peer_audio_stream_id(peer_id, HF_INPUT_UUID);
        let (codec, client) = codec::SoftCodec::create(
            Some(&audio_dev_id),
            "Fuchsia",
            super::DEVICE_NAME,
            codec::CodecDirection::Duplex,
            codec_id.try_into()?,
            true,
        );

        self._codec_task = fasync::Task::local(codec_task(self._provider.clone(), codec, client));
        // TODO(b/341116731): Send errors back from setup or start requests.
        Ok(())
    }
}
