// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fuchsia::debug::{handle_debug_request, FxfsDebug};
use crate::fuchsia::errors::map_to_status;
use crate::fuchsia::memory_pressure::MemoryPressureMonitor;
use crate::fuchsia::volumes_directory::VolumesDirectory;
use anyhow::{bail, Context, Error};
use async_trait::async_trait;
use block_client::{BlockClient as _, RemoteBlockClient};
use fidl::endpoints::{ClientEnd, DiscoverableProtocolMarker, RequestStream};
use fidl::AsHandleRef;
use fidl_fuchsia_fs::{AdminMarker, AdminRequest, AdminRequestStream};
use fidl_fuchsia_fs_startup::{
    CheckOptions, StartOptions, StartupMarker, StartupRequest, StartupRequestStream, VolumesMarker,
    VolumesRequest, VolumesRequestStream,
};
use fidl_fuchsia_fxfs::{DebugMarker, DebugRequestStream};
use fidl_fuchsia_hardware_block::BlockMarker;
use fidl_fuchsia_process_lifecycle::{LifecycleRequest, LifecycleRequestStream};
use fs_inspect::{FsInspect, FsInspectTree, InfoData, UsageData};
use futures::lock::Mutex;
use futures::TryStreamExt;
use fxfs::filesystem::{mkfs, FxFilesystem, FxFilesystemBuilder, OpenFxFilesystem, MIN_BLOCK_SIZE};
use fxfs::log::*;
use fxfs::object_store::volume::root_volume;
use fxfs::serialized_types::LATEST_VERSION;
use fxfs::{fsck, metrics};
use std::ops::Deref;
use std::sync::{Arc, Weak};
use storage_device::block_device::BlockDevice;
use storage_device::DeviceHolder;
use vfs::directory::entry_container::Directory;
use vfs::directory::helper::DirectlyMutable;
use vfs::execution_scope::ExecutionScope;
use vfs::path::Path;
use {fidl_fuchsia_io as fio, fuchsia_async as fasync};

const FXFS_INFO_NAME: &'static str = "fxfs";

pub fn map_to_raw_status(e: Error) -> zx::sys::zx_status_t {
    map_to_status(e).into_raw()
}

pub async fn new_block_client(remote: ClientEnd<BlockMarker>) -> Result<RemoteBlockClient, Error> {
    Ok(RemoteBlockClient::new(remote.into_proxy()?).await?)
}

/// Runs Fxfs as a component.
pub struct Component {
    state: futures::lock::Mutex<State>,

    // The execution scope of the pseudo filesystem.
    scope: ExecutionScope,

    // The root of the pseudo filesystem for the component.
    export_dir: Arc<vfs::directory::immutable::Simple>,
}

// Wrapper type to add `FsInspect` support to an `OpenFxFilesystem`.
struct InspectedFxFilesystem(OpenFxFilesystem, /*fs_id=*/ u64);

impl From<OpenFxFilesystem> for InspectedFxFilesystem {
    fn from(fs: OpenFxFilesystem) -> Self {
        Self(fs, zx::Event::create().get_koid().unwrap().raw_koid())
    }
}

impl Deref for InspectedFxFilesystem {
    type Target = Arc<FxFilesystem>;
    fn deref(&self) -> &Self::Target {
        &self.0.deref()
    }
}

#[async_trait]
impl FsInspect for InspectedFxFilesystem {
    fn get_info_data(&self) -> InfoData {
        let earliest_version = self.0.super_block_header().earliest_version;
        InfoData {
            id: self.1,
            fs_type: fidl_fuchsia_fs::VfsType::Fxfs.into_primitive().into(),
            name: FXFS_INFO_NAME.into(),
            version_major: LATEST_VERSION.major.into(),
            version_minor: LATEST_VERSION.minor.into(),
            block_size: self.0.block_size() as u64,
            max_filename_length: fio::MAX_FILENAME,
            oldest_version: Some((earliest_version.major.into(), earliest_version.minor.into())),
        }
    }

