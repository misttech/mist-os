// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::gpt::GptManager;
use anyhow::{Context as _, Error};
use fidl::endpoints::{DiscoverableProtocolMarker as _, RequestStream as _};
use fs_management::filesystem::BlockConnector;
use futures::lock::Mutex as AsyncMutex;
use futures::stream::TryStreamExt as _;
use std::sync::Arc;
use vfs::directory::entry_container::Directory as _;
use vfs::directory::helper::DirectlyMutable as _;
use vfs::execution_scope::ExecutionScope;
use vfs::path::Path;
use {
    fidl_fuchsia_device as fdevice, fidl_fuchsia_io as fio,
    fidl_fuchsia_process_lifecycle as flifecycle, fidl_fuchsia_storagehost as fstoragehost,
    fuchsia_async as fasync, fuchsia_zircon as zx,
};

pub struct StorageHostService {
    state: AsyncMutex<State>,

    // The execution scope of the pseudo filesystem.
    scope: ExecutionScope,

    // The root of the pseudo filesystem for the component.
    export_dir: Arc<vfs::directory::immutable::Simple>,

    // A directory where partitions are published.
    partitions_dir: Arc<vfs::directory::immutable::Simple>,
}

#[derive(Default)]
enum State {
    #[default]
    Stopped,
    Running(Arc<GptManager>),
}

pub enum Services {
    StorageHost(fstoragehost::StorageHostRequestStream),
}

impl StorageHostService {
    pub fn new() -> Arc<Self> {
        let export_dir = vfs::directory::immutable::simple();
        let partitions_dir = vfs::directory::immutable::simple();
        export_dir.add_entry("partitions", partitions_dir.clone()).unwrap();
        Arc::new(Self {
            state: Default::default(),
            scope: ExecutionScope::new(),
            export_dir,
            partitions_dir,
        })
    }

    pub async fn run(
        self: Arc<Self>,
        outgoing_dir: zx::Channel,
        lifecycle_channel: Option<zx::Channel>,
    ) -> Result<(), Error> {
        let svc_dir = vfs::directory::immutable::simple();
        self.export_dir.add_entry("svc", svc_dir.clone()).expect("Unable to create svc dir");

        let weak = Arc::downgrade(&self);
        svc_dir.add_entry(
            fstoragehost::StorageHostMarker::PROTOCOL_NAME,
            vfs::service::host(move |requests| {
                let weak = weak.clone();
                async move {
                    if let Some(me) = weak.upgrade() {
                        let _ = me.handle_start_requests(requests).await;
                    }
                }
            }),
        )?;

        self.export_dir.clone().open(
            self.scope.clone(),
            fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::DIRECTORY
                | fio::OpenFlags::RIGHT_EXECUTABLE,
            Path::dot(),
            outgoing_dir.into(),
        );

        if let Some(channel) = lifecycle_channel {
            let me = self.clone();
            self.scope.spawn(async move {
                if let Err(e) = me.handle_lifecycle_requests(channel).await {
                    tracing::warn!(error = ?e, "handle_lifecycle_requests");
                }
            });
        }

        self.scope.wait().await;

        Ok(())
    }

    async fn handle_start_requests(
        self: Arc<Self>,
        mut stream: fstoragehost::StorageHostRequestStream,
    ) -> Result<(), Error> {
        while let Some(request) = stream.try_next().await.context("Reading request")? {
            tracing::debug!(?request);
            match request {
                fstoragehost::StorageHostRequest::Start { device_controller, responder } => {
                    responder
                        .send(
                            self.start(device_controller.into_proxy().unwrap())
                                .await
                                .map_err(|status| status.into_raw()),
                        )
                        .unwrap_or_else(|e| tracing::error!(?e, "Failed to send Start response"));
                }
            }
        }
        Ok(())
    }

    async fn start(
        self: &Arc<Self>,
        controller_proxy: fdevice::ControllerProxy,
    ) -> Result<(), zx::Status> {
        let mut state = self.state.lock().await;
        if let State::Running(..) = *state {
            tracing::warn!("Device already bound");
            return Err(zx::Status::ALREADY_BOUND);
        }

        let runner = GptManager::new(
            Arc::new(controller_proxy) as Arc<dyn BlockConnector>,
            self.partitions_dir.clone(),
        )
        .await
        .map_err(|err| {
            tracing::error!(?err, "Failed to load GPT");
            zx::Status::INTERNAL
        })?;
        *state = State::Running(runner);

        Ok(())
    }

