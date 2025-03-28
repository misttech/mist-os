// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use discovery::{
    wait_for_devices, DiscoverySources, FastbootConnectionState, TargetEvent, TargetState,
};
use ffx::TargetIpAddrInfo;
use ffx_config::{get, is_usb_discovery_disabled};
use ffx_stream_util::TryStreamUtilExt;
use fidl::endpoints::ProtocolMarker;
use fidl_fuchsia_developer_ffx as ffx;
use fuchsia_async::Task;
use futures::{StreamExt, TryStreamExt};
use protocols::prelude::*;
use std::path::PathBuf;
use std::rc::Rc;

struct Inner {
    events_in: async_channel::Receiver<ffx::FastbootTarget>,
    events_out: async_channel::Sender<ffx::FastbootTarget>,
}

#[ffx_protocol]
#[derive(Default)]
pub struct FastbootTargetStreamProtocol {
    inner: Option<Rc<Inner>>,
    fastboot_task: Option<Task<Result<()>>>,
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
        if let Some(context) = ffx_config::global_env_context() {
            // Probably could avoid creating the entire inner object but that refactoring can wait
            if is_usb_discovery_disabled(&context).await {
                return Ok(());
            }
        }
        self.fastboot_task.replace(Task::local(async move {
            loop {
                let fastboot_file_path: Option<PathBuf> =
                    get(fastboot_file_discovery::FASTBOOT_FILE_PATH).ok();
                let mut device_stream = wait_for_devices(
                    |_: &_| true,
                    None,
                    fastboot_file_path,
                    true,
                    true,
                    DiscoverySources::USB | DiscoverySources::FASTBOOT_FILE,
                )
                .await?;
                while let Some(s) = device_stream.next().await {
                    if let Ok(event) = s {
                        match event {
                            TargetEvent::Added(e) => match e.state {
                                TargetState::Fastboot(fts) => match fts.connection_state {
                                    FastbootConnectionState::Usb => {
                                        if let Some(inner) = inner.upgrade() {
                                            let _ = inner
                                                .events_out
                                                .send(ffx::FastbootTarget {
                                                    serial: Some(fts.serial_number),
                                                    ..Default::default()
                                                })
                                                .await;
                                        }
                                    }
                                    FastbootConnectionState::Tcp(addrs) => {
                                        if let Some(inner) = inner.upgrade() {
                                            let _ = inner
                                                .events_out
                                                .send(ffx::FastbootTarget {
                                                    addresses: Some(
                                                        addrs
                                                            .into_iter()
                                                            .map(|x| TargetIpAddrInfo::from(x.into()))
                                                            .collect(),
                                                    ),
                                                    ..Default::default()
                                                })
                                                .await;
                                        }
                                    }
                                    FastbootConnectionState::Udp(addrs) => {
                                        if let Some(inner) = inner.upgrade() {
                                            let _ = inner
                                                .events_out
                                                .send(ffx::FastbootTarget {
                                                    addresses: Some(
                                                        addrs
                                                            .into_iter()
                                                            .map(|x| TargetIpAddrInfo::from(x.into()))
                                                            .collect(),
                                                    ),
                                                    ..Default::default()
                                                })
                                                .await;
                                        }
                                    }
                                },
                                e @ _ => {
                                    tracing::debug!("We only support Fastboot events in this module... skipping non fastboot event: {:?}", e);
                                }
                            },
                            TargetEvent::Removed(_) => {
                                tracing::debug!("Skipping removed event");
                            }
                        }
                    }
                }
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
