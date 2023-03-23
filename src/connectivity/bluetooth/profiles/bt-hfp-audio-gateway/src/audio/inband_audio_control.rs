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
use fuchsia_bluetooth::types::{peer_audio_stream_id, PeerId, Uuid};
use futures::{task::Context, FutureExt};
use media::AudioDeviceEnumeratorProxy;
use tracing::{info, warn};

use super::*;

pub struct InbandAudioControl {
    audio_core: media::AudioDeviceEnumeratorProxy,
    session_task: Option<fasync::Task<()>>,
}

struct AudioSession {
    sco: ScoConnection,
}

impl AudioSession {
    fn setup(connection: bredr::ScoConnectionProxy, codec: CodecId) -> Self {
        todo!()
    }

    fn start(self) -> Result<fasync::Task<()>, AudioError> {
        todo!()
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
        self.session_task = Some(AudioSession { sco: connection }.start()?);
        Ok(())
    }

    fn stop(&mut self) -> Result<(), AudioError> {
        todo!()
    }
}