    async fn get_usage_data(&self) -> UsageData {
        let info = self.0.get_info();
        UsageData {
            total_bytes: info.total_bytes,
            used_bytes: info.used_bytes,
            // TODO(https://fxbug.dev/42175930): Should these be moved to per-volume nodes?
            total_nodes: 0,
            used_nodes: 0,
        }
    }
}

struct RunningState {
    // We have to wrap this in an Arc, even though it itself basically just wraps an Arc, so that
    // FsInspectTree can reference `fs` as a Weak<dyn FsInspect>`.
    fs: Arc<InspectedFxFilesystem>,
    volumes: Arc<VolumesDirectory>,
    _debug: Arc<FxfsDebug>,
    _inspect_tree: Arc<FsInspectTree>,
}

enum State {
    ComponentStarted,
    Running(RunningState),
}

impl State {
    async fn stop(&mut self, outgoing_dir: &vfs::directory::immutable::Simple) {
        if let State::Running(RunningState { fs, volumes, .. }) =
            std::mem::replace(self, State::ComponentStarted)
        {
            info!("Stopping Fxfs runtime; remaining connections will be forcibly closed");
            let _ = outgoing_dir.remove_entry("volumes", /* must_be_directory: */ false);
            let _ = outgoing_dir.remove_entry("debug", /* must_be_directory: */ false);
            volumes.terminate().await;
            let _ = fs.deref().close().await;
        }
    }
}

