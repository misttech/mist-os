// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::gpt::GptManager;
use anyhow::{Context as _, Error};
use fidl::endpoints::{DiscoverableProtocolMarker as _, RequestStream as _};
use futures::lock::Mutex as AsyncMutex;
use futures::stream::TryStreamExt as _;
use std::sync::Arc;
use vfs::directory::entry_container::Directory as _;
use vfs::directory::helper::DirectlyMutable as _;
use vfs::execution_scope::ExecutionScope;
use vfs::path::Path;
use {
    fidl_fuchsia_fs_startup as fstartup, fidl_fuchsia_hardware_block as fblock,
    fidl_fuchsia_io as fio, fidl_fuchsia_process_lifecycle as flifecycle,
    fidl_fuchsia_storagehost as fstoragehost, fuchsia_async as fasync,
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
        let weak2 = weak.clone();
        let weak3 = weak.clone();
        svc_dir.add_entry(
            fstartup::StartupMarker::PROTOCOL_NAME,
            vfs::service::host(move |requests| {
                let weak = weak.clone();
                async move {
                    if let Some(me) = weak.upgrade() {
                        let _ = me.handle_start_requests(requests).await;
                    }
                }
            }),
        )?;
        svc_dir.add_entry(
            fstoragehost::PartitionsManagerMarker::PROTOCOL_NAME,
            vfs::service::host(move |requests| {
                let weak = weak2.clone();
                async move {
                    if let Some(me) = weak.upgrade() {
                        let _ = me.handle_partitions_manager_requests(requests).await;
                    }
                }
            }),
        )?;
        svc_dir.add_entry(
            fstoragehost::PartitionsAdminMarker::PROTOCOL_NAME,
            vfs::service::host(move |requests| {
                let weak = weak3.clone();
                async move {
                    if let Some(me) = weak.upgrade() {
                        let _ = me.handle_partitions_admin_requests(requests).await;
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
        mut stream: fstartup::StartupRequestStream,
    ) -> Result<(), Error> {
        while let Some(request) = stream.try_next().await.context("Reading request")? {
            tracing::debug!(?request);
            match request {
                fstartup::StartupRequest::Start { device, options: _, responder } => {
                    responder
                        .send(
                            self.start(device.into_proxy().unwrap())
                                .await
                                .map_err(|status| status.into_raw()),
                        )
                        .unwrap_or_else(|e| tracing::error!(?e, "Failed to send Start response"));
                }
                fstartup::StartupRequest::Format { responder, .. } => {
                    responder
                        .send(Err(zx::Status::NOT_SUPPORTED.into_raw()))
                        .unwrap_or_else(|e| tracing::error!(?e, "Failed to send Check response"));
                }
                fstartup::StartupRequest::Check { responder, .. } => {
                    responder
                        .send(Err(zx::Status::NOT_SUPPORTED.into_raw()))
                        .unwrap_or_else(|e| tracing::error!(?e, "Failed to send Check response"));
                }
            }
        }
        Ok(())
    }

    async fn start(self: &Arc<Self>, device: fblock::BlockProxy) -> Result<(), zx::Status> {
        let mut state = self.state.lock().await;
        if let State::Running(..) = *state {
            tracing::warn!("Device already bound");
            return Err(zx::Status::ALREADY_BOUND);
        }

        let runner = GptManager::new(device, self.partitions_dir.clone()).await.map_err(|err| {
            tracing::error!(?err, "Failed to load GPT");
            zx::Status::INTERNAL
        })?;
        *state = State::Running(runner);

        Ok(())
    }

    async fn handle_partitions_manager_requests(
        self: Arc<Self>,
        mut stream: fstoragehost::PartitionsManagerRequestStream,
    ) -> Result<(), Error> {
        while let Some(request) = stream.try_next().await.context("Reading request")? {
            tracing::debug!(?request);
            match request {
                fstoragehost::PartitionsManagerRequest::CreateTransaction { responder } => {
                    responder
                        .send(self.create_transaction().await.map_err(|status| status.into_raw()))
                        .unwrap_or_else(|e| tracing::error!(?e, "Failed to send Start response"));
                }
                fstoragehost::PartitionsManagerRequest::CommitTransaction {
                    transaction,
                    responder,
                } => {
                    responder
                        .send(
                            self.commit_transaction(transaction)
                                .await
                                .map_err(|status| status.into_raw()),
                        )
                        .unwrap_or_else(|e| tracing::error!(?e, "Failed to send Start response"));
                }
            }
        }
        Ok(())
    }

    async fn create_transaction(&self) -> Result<zx::EventPair, zx::Status> {
        let gpt_manager = self.gpt_manager().await?;
        gpt_manager.create_transaction().await
    }

    async fn commit_transaction(&self, transaction: zx::EventPair) -> Result<(), zx::Status> {
        let gpt_manager = self.gpt_manager().await?;
        gpt_manager.commit_transaction(transaction).await
    }

    async fn handle_partitions_admin_requests(
        self: Arc<Self>,
        mut stream: fstoragehost::PartitionsAdminRequestStream,
    ) -> Result<(), Error> {
        while let Some(request) = stream.try_next().await.context("Reading request")? {
            tracing::debug!(?request);
            match request {
                fstoragehost::PartitionsAdminRequest::ResetPartitionTable {
                    partitions,
                    responder,
                } => {
                    responder
                        .send(
                            self.reset_partition_table(partitions)
                                .await
                                .map_err(|status| status.into_raw()),
                        )
                        .unwrap_or_else(|e| tracing::error!(?e, "Failed to send Start response"));
                }
            }
        }
        Ok(())
    }

    async fn reset_partition_table(
        &self,
        partitions: Vec<fstoragehost::PartitionInfo>,
    ) -> Result<(), zx::Status> {
        let gpt_manager = self.gpt_manager().await?;
        gpt_manager.reset_partition_table(partitions).await
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

    async fn gpt_manager(&self) -> Result<Arc<GptManager>, zx::Status> {
        match &*self.state.lock().await {
            State::Stopped => Err(zx::Status::BAD_STATE),
            State::Running(gpt) => Ok(gpt.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::StorageHostService;
    use block_client::RemoteBlockClient;
    use fake_block_server::FakeServer;
    use fidl::endpoints::Proxy as _;
    use fidl_fuchsia_process_lifecycle::LifecycleMarker;
    use fuchsia_component::client::connect_to_protocol_at_dir_svc;
    use futures::FutureExt as _;
    use gpt::{Gpt, Guid, PartitionInfo};
    use std::sync::Arc;
    use {
        fidl_fuchsia_fs_startup as fstartup, fidl_fuchsia_hardware_block as fblock,
        fidl_fuchsia_hardware_block_volume as fvolume, fidl_fuchsia_io as fio,
        fidl_fuchsia_storagehost as fstoragehost, fuchsia_async as fasync,
    };

    async fn setup_server(
        block_size: u32,
        block_count: u64,
        partitions: Vec<PartitionInfo>,
    ) -> Arc<FakeServer> {
        let vmo = zx::Vmo::create(block_size as u64 * block_count).unwrap();
        let server = Arc::new(FakeServer::from_vmo(512, vmo));
        {
            let (block_client, block_server) =
                fidl::endpoints::create_proxy::<fblock::BlockMarker>().unwrap();
            let volume_stream = fidl::endpoints::ServerEnd::<fvolume::VolumeMarker>::from(
                block_server.into_channel(),
            )
            .into_stream()
            .unwrap();
            let server_clone = server.clone();
            let _task = fasync::Task::spawn(async move { server_clone.serve(volume_stream).await });
            let client = Arc::new(RemoteBlockClient::new(block_client).await.unwrap());
            Gpt::format(client, partitions).await.unwrap();
        }
        server
    }

    #[fuchsia::test]
    async fn lifecycle() {
        let (outgoing_dir, outgoing_dir_server) =
            fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
        let (lifecycle_client, lifecycle_server) =
            fidl::endpoints::create_proxy::<LifecycleMarker>().unwrap();
        let (block_client, block_server) =
            fidl::endpoints::create_endpoints::<fblock::BlockMarker>();
        let volume_stream =
            fidl::endpoints::ServerEnd::<fvolume::VolumeMarker>::from(block_server.into_channel())
                .into_stream()
                .unwrap();

        futures::join!(
            async {
                // Client
                let client =
                    connect_to_protocol_at_dir_svc::<fstartup::StartupMarker>(&outgoing_dir)
                        .unwrap();
                client
                    .start(
                        block_client,
                        fstartup::StartOptions {
                            read_only: false,
                            verbose: false,
                            fsck_after_every_transaction: false,
                            write_compression_algorithm:
                                fstartup::CompressionAlgorithm::ZstdChunked,
                            write_compression_level: 0,
                            cache_eviction_policy_override: fstartup::EvictionPolicyOverride::None,
                            startup_profiling_seconds: 0,
                        },
                    )
                    .await
                    .expect("FIDL error")
                    .expect("Start failed");
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
                let server = setup_server(
                    512,
                    8,
                    vec![PartitionInfo {
                        label: "part".to_string(),
                        type_guid: Guid::from_bytes([0xabu8; 16]),
                        instance_guid: Guid::from_bytes([0xcdu8; 16]),
                        start_block: 4,
                        num_blocks: 1,
                        flags: 0,
                    }],
                )
                .await;
                let _ = server.serve(volume_stream).await;
            }
            .fuse(),
        );
    }

    #[fuchsia::test]
    async fn transaction_lifecycle() {
        let (outgoing_dir, outgoing_dir_server) =
            fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
        let (lifecycle_client, lifecycle_server) =
            fidl::endpoints::create_proxy::<LifecycleMarker>().unwrap();
        let (block_client, block_server) =
            fidl::endpoints::create_endpoints::<fblock::BlockMarker>();
        let volume_stream =
            fidl::endpoints::ServerEnd::<fvolume::VolumeMarker>::from(block_server.into_channel())
                .into_stream()
                .unwrap();

        futures::join!(
            async {
                // Client
                connect_to_protocol_at_dir_svc::<fstartup::StartupMarker>(&outgoing_dir)
                    .unwrap()
                    .start(
                        block_client,
                        fstartup::StartOptions {
                            read_only: false,
                            verbose: false,
                            fsck_after_every_transaction: false,
                            write_compression_algorithm:
                                fstartup::CompressionAlgorithm::ZstdChunked,
                            write_compression_level: 0,
                            cache_eviction_policy_override: fstartup::EvictionPolicyOverride::None,
                            startup_profiling_seconds: 0,
                        },
                    )
                    .await
                    .expect("FIDL error")
                    .expect("Start failed");

                let pm_client = connect_to_protocol_at_dir_svc::<
                    fstoragehost::PartitionsManagerMarker,
                >(&outgoing_dir)
                .unwrap();
                let transaction = pm_client
                    .create_transaction()
                    .await
                    .expect("FIDL error")
                    .expect("create_transaction failed");

                pm_client
                    .create_transaction()
                    .await
                    .expect("FIDL error")
                    .expect_err("create_transaction should fail while other txn exists");

                pm_client
                    .commit_transaction(transaction)
                    .await
                    .expect("FIDL error")
                    .expect("commit_transaction failed");

                {
                    let _transaction = pm_client
                        .create_transaction()
                        .await
                        .expect("FIDL error")
                        .expect("create_transaction should succeed after committing txn");
                }

                pm_client
                    .create_transaction()
                    .await
                    .expect("FIDL error")
                    .expect("create_transaction should succeed after dropping txn");

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
                let server = setup_server(
                    512,
                    16,
                    vec![PartitionInfo {
                        label: "part".to_string(),
                        type_guid: Guid::from_bytes([0xabu8; 16]),
                        instance_guid: Guid::from_bytes([0xcdu8; 16]),
                        start_block: 4,
                        num_blocks: 1,
                        flags: 0,
                    }],
                )
                .await;
                let _ = server.serve(volume_stream).await;
            }
            .fuse(),
        );
    }
}