    async fn handle_lifecycle_requests(&self, lifecycle_channel: zx::Channel) -> Result<(), Error> {
        let mut stream = flifecycle::LifecycleRequestStream::from_channel(
            fasync::Channel::from_channel(lifecycle_channel),
        );
        match stream.try_next().await.context("Reading request")? {
            Some(flifecycle::LifecycleRequest::Stop { .. }) => {
                tracing::info!("Received Lifecycle::Stop request");
                let mut state = self.state.lock().await;
                if let State::Running(gpt) = std::mem::take(&mut *state) {
                    gpt.shutdown().await;
                }
                self.scope.shutdown();
                tracing::info!("Shutdown complete");
            }
            None => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::StorageHostService;
    use crate::gpt::format::testing::{format_gpt, PartitionDescriptor};
    use fake_block_server::FakeServer;
    use fidl::endpoints::{create_endpoints, Proxy as _, RequestStream as _};
    use fidl_fuchsia_process_lifecycle::LifecycleMarker;
    use fuchsia_component::client::connect_to_protocol_at_dir_svc;
    use futures::{FutureExt as _, StreamExt as _};
    use std::sync::Arc;
    use {
        fidl_fuchsia_device as fdevice, fidl_fuchsia_hardware_block_volume as fvolume,
        fidl_fuchsia_io as fio, fidl_fuchsia_storagehost as fstoragehost, fuchsia_async as fasync,
        fuchsia_zircon as zx,
    };

    #[fuchsia::test]
    async fn lifecycle() {
        let (outgoing_dir, outgoing_dir_server) =
            fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
        let (lifecycle_client, lifecycle_server) =
            fidl::endpoints::create_proxy::<LifecycleMarker>().unwrap();
        let (controller_client, controller_server) =
            create_endpoints::<fdevice::ControllerMarker>();

        futures::join!(
            async {
                // Client
                let client = connect_to_protocol_at_dir_svc::<fstoragehost::StorageHostMarker>(
                    &outgoing_dir,
                )
                .unwrap();
                client.start(controller_client).await.expect("FIDL error").expect("Start failed");
                lifecycle_client.stop().expect("Stop failed");
                fasync::OnSignals::new(
                    &lifecycle_client.into_channel().expect("into_channel failed"),
                    zx::Signals::CHANNEL_PEER_CLOSED,
                )
                .await
                .expect("OnSignals failed");
            },
            async {
                // Server
                let service = StorageHostService::new();
                service
                    .run(outgoing_dir_server.into_channel(), Some(lifecycle_server.into_channel()))
                    .await
                    .expect("Run failed");
            },
            async {
                // Block device
                let vmo = zx::Vmo::create(4096).unwrap();
                format_gpt(
                    &vmo,
                    512,
                    vec![PartitionDescriptor {
                        label: "part".to_string(),
                        type_guid: uuid::Uuid::from_bytes([0xabu8; 16]),
                        instance_guid: uuid::Uuid::from_bytes([0xcdu8; 16]),
                        start_block: 4,
                        num_blocks: 1,
                    }],
                );
                let block_server = Arc::new(FakeServer::from_vmo(512, vmo));
                let mut stream = fdevice::ControllerRequestStream::from_channel(
                    fasync::Channel::from_channel(controller_server.into_channel()),
                );
                while let Some(request) = stream.next().await {
                    let block_server = block_server.clone();
                    match request {
                        Ok(fdevice::ControllerRequest::ConnectToDeviceFidl { server, .. }) => {
                            fasync::Task::spawn(async move {
                                let stream = fvolume::VolumeRequestStream::from_channel(
                                    fasync::Channel::from_channel(server),
                                );
                                block_server.serve(stream).await.expect("Handle requests");
                            })
                            .detach();
                        }
                        _ => {
                            unreachable!()
                        }
                    }
                }
            }
            .fuse(),
        );
    }
}