impl Component {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::ComponentStarted),
            scope: ExecutionScope::new(),
            export_dir: vfs::directory::immutable::simple(),
        })
    }

    /// Runs Fxfs as a component.
    pub async fn run(
        self: Arc<Self>,
        outgoing_dir: zx::Channel,
        lifecycle_channel: Option<zx::Channel>,
    ) -> Result<(), Error> {
        let svc_dir = vfs::directory::immutable::simple();
        self.export_dir.add_entry("svc", svc_dir.clone()).expect("Unable to create svc dir");

        let weak = Arc::downgrade(&self);
        svc_dir.add_entry(
            StartupMarker::PROTOCOL_NAME,
            vfs::service::host(move |requests| {
                let weak = weak.clone();
                async move {
                    if let Some(me) = weak.upgrade() {
                        let _ = me.handle_startup_requests(requests).await;
                    }
                }
            }),
        )?;
        let weak = Arc::downgrade(&self);
        svc_dir.add_entry(
            VolumesMarker::PROTOCOL_NAME,
            vfs::service::host(move |requests| {
                let weak = weak.clone();
                async move {
                    if let Some(me) = weak.upgrade() {
                        me.handle_volumes_requests(requests).await;
                    }
                }
            }),
        )?;

        let weak = Arc::downgrade(&self);
        svc_dir.add_entry(
            AdminMarker::PROTOCOL_NAME,
            vfs::service::host(move |requests| {
                let weak = weak.clone();
                async move {
                    if let Some(me) = weak.upgrade() {
                        let _ = me.handle_admin_requests(requests).await;
                    }
                }
            }),
        )?;

        // TODO(b/315704445): Only enable in debug builds?
        let weak = Arc::downgrade(&self);
        svc_dir.add_entry(
            DebugMarker::PROTOCOL_NAME,
            vfs::service::host(move |requests| {
                let weak = weak.clone();
                async move {
                    if let Some(me) = weak.upgrade() {
                        let _ = me.handle_debug_requests(requests).await;
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
                if let Err(error) = me.handle_lifecycle_requests(channel).await {
                    warn!(?error, "handle_lifecycle_requests");
                }
            });
        }

        self.scope.wait().await;

        Ok(())
    }

    async fn handle_startup_requests(&self, mut stream: StartupRequestStream) -> Result<(), Error> {
        while let Some(request) = stream.try_next().await? {
            match request {
                StartupRequest::Start { responder, device, options } => {
                    responder.send(self.handle_start(device, options).await.map_err(|error| {
                        error!(?error, "handle_start failed");
                        map_to_raw_status(error)
                    }))?
                }
                StartupRequest::Format { responder, device, .. } => {
                    responder.send(self.handle_format(device).await.map_err(|error| {
                        error!(?error, "handle_format failed");
                        map_to_raw_status(error)
                    }))?
                }
                StartupRequest::Check { responder, device, options } => {
                    responder.send(self.handle_check(device, options).await.map_err(|error| {
                        error!(?error, "handle_check failed");
                        map_to_raw_status(error)
                    }))?
                }
            }
        }
        Ok(())
    }

    async fn handle_start(
        &self,
        device: ClientEnd<BlockMarker>,
        options: StartOptions,
    ) -> Result<(), Error> {
        info!(?options, "Received start request");
        let mut state = self.state.lock().await;
        // TODO(https://fxbug.dev/42174810): This is not very graceful.  It would be better for the client to
        // explicitly shut down all volumes first, and make this fail if there are remaining active
        // connections.  Fix the bug in fs_test which requires this.
        state.stop(&self.export_dir).await;
        let client = new_block_client(device).await?;

        // TODO(https://fxbug.dev/42063349) Add support for block sizes greater than the page size.
        assert!(client.block_size() <= zx::system_get_page_size());
        assert!((zx::system_get_page_size() as u64) == MIN_BLOCK_SIZE);

        let fs = FxFilesystemBuilder::new()
            .fsck_after_every_transaction(options.fsck_after_every_transaction)
            .read_only(options.read_only)
            .open(DeviceHolder::new(BlockDevice::new(Box::new(client), options.read_only).await?))
            .await?;
        let root_volume = root_volume(fs.clone()).await?;
        let fs: Arc<InspectedFxFilesystem> = Arc::new(fs.into());
        let weak_fs = Arc::downgrade(&fs) as Weak<dyn FsInspect + Send + Sync>;
        let inspect_tree =
            Arc::new(FsInspectTree::new(weak_fs, fuchsia_inspect::component::inspector().root()));
        let mem_monitor = match MemoryPressureMonitor::start().await {
            Ok(v) => Some(v),
            Err(error) => {
                warn!(
                    ?error,
                    "Failed to connect to memory pressure monitor. Running \
                     without pressure awareness."
                );
                None
            }
        };

        let volumes =
            VolumesDirectory::new(root_volume, Arc::downgrade(&inspect_tree), mem_monitor).await?;

        self.export_dir.add_entry_may_overwrite(
            "volumes",
            volumes.directory_node().clone(),
            /* overwrite: */ true,
        )?;

        let debug = FxfsDebug::new(&**fs, &volumes)?;
        self.export_dir.add_entry_may_overwrite("debug", debug.root(), true)?;

        fs.allocator().track_statistics(&metrics::detail(), "allocator");
        fs.journal().track_statistics(&metrics::detail(), "journal");
        fs.object_manager().track_statistics(&metrics::detail(), "object_manager");
        metrics::object_stores_tracker().register_store("__root", Arc::downgrade(&fs.root_store()));

        let info = fs.get_info();
        info!(
            device_size = info.total_bytes,
            used = info.used_bytes,
            free = info.total_bytes - info.used_bytes,
            "Mounted"
        );

        if options.startup_profiling_seconds > 0 {
            // Unwrap ok, shouldn't have anything else recording or replaying this early in startup.
            volumes
                .clone()
                .record_or_replay_profile(".boot".to_owned(), options.startup_profiling_seconds)
                .await
                .unwrap();
        }

        *state = State::Running(RunningState {
            fs,
            volumes,
            _debug: debug,
            _inspect_tree: inspect_tree,
        });

        Ok(())
    }

    async fn handle_format(&self, device: ClientEnd<BlockMarker>) -> Result<(), Error> {
        let device = DeviceHolder::new(
            BlockDevice::new(
                Box::new(new_block_client(device).await?),
                /* read_only: */ false,
            )
            .await?,
        );
        mkfs(device).await?;
        info!("Formatted filesystem");
        Ok(())
    }

    async fn handle_check(
        &self,
        device: ClientEnd<BlockMarker>,
        _options: CheckOptions,
    ) -> Result<(), Error> {
        let state = self.state.lock().await;
        let (fs_container, fs) = match *state {
            State::ComponentStarted => {
                let client = new_block_client(device).await?;
                let fs_container = FxFilesystemBuilder::new()
                    .read_only(true)
                    .open(DeviceHolder::new(
                        BlockDevice::new(Box::new(client), /* read_only: */ true).await?,
                    ))
                    .await?;
                let fs = fs_container.clone();
                (Some(fs_container), fs)
            }
            State::Running(RunningState { ref fs, .. }) => (None, fs.deref().deref().clone()),
        };
        let res = fsck::fsck(fs.clone()).await;
        if let Some(fs_container) = fs_container {
            let _ = fs_container.close().await;
        }
        // TODO(b/311550633): Stash ok res in inspect.
        info!("handle_check for fs: {:?}", res?);
        Ok(())
    }

    async fn handle_admin_requests(&self, mut stream: AdminRequestStream) -> Result<(), Error> {
        while let Some(request) = stream.try_next().await.context("Reading request")? {
            if self.handle_admin(request).await? {
                break;
            }
        }
        Ok(())
    }

    // Returns true if we should close the connection.
    async fn handle_admin(&self, req: AdminRequest) -> Result<bool, Error> {
        match req {
            AdminRequest::Shutdown { responder } => {
                info!("Received shutdown request");
                self.shutdown().await;
                responder
                    .send()
                    .unwrap_or_else(|error| warn!(?error, "Failed to send shutdown response"));
                return Ok(true);
            }
        }
    }

    /// Handles fuchsia.fxfs.Debug requests, providing live debugging internals of the running
    /// filesystem.
    async fn handle_debug_requests(&self, mut stream: DebugRequestStream) -> Result<(), Error> {
        while let Some(request) = stream.try_next().await.context("Reading request")? {
            let state = self.state.lock().await;
            let (fs, volumes) = match &*state {
                State::ComponentStarted => {
                    info!("Debug commands are not valid unless component is started.");
                    bail!("Component not started");
                }
                State::Running(RunningState { ref fs, volumes, .. }) => {
                    (fs.deref().deref().clone(), volumes.clone())
                }
            };
            handle_debug_request(fs, volumes, request).await?;
        }
        Ok(())
    }

    async fn shutdown(&self) {
        self.state.lock().await.stop(&self.export_dir).await;
        info!("Filesystem terminated");
    }

    async fn handle_volumes_requests(&self, mut stream: VolumesRequestStream) {
        let volumes =
            if let State::Running(RunningState { ref volumes, .. }) = &*self.state.lock().await {
                volumes.clone()
            } else {
                let _ = stream.into_inner().0.shutdown_with_epitaph(zx::Status::BAD_STATE);
                return;
            };
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                VolumesRequest::Create {
                    name,
                    outgoing_directory,
                    create_options: _,
                    mount_options,
                    responder,
                } => {
                    info!(
                        name = name.as_str(),
                        "Create {}volume",
                        if mount_options.crypt.is_some() { "encrypted " } else { "" }
                    );
                    responder
                        .send(
                            volumes
                                .create_and_serve_volume(
                                    &name,
                                    outgoing_directory.into_channel().into(),
                                    mount_options,
                                )
                                .await
                                .map_err(map_to_raw_status),
                        )
                        .unwrap_or_else(|error| {
                            warn!(?error, "Failed to send volume creation response")
                        });
                }
                VolumesRequest::Remove { name, responder } => {
                    info!(name = name.as_str(), "Remove volume");
                    responder
                        .send(volumes.remove_volume(&name).await.map_err(map_to_raw_status))
                        .unwrap_or_else(|error| {
                            warn!(?error, "Failed to send volume removal response")
                        });
                }
            }
        }
    }

    async fn handle_lifecycle_requests(&self, lifecycle_channel: zx::Channel) -> Result<(), Error> {
        let mut stream =
            LifecycleRequestStream::from_channel(fasync::Channel::from_channel(lifecycle_channel));
        match stream.try_next().await.context("Reading request")? {
            Some(LifecycleRequest::Stop { .. }) => {
                info!("Received Lifecycle::Stop request");
                self.shutdown().await;
            }
            None => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{new_block_client, Component};
    use fidl::endpoints::{Proxy, ServerEnd};
    use fidl_fuchsia_fs::AdminMarker;
    use fidl_fuchsia_fs_startup::{
        CompressionAlgorithm, CreateOptions, EvictionPolicyOverride, MountOptions, StartOptions,
        StartupMarker, VolumesMarker,
    };
    use fidl_fuchsia_process_lifecycle::{LifecycleMarker, LifecycleProxy};
    use fuchsia_component::client::connect_to_protocol_at_dir_svc;
    use fuchsia_fs::directory::readdir;
    use futures::future::{BoxFuture, FusedFuture, FutureExt};
    use futures::{pin_mut, select};
    use fxfs::filesystem::FxFilesystem;
    use fxfs::object_store::volume::root_volume;
    use ramdevice_client::RamdiskClientBuilder;
    use std::pin::Pin;
    use storage_device::block_device::BlockDevice;
    use storage_device::DeviceHolder;
    use {fidl_fuchsia_io as fio, fuchsia_async as fasync};

    async fn run_test(
        callback: impl Fn(&fio::DirectoryProxy, LifecycleProxy) -> BoxFuture<'static, ()>,
    ) -> Pin<Box<impl FusedFuture>> {
        let ramdisk = RamdiskClientBuilder::new(512, 16384)
            .use_v2()
            .build()
            .await
            .expect("Failed to build ramdisk");

        {
            let fs = FxFilesystem::new_empty(DeviceHolder::new(
                BlockDevice::new(
                    Box::new(
                        new_block_client(ramdisk.open().expect("Unable to open ramdisk"))
                            .await
                            .expect("Unable to create block client"),
                    ),
                    false,
                )
                .await
                .unwrap(),
            ))
            .await
            .expect("FxFilesystem::new_empty failed");
            {
                let root_volume = root_volume(fs.clone()).await.expect("Open root_volume failed");
                root_volume.new_volume("default", None).await.expect("Create volume failed");
            }
            fs.close().await.expect("close failed");
        }

        let (client_end, server_end) =
            fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();

        let (lifecycle_client, lifecycle_server) =
            fidl::endpoints::create_proxy::<LifecycleMarker>().unwrap();

        let mut component_task = Box::pin(
            async {
                Component::new()
                    .run(server_end.into_channel(), Some(lifecycle_server.into_channel()))
                    .await
                    .expect("Failed to run component");
            }
            .fuse(),
        );

        let startup_proxy = connect_to_protocol_at_dir_svc::<StartupMarker>(&client_end)
            .expect("Unable to connect to Startup protocol");
        let task = async {
            startup_proxy
                .start(
                    ramdisk.open().expect("Unable to open ramdisk").into(),
                    StartOptions {
                        read_only: false,
                        verbose: false,
                        fsck_after_every_transaction: false,
                        write_compression_algorithm: CompressionAlgorithm::ZstdChunked,
                        write_compression_level: 0,
                        cache_eviction_policy_override: EvictionPolicyOverride::None,
                        startup_profiling_seconds: 0,
                    },
                )
                .await
                .expect("Start failed (FIDL)")
                .expect("Start failed");
            callback(&client_end, lifecycle_client).await;
        }
        .fuse();

        pin_mut!(task);

        loop {
            select! {
                () = component_task => {},
                () = task => break,
            }
        }

        component_task
    }

    #[fuchsia::test(threads = 2)]
    async fn test_shutdown() {
        let component_task = run_test(|client, _| {
            let admin_proxy = connect_to_protocol_at_dir_svc::<AdminMarker>(client)
                .expect("Unable to connect to Admin protocol");
            async move {
                admin_proxy.shutdown().await.expect("shutdown failed");
            }
            .boxed()
        })
        .await;
        assert!(!component_task.is_terminated());
    }

    #[fuchsia::test(threads = 2)]
    async fn test_lifecycle_stop() {
        let component_task = run_test(|_, lifecycle_client| {
            lifecycle_client.stop().expect("Stop failed");
            async move {
                fasync::OnSignals::new(
                    &lifecycle_client.into_channel().expect("into_channel failed"),
                    zx::Signals::CHANNEL_PEER_CLOSED,
                )
                .await
                .expect("OnSignals failed");
            }
            .boxed()
        })
        .await;
        component_task.await;
    }

    #[fuchsia::test(threads = 2)]
    async fn test_create_and_remove() {
        run_test(|client, _| {
            let volumes_proxy = connect_to_protocol_at_dir_svc::<VolumesMarker>(client)
                .expect("Unable to connect to Volumes protocol");

            let fs_admin_proxy = connect_to_protocol_at_dir_svc::<AdminMarker>(client)
                .expect("Unable to connect to Admin protocol");

            async move {
                let (dir_proxy, server_end) =
                    fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
                        .expect("create_proxy failed");
                volumes_proxy
                    .create("test", server_end, CreateOptions::default(), MountOptions::default())
                    .await
                    .expect("fidl failed")
                    .expect("create failed");

                // This should fail whilst the volume is mounted.
                volumes_proxy
                    .remove("test")
                    .await
                    .expect("fidl failed")
                    .expect_err("remove succeeded");

                let volume_admin_proxy = connect_to_protocol_at_dir_svc::<AdminMarker>(&dir_proxy)
                    .expect("Unable to connect to Admin protocol");
                volume_admin_proxy.shutdown().await.expect("shutdown failed");

                // Creating another volume with the same name should fail.
                let (_dir_proxy, server_end) =
                    fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
                        .expect("create_proxy failed");
                volumes_proxy
                    .create("test", server_end, CreateOptions::default(), MountOptions::default())
                    .await
                    .expect("fidl failed")
                    .expect_err("create succeeded");

                volumes_proxy.remove("test").await.expect("fidl failed").expect("remove failed");

                // Removing a non-existent volume should fail.
                volumes_proxy
                    .remove("test")
                    .await
                    .expect("fidl failed")
                    .expect_err("remove failed");

                // Create the same volume again and it should now succeed.
                let (_dir_proxy, server_end) =
                    fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
                        .expect("create_proxy failed");
                volumes_proxy
                    .create("test", server_end, CreateOptions::default(), MountOptions::default())
                    .await
                    .expect("fidl failed")
                    .expect("create failed");

                fs_admin_proxy.shutdown().await.expect("shutdown failed");
            }
            .boxed()
        })
        .await
        .await;
    }

    #[fuchsia::test(threads = 2)]
    async fn test_volumes_enumeration() {
        run_test(|client, _| {
            let volumes_proxy = connect_to_protocol_at_dir_svc::<VolumesMarker>(client)
                .expect("Unable to connect to Volumes protocol");

            let (volumes_dir_proxy, server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
                    .expect("create_proxy failed");
            client
                .open(
                    fio::OpenFlags::RIGHT_READABLE,
                    fio::ModeType::empty(),
                    "volumes",
                    ServerEnd::new(server_end.into_channel()),
                )
                .expect("open failed");

            let fs_admin_proxy = connect_to_protocol_at_dir_svc::<AdminMarker>(client)
                .expect("Unable to connect to Admin protocol");

            async move {
                let (_dir_proxy, server_end) =
                    fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
                        .expect("create_proxy failed");
                volumes_proxy
                    .create("test", server_end, CreateOptions::default(), MountOptions::default())
                    .await
                    .expect("fidl failed")
                    .expect("create failed");

                let entries = readdir(&volumes_dir_proxy).await.expect("readdir failed");
                let mut entry_names = entries.iter().map(|d| d.name.as_str()).collect::<Vec<_>>();
                entry_names.sort();
                assert_eq!(entry_names, ["default", "test"]);

                fs_admin_proxy.shutdown().await.expect("shutdown failed");
            }
            .boxed()
        })
        .await
        .await;
    }
}
