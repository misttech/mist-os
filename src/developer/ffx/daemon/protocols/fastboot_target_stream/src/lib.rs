// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use ffx_daemon_target::FASTBOOT_CHECK_INTERVAL;
use ffx_fastboot::common::find::find_serial_numbers;
use ffx_stream_util::TryStreamUtilExt;
use fidl::endpoints::ProtocolMarker;
use fidl_fuchsia_developer_ffx as ffx;
use fuchsia_async::Task;
use futures::TryStreamExt;
use protocols::prelude::*;
use std::rc::Rc;

struct Inner {
    events_in: async_channel::Receiver<ffx::FastbootTarget>,
    events_out: async_channel::Sender<ffx::FastbootTarget>,
}

#[ffx_protocol]
#[derive(Default)]
pub struct FastbootTargetStreamProtocol {
    inner: Option<Rc<Inner>>,
    fastboot_task: Option<Task<()>>,
}

#[async_trait(?Send)]
impl FidlProtocol for FastbootTargetStreamProtocol {
    type Protocol = ffx::FastbootTargetStreamMarker;
    type StreamHandler = FidlStreamHandler<Self>;

    async fn handle(&self, _cx: &Context, req: ffx::FastbootTargetStreamRequest) -> Result<()> {
        match req {
            ffx::FastbootTargetStreamRequest::GetNext { responder } => responder
                .send(
                    &self
                        .inner
                        .as_ref()
                        .expect("inner state should have been initialized")
                        .events_in
                        .recv()
                        .await?,
                )
                .map_err(Into::into),
        }
    }

    async fn start(&mut self, _cx: &Context) -> Result<()> {
        let (sender, receiver) = async_channel::bounded::<ffx::FastbootTarget>(1);
        let inner = Rc::new(Inner { events_in: receiver, events_out: sender });
        self.inner.replace(inner.clone());
        let inner = Rc::downgrade(&inner);
        let is_disabled: bool = ffx_daemon_target::fastboot::is_usb_discovery_enabled().await;
        // Probably could avoid creating the entire inner object but that refactoring can wait
        if is_disabled {
            return Ok(());
        }
        self.fastboot_task.replace(Task::local(async move {
            loop {
                let fastboot_serials = find_serial_numbers();
                if let Some(inner) = inner.upgrade() {
                    for serial in fastboot_serials {
                        let _ = inner
                            .events_out
                            .send(ffx::FastbootTarget {
                                serial: Some(serial),
                                ..Default::default()
                            })
                            .await;
                    }
                } else {
                    break;
                }
                fuchsia_async::Timer::new(FASTBOOT_CHECK_INTERVAL).await;
            }
        }));
        Ok(())
    }

    async fn stop(&mut self, _cx: &Context) -> Result<()> {
        if let Some(task) = self.fastboot_task.take() {
            task.cancel().await;
        }
        Ok(())
    }

    async fn serve<'a>(
        &'a self,
        cx: &'a Context,
        stream: <Self::Protocol as ProtocolMarker>::RequestStream,
    ) -> Result<()> {
        stream
            .map_err(|err| anyhow!("{}", err))
            .try_for_each_concurrent_while_connected(None, |req| self.handle(cx, req))
            .await
    }
}
