// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::fuchsia::{
        component::map_to_raw_status,
        directory::FxDirectory,
        file::FxFile,
        memory_pressure::{MemoryPressureLevel, MemoryPressureMonitor},
        node::{FxNode, GetResult, NodeCache},
        pager::{Pager, PagerExecutor},
        symlink::FxSymlink,
        vmo_data_buffer::VmoDataBuffer,
        volumes_directory::VolumesDirectory,
    },
    anyhow::{bail, ensure, Error},
    async_trait::async_trait,
    fidl_fuchsia_fxfs::{
        BytesAndNodes, ProjectIdRequest, ProjectIdRequestStream, ProjectIterToken,
    },
    fidl_fuchsia_io as fio,
    fs_inspect::{FsInspectVolume, VolumeData},
    fuchsia_async as fasync,
    futures::{
        self,
        channel::oneshot,
        stream::{self, FusedStream, Stream},
        FutureExt, StreamExt, TryStreamExt,
    },
    fxfs::{
        errors::FxfsError,
        filesystem::{self, SyncOptions},
        log::*,
        object_store::{
            directory::{Directory, ObjectDescriptor},
            transaction::Options,
            HandleOptions, HandleOwner, ObjectStore,
        },
    },
    std::{
        boxed::Box,
        convert::TryInto,
        marker::Unpin,
        sync::{Arc, Mutex, Weak},
        time::Duration,
    },
    vfs::{directory::entry::DirectoryEntry, execution_scope::ExecutionScope},
};

#[derive(Clone)]
pub struct FlushTaskConfig {
    /// The period to wait between flushes at [`MemoryPressureLevel::Normal`].
    pub mem_normal_period: Duration,

    /// The period to wait between flushes at [`MemoryPressureLevel::Warning`].
    pub mem_warning_period: Duration,

    /// The period to wait between flushes at [`MemoryPressureLevel::Critical`].
    pub mem_critical_period: Duration,
}

impl FlushTaskConfig {
    pub fn flush_period_from_level(&self, level: &MemoryPressureLevel) -> Duration {
        match level {
            MemoryPressureLevel::Normal => self.mem_normal_period,
            MemoryPressureLevel::Warning => self.mem_warning_period,
            MemoryPressureLevel::Critical => self.mem_critical_period,
        }
    }
}

impl Default for FlushTaskConfig {
    fn default() -> Self {
        // TODO(https://fxbug.dev/110000): investigate a smarter strategy for determining flush
        // frequency.
        Self {
            mem_normal_period: Duration::from_secs(20),
            mem_warning_period: Duration::from_secs(5),
            mem_critical_period: Duration::from_millis(1500),
        }
    }
}

/// FxVolume represents an opened volume. It is also a (weak) cache for all opened Nodes within the
/// volume.
pub struct FxVolume {
    parent: Weak<VolumesDirectory>,
    cache: NodeCache,
    store: Arc<ObjectStore>,
    pager: Pager,
    executor: fasync::EHandle,

    // A tuple of the actual task and a channel to signal to terminate the task.
    flush_task: Mutex<Option<(fasync::Task<()>, oneshot::Sender<()>)>>,

    // Unique identifier of the filesystem that owns this volume.
    fs_id: u64,

    // The execution scope for this volume.
    scope: ExecutionScope,
}

impl FxVolume {
    pub fn new(
        parent: Weak<VolumesDirectory>,
        store: Arc<ObjectStore>,
        fs_id: u64,
    ) -> Result<Self, Error> {
        Ok(Self {
            parent,
            cache: NodeCache::new(),
            store,
            pager: Pager::new(PagerExecutor::global_instance())?,
            executor: fasync::EHandle::local(),
            flush_task: Mutex::new(None),
            fs_id,
            scope: ExecutionScope::new(),
        })
    }

    pub fn store(&self) -> &Arc<ObjectStore> {
        &self.store
    }

    pub fn cache(&self) -> &NodeCache {
        &self.cache
    }

    pub fn pager(&self) -> &Pager {
        &self.pager
    }

    pub fn executor(&self) -> &fasync::EHandle {
        &self.executor
    }

    pub fn id(&self) -> u64 {
        self.fs_id
    }

    pub fn scope(&self) -> &ExecutionScope {
        &self.scope
    }

    pub async fn terminate(&self) {
        self.cache.clear();
        self.scope.shutdown();
        self.scope.wait().await;
        self.pager.terminate().await;
        self.store.filesystem().graveyard().flush().await;
        let task = std::mem::replace(&mut *self.flush_task.lock().unwrap(), None);
        if let Some((task, terminate)) = task {
            let _ = terminate.send(());
            task.await;
        }
        self.flush_all_files().await;
        if self.store.crypt().is_some() {
            if let Err(e) = self.store.lock().await {
                // The store will be left in a safe state and there won't be data-loss unless
                // there's an issue flushing the journal later.
                warn!(error = e.as_value(), "Locking store error");
            }
        }
        let sync_status = self
            .store
            .filesystem()
            .sync(SyncOptions { flush_device: true, ..Default::default() })
            .await;
        if let Err(e) = sync_status {
            error!(error = e.as_value(), "Failed to sync filesystem; data may be lost");
        }
    }

    /// Attempts to get a node from the node cache. If the node wasn't present in the cache, loads
    /// the object from the object store, installing the returned node into the cache and returns
    /// the newly created FxNode backed by the loaded object.  |parent| is only set on the node if
    /// the node was not present in the cache.  Otherwise, it is ignored.
    pub async fn get_or_load_node(
        self: &Arc<Self>,
        object_id: u64,
        object_descriptor: ObjectDescriptor,
        parent: Option<Arc<FxDirectory>>,
    ) -> Result<Arc<dyn FxNode>, Error> {
        match self.cache.get_or_reserve(object_id).await {
            GetResult::Node(node) => Ok(node),
            GetResult::Placeholder(placeholder) => {
                let node = match object_descriptor {
                    ObjectDescriptor::File => FxFile::new(
                        ObjectStore::open_object(
                            self,
                            object_id,
                            HandleOptions::default(),
                            self.store().crypt(),
                        )
                        .await?,
                    ) as Arc<dyn FxNode>,
                    ObjectDescriptor::Directory => Arc::new(FxDirectory::new(
                        parent,
                        Directory::open_unchecked(self.clone(), object_id),
                    )) as Arc<dyn FxNode>,
                    ObjectDescriptor::Symlink => Arc::new(FxSymlink::new(self.clone(), object_id)),
                    _ => bail!(FxfsError::Inconsistent),
                };
                placeholder.commit(&node);
                Ok(node)
            }
        }
    }

    pub fn into_store(self) -> Arc<ObjectStore> {
        self.store
    }

    /// Marks the given directory deleted.
    pub fn mark_directory_deleted(&self, object_id: u64) {
        if let Some(node) = self.cache.get(object_id) {
            // It's possible that node is a placeholder, in which case we don't need to wait for it
            // to be resolved because it should be blocked behind the locks that are held by the
            // caller, and once they're dropped, it'll be found to be deleted via the tree.
            if let Ok(dir) = node.into_any().downcast::<FxDirectory>() {
                dir.set_deleted();
            }
        }
    }

    /// Removes resources associated with |object_id| (which ought to be a file), if there are no
    /// open connections to that file.
    ///
    /// This must be called *after committing* a transaction which deletes the last reference to
    /// |object_id|, since before that point, new connections could be established.
    pub(super) async fn maybe_purge_file(&self, object_id: u64) -> Result<(), Error> {
        if let Some(node) = self.cache.get(object_id) {
            if let Ok(file) = node.into_any().downcast::<FxFile>() {
                if !file.mark_purged() {
                    return Ok(());
                }
            }
        }
        // If this fails, the graveyard should clean it up on next mount.
        self.store
            .tombstone(object_id, Options { borrow_metadata_space: true, ..Default::default() })
            .await?;
        Ok(())
    }

    /// Starts the background flush task.  This task will periodically scan all files and flush them
    /// to disk.
    /// The task will hold a strong reference to the FxVolume while it is running, so the task must
    /// be closed later with Self::terminate, or the FxVolume will never be dropped.
    pub fn start_flush_task(
        self: &Arc<Self>,
        config: FlushTaskConfig,
        mem_monitor: Option<&MemoryPressureMonitor>,
    ) {
        let mut flush_task = self.flush_task.lock().unwrap();
        if flush_task.is_none() {
            let (tx, rx) = oneshot::channel();

            let task = if let Some(mem_monitor) = mem_monitor {
                fasync::Task::spawn(self.clone().flush_task(
                    config,
                    mem_monitor.get_level_stream(),
                    rx,
                ))
            } else {
                // With no memory pressure monitoring, just stub the stream out as always pending.
                fasync::Task::spawn(self.clone().flush_task(config, stream::pending(), rx))
            };

            *flush_task = Some((task, tx));
        }
    }

    async fn flush_task(
        self: Arc<Self>,
        config: FlushTaskConfig,
        mut level_stream: impl Stream<Item = MemoryPressureLevel> + FusedStream + Unpin,
        terminate: oneshot::Receiver<()>,
    ) {
        debug!(store_id = self.store.store_object_id(), "FxVolume::flush_task start");
        let mut terminate = terminate.fuse();
        // Default to the normal flush period until updates come from the `level_stream`.
        let mut level = MemoryPressureLevel::Normal;
        let mut timer = fasync::Timer::new(config.flush_period_from_level(&level)).fuse();

        loop {
            let mut should_terminate = false;
            let mut should_flush = false;

            futures::select_biased! {
                _ = terminate => should_terminate = true,
                new_level = level_stream.next() => {
                    // Because `level_stream` will never terminate, this is safe to unwrap.
                    let new_level = new_level.unwrap();
                    // At critical levels, it's okay to undertake expensive work immediately
                    // to reclaim memory.
                    should_flush = matches!(new_level, MemoryPressureLevel::Critical);
                    if new_level != level {
                        level = new_level;
                        timer = fasync::Timer::new(config.flush_period_from_level(&level)).fuse();
                        debug!(
                            "Background flush period changed to {:?} due to new memory pressure \
                            level ({:?}).",
                            config.flush_period_from_level(&level), level
                        );
                    }
                }
                _ = timer => {
                    timer = fasync::Timer::new(config.flush_period_from_level(&level)).fuse();
                    should_flush = true;
                }
            };
            if should_terminate {
                break;
            }

            if should_flush {
                self.flush_all_files().await;
            }
        }
        debug!(store_id = self.store.store_object_id(), "FxVolume::flush_task end");
    }

    /// Reports that a certain number of bytes will be dirtied in a pager-backed VMO.
    ///
    /// Note that this function may await flush tasks.
    pub async fn report_pager_dirty(&self, num_bytes: u64) {
        if let Some(parent) = self.parent.upgrade() {
            parent.report_pager_dirty(num_bytes).await;
        }
    }

    /// Reports that a certain number of bytes were cleaned in a pager-backed VMO.
    pub fn report_pager_clean(&self, num_bytes: u64) {
        if let Some(parent) = self.parent.upgrade() {
            parent.report_pager_clean(num_bytes);
        }
    }

    pub async fn flush_all_files(&self) {
        let mut flushed = 0;
        for file in self.cache.files() {
            if let Err(e) = file.flush().await {
                warn!(
                    store_id = self.store.store_object_id(),
                    oid = file.object_id(),
                    error = e.as_value(),
                    "Failed to flush",
                )
            }
            flushed += 1;
        }
        debug!(store_id = self.store.store_object_id(), file_count = flushed, "FxVolume flushed");
    }
}

impl HandleOwner for FxVolume {
    type Buffer = VmoDataBuffer;

    fn create_data_buffer(&self, object_id: u64, initial_size: u64) -> Self::Buffer {
        self.pager.create_vmo(object_id, initial_size).unwrap().try_into().unwrap()
    }
}

impl AsRef<ObjectStore> for FxVolume {
    fn as_ref(&self) -> &ObjectStore {
        &self.store
    }
}

#[async_trait]
impl FsInspectVolume for FxVolume {
    async fn get_volume_data(&self) -> VolumeData {
        let object_count = self.store().object_count();
        let (used_bytes, bytes_limit) =
            self.store.filesystem().allocator().owner_allocation_info(self.store.store_object_id());
        let encrypted = self.store().crypt().is_some();
        VolumeData { bytes_limit, used_bytes, used_nodes: object_count, encrypted }
    }
}

pub trait RootDir: FxNode + DirectoryEntry {
    fn as_directory_entry(self: Arc<Self>) -> Arc<dyn DirectoryEntry>;

    fn as_node(self: Arc<Self>) -> Arc<dyn FxNode>;
}

impl<T: FxNode + DirectoryEntry> RootDir for T {
    fn as_directory_entry(self: Arc<Self>) -> Arc<dyn DirectoryEntry> {
        self as Arc<dyn DirectoryEntry>
    }

    fn as_node(self: Arc<Self>) -> Arc<dyn FxNode> {
        self as Arc<dyn FxNode>
    }
}

#[derive(Clone)]
pub struct FxVolumeAndRoot {
    volume: Arc<FxVolume>,
    root: Arc<dyn RootDir>,
}

impl FxVolumeAndRoot {
    pub async fn new<T: From<Directory<FxVolume>> + RootDir>(
        _parent: Weak<VolumesDirectory>,
        store: Arc<ObjectStore>,
        unique_id: u64,
    ) -> Result<Self, Error> {
        let volume = Arc::new(FxVolume::new(Weak::new(), store, unique_id)?);
        let root_object_id = volume.store().root_directory_object_id();
        let root_dir = Directory::open(&volume, root_object_id).await?;
        let root = Arc::<T>::new(root_dir.into()) as Arc<dyn RootDir>;
        volume
            .cache
            .get_or_reserve(root_object_id)
            .await
            .placeholder()
            .unwrap()
            .commit(&root.clone().as_node());
        Ok(Self { volume, root })
    }

    pub fn volume(&self) -> &Arc<FxVolume> {
        &self.volume
    }

    pub fn root(&self) -> &Arc<dyn RootDir> {
        &self.root
    }

    // The same as root but downcasted to FxDirectory.
    pub fn root_dir(&self) -> Arc<FxDirectory> {
        self.root().clone().into_any().downcast::<FxDirectory>().expect("Invalid type for root")
    }

    pub async fn handle_project_id_requests(
        &self,
        mut requests: ProjectIdRequestStream,
    ) -> Result<(), Error> {
        let store_id = self.volume.store.store_object_id();
        while let Some(request) = requests.try_next().await? {
            match request {
                ProjectIdRequest::SetLimit { responder, project_id, bytes, nodes } => responder
                    .send(
                        self.volume
                            .store()
                            .set_project_limit(project_id, bytes, nodes)
                            .await
                            .map_err(|error| {
                                error!(?error, store_id, project_id, "Failed to set project limit");
                                map_to_raw_status(error)
                            }),
                    )?,
                ProjectIdRequest::Clear { responder, project_id } => responder.send(
                    self.volume.store().clear_project_limit(project_id).await.map_err(|error| {
                        error!(?error, store_id, project_id, "Failed to clear project limit");
                        map_to_raw_status(error)
                    }),
                )?,
                ProjectIdRequest::SetForNode { responder, node_id, project_id } => responder.send(
                    self.volume.store().set_project_for_node(node_id, project_id).await.map_err(
                        |error| {
                            error!(?error, store_id, node_id, project_id, "Failed to apply node.");
                            map_to_raw_status(error)
                        },
                    ),
                )?,
                ProjectIdRequest::GetForNode { responder, node_id } => responder.send(
                    self.volume.store().get_project_for_node(node_id).await.map_err(|error| {
                        error!(?error, store_id, node_id, "Failed to get node.");
                        map_to_raw_status(error)
                    }),
                )?,
                ProjectIdRequest::ClearForNode { responder, node_id } => responder.send(
                    self.volume.store().clear_project_for_node(node_id).await.map_err(|error| {
                        error!(?error, store_id, node_id, "Failed to clear for node.");
                        map_to_raw_status(error)
                    }),
                )?,
                ProjectIdRequest::List { responder, token } => {
                    responder.send(match self.list_projects(&token).await {
                        Ok((ref entries, ref next_token)) => Ok((entries, next_token.as_ref())),
                        Err(error) => {
                            error!(?error, store_id, ?token, "Failed to list projects.");
                            Err(map_to_raw_status(error))
                        }
                    })?
                }
                ProjectIdRequest::Info { responder, project_id } => {
                    responder.send(match self.project_info(project_id).await {
                        Ok((ref limit, ref usage)) => Ok((limit, usage)),
                        Err(error) => {
                            error!(?error, store_id, project_id, "Failed to get project info.");
                            Err(map_to_raw_status(error))
                        }
                    })?
                }
            }
        }
        Ok(())
    }

    pub fn into_volume(self) -> Arc<FxVolume> {
        self.volume
    }

    // Maximum entries to fit based on 64KiB message size minus 16 bytes of header, 16 bytes
    // of vector header, 16 bytes for the optional token header, and 8 bytes of token value.
    // https://fuchsia.dev/fuchsia-src/development/languages/fidl/guides/max-out-pagination
    const MAX_PROJECT_ENTRIES: usize = 8184;

    // Calls out to the inner volume to list available projects, removing and re-adding the fidl
    // wrapper types for the pagination token.
    async fn list_projects(
        &self,
        last_token: &Option<Box<ProjectIterToken>>,
    ) -> Result<(Vec<u64>, Option<ProjectIterToken>), Error> {
        let (entries, token) = self
            .volume
            .store()
            .list_projects(
                match last_token {
                    None => 0,
                    Some(v) => v.value,
                },
                Self::MAX_PROJECT_ENTRIES,
            )
            .await?;
        Ok((entries, token.map(|value| ProjectIterToken { value })))
    }

    async fn project_info(&self, project_id: u64) -> Result<(BytesAndNodes, BytesAndNodes), Error> {
        let (limit, usage) = self.volume.store().project_info(project_id).await?;
        // At least one of them needs to be around to return anything.
        ensure!(limit.is_some() || usage.is_some(), FxfsError::NotFound);
        Ok((
            limit.map_or_else(
                || BytesAndNodes { bytes: u64::MAX, nodes: u64::MAX },
                |v| BytesAndNodes { bytes: v.0, nodes: v.1 },
            ),
            usage.map_or_else(
                || BytesAndNodes { bytes: 0, nodes: 0 },
                |v| BytesAndNodes { bytes: v.0, nodes: v.1 },
            ),
        ))
    }
}

// The correct number here is arguably u64::MAX - 1 (because node 0 is reserved). There's a bug
// where inspect test cases fail if we try and use that, possibly because of a signed/unsigned bug.
// See fxbug.dev/87152.  Until that's fixed, we'll have to use i64::MAX.
const TOTAL_NODES: u64 = i64::MAX as u64;

// An array used to initialize the FilesystemInfo |name| field. This just spells "fxfs" 0-padded to
// 32 bytes.
const FXFS_INFO_NAME_FIDL: [i8; 32] = [
    0x66, 0x78, 0x66, 0x73, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0,
];

pub fn info_to_filesystem_info(
    info: filesystem::Info,
    block_size: u64,
    object_count: u64,
    fs_id: u64,
) -> fio::FilesystemInfo {
    fio::FilesystemInfo {
        total_bytes: info.total_bytes,
        used_bytes: info.used_bytes,
        total_nodes: TOTAL_NODES,
        used_nodes: object_count,
        // TODO(fxbug.dev/93770): Support free_shared_pool_bytes.
        free_shared_pool_bytes: 0,
        fs_id,
        block_size: block_size as u32,
        max_filename_size: fio::MAX_FILENAME as u32,
        fs_type: fidl_fuchsia_fs::VfsType::Fxfs.into_primitive(),
        padding: 0,
        name: FXFS_INFO_NAME_FIDL,
    }
}

#[cfg(test)]
mod tests {
    use {
        crate::fuchsia::{
            directory::FxDirectory,
            file::FxFile,
            memory_pressure::{MemoryPressureLevel, MemoryPressureMonitor},
            testing::{
                close_dir_checked, close_file_checked, open_dir, open_dir_checked, open_file,
                open_file_checked, write_at, TestFixture,
            },
            volume::{FlushTaskConfig, FxVolumeAndRoot},
            volumes_directory::VolumesDirectory,
        },
        fidl::endpoints::ServerEnd,
        fidl_fuchsia_fxfs::{BytesAndNodes, ProjectIdMarker, VolumeMarker},
        fidl_fuchsia_io as fio, fuchsia_async as fasync,
        fuchsia_component::client::connect_to_protocol_at_dir_svc,
        fuchsia_fs::file,
        fuchsia_zircon::Status,
        fxfs::{
            filesystem::{Filesystem, FxFilesystem},
            fsck::{fsck, fsck_volume},
            object_handle::{ObjectHandle, ObjectHandleExt},
            object_store::{
                directory::ObjectDescriptor,
                transaction::{Options, TransactionHandler},
                volume::root_volume,
                HandleOptions, ObjectStore,
            },
        },
        fxfs_insecure_crypto::InsecureCrypt,
        std::{
            sync::{Arc, Weak},
            time::Duration,
        },
        storage_device::{fake_device::FakeDevice, DeviceHolder},
        vfs::{directory::entry::DirectoryEntry, execution_scope::ExecutionScope, path::Path},
    };

    #[fuchsia::test(threads = 10)]
    async fn test_rename_different_dirs() {
        use fuchsia_zircon::Event;

        let fixture = TestFixture::new().await;
        let root = fixture.root();

        let src = open_dir_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::DIRECTORY,
            "foo",
        )
        .await;

        let dst = open_dir_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::DIRECTORY,
            "bar",
        )
        .await;

        let f = open_file_checked(
            &root,
            fio::OpenFlags::CREATE | fio::OpenFlags::NOT_DIRECTORY,
            "foo/a",
        )
        .await;
        close_file_checked(f).await;

        let (status, dst_token) = dst.get_token().await.expect("FIDL call failed");
        Status::ok(status).expect("get_token failed");
        src.rename("a", Event::from(dst_token.unwrap()), "b")
            .await
            .expect("FIDL call failed")
            .expect("rename failed");

        assert_eq!(
            open_file(&root, fio::OpenFlags::NOT_DIRECTORY, "foo/a")
                .await
                .expect_err("Open succeeded")
                .root_cause()
                .downcast_ref::<Status>()
                .expect("No status"),
            &Status::NOT_FOUND,
        );
        let f = open_file_checked(&root, fio::OpenFlags::NOT_DIRECTORY, "bar/b").await;
        close_file_checked(f).await;

        close_dir_checked(dst).await;
        close_dir_checked(src).await;
        fixture.close().await;
    }

    #[fuchsia::test(threads = 10)]
    async fn test_rename_same_dir() {
        use fuchsia_zircon::Event;
        let fixture = TestFixture::new().await;
        let root = fixture.root();

        let src = open_dir_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::DIRECTORY,
            "foo",
        )
        .await;

        let f = open_file_checked(
            &root,
            fio::OpenFlags::CREATE | fio::OpenFlags::NOT_DIRECTORY,
            "foo/a",
        )
        .await;
        close_file_checked(f).await;

        let (status, src_token) = src.get_token().await.expect("FIDL call failed");
        Status::ok(status).expect("get_token failed");
        src.rename("a", Event::from(src_token.unwrap()), "b")
            .await
            .expect("FIDL call failed")
            .expect("rename failed");

        assert_eq!(
            open_file(&root, fio::OpenFlags::NOT_DIRECTORY, "foo/a")
                .await
                .expect_err("Open succeeded")
                .root_cause()
                .downcast_ref::<Status>()
                .expect("No status"),
            &Status::NOT_FOUND,
        );
        let f = open_file_checked(&root, fio::OpenFlags::NOT_DIRECTORY, "foo/b").await;
        close_file_checked(f).await;

        close_dir_checked(src).await;
        fixture.close().await;
    }

    #[fuchsia::test(threads = 10)]
    async fn test_rename_overwrites_file() {
        use fuchsia_zircon::Event;
        let fixture = TestFixture::new().await;
        let root = fixture.root();

        let src = open_dir_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::DIRECTORY,
            "foo",
        )
        .await;

        let dst = open_dir_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::DIRECTORY,
            "bar",
        )
        .await;

        // The src file is non-empty.
        let src_file = open_file_checked(
            &root,
            fio::OpenFlags::CREATE | fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::NOT_DIRECTORY,
            "foo/a",
        )
        .await;
        let buf = vec![0xaa as u8; 8192];
        file::write(&src_file, buf.as_slice()).await.expect("Failed to write to file");
        close_file_checked(src_file).await;

        // The dst file is empty (so we can distinguish it).
        let f = open_file_checked(
            &root,
            fio::OpenFlags::CREATE | fio::OpenFlags::NOT_DIRECTORY,
            "bar/b",
        )
        .await;
        close_file_checked(f).await;

        let (status, dst_token) = dst.get_token().await.expect("FIDL call failed");
        Status::ok(status).expect("get_token failed");
        src.rename("a", Event::from(dst_token.unwrap()), "b")
            .await
            .expect("FIDL call failed")
            .expect("rename failed");

        assert_eq!(
            open_file(&root, fio::OpenFlags::NOT_DIRECTORY, "foo/a")
                .await
                .expect_err("Open succeeded")
                .root_cause()
                .downcast_ref::<Status>()
                .expect("No status"),
            &Status::NOT_FOUND,
        );
        let file = open_file_checked(
            &root,
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::NOT_DIRECTORY,
            "bar/b",
        )
        .await;
        let buf = file::read(&file).await.expect("read file failed");
        assert_eq!(buf, vec![0xaa as u8; 8192]);
        close_file_checked(file).await;

        close_dir_checked(dst).await;
        close_dir_checked(src).await;
        fixture.close().await;
    }

    #[fuchsia::test(threads = 10)]
    async fn test_rename_overwrites_dir() {
        use fuchsia_zircon::Event;
        let fixture = TestFixture::new().await;
        let root = fixture.root();

        let src = open_dir_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::DIRECTORY,
            "foo",
        )
        .await;

        let dst = open_dir_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::DIRECTORY,
            "bar",
        )
        .await;

        // The src dir is non-empty.
        open_dir_checked(
            &root,
            fio::OpenFlags::CREATE | fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::DIRECTORY,
            "foo/a",
        )
        .await;
        open_file_checked(
            &root,
            fio::OpenFlags::CREATE | fio::OpenFlags::NOT_DIRECTORY,
            "foo/a/file",
        )
        .await;
        open_dir_checked(&root, fio::OpenFlags::CREATE | fio::OpenFlags::DIRECTORY, "bar/b").await;

        let (status, dst_token) = dst.get_token().await.expect("FIDL call failed");
        Status::ok(status).expect("get_token failed");
        src.rename("a", Event::from(dst_token.unwrap()), "b")
            .await
            .expect("FIDL call failed")
            .expect("rename failed");

        assert_eq!(
            open_dir(&root, fio::OpenFlags::DIRECTORY, "foo/a")
                .await
                .expect_err("Open succeeded")
                .root_cause()
                .downcast_ref::<Status>()
                .expect("No status"),
            &Status::NOT_FOUND,
        );
        let f = open_file_checked(&root, fio::OpenFlags::NOT_DIRECTORY, "bar/b/file").await;
        close_file_checked(f).await;

        close_dir_checked(dst).await;
        close_dir_checked(src).await;

        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_background_flush() {
        // We have to do a bit of set-up ourselves for this test, since we want to be able to access
        // the underlying StoreObjectHandle at the same time as the FxFile which corresponds to it.
        let device = DeviceHolder::new(FakeDevice::new(8192, 512));
        let filesystem = FxFilesystem::new_empty(device).await.unwrap();
        {
            let root_volume = root_volume(filesystem.clone()).await.unwrap();
            let volume =
                root_volume.new_volume("vol", Some(Arc::new(InsecureCrypt::new()))).await.unwrap();
            let mut transaction = filesystem
                .clone()
                .new_transaction(&[], Options::default())
                .await
                .expect("new_transaction failed");
            let object_id = ObjectStore::create_object(
                &volume,
                &mut transaction,
                HandleOptions::default(),
                None,
            )
            .await
            .expect("create_object failed")
            .object_id();
            transaction.commit().await.expect("commit failed");
            let vol =
                FxVolumeAndRoot::new::<FxDirectory>(Weak::new(), volume.clone(), 0).await.unwrap();

            let file = vol
                .volume()
                .get_or_load_node(object_id, ObjectDescriptor::File, None)
                .await
                .expect("get_or_load_node failed")
                .into_any()
                .downcast::<FxFile>()
                .expect("Not a file");

            // Write some data to the file, which will only go to the cache for now.
            write_at(&file, 0, &[123u8]).expect("write_at failed");

            let data_has_persisted = || async {
                // We have to reopen the object each time since this is a distinct handle from the
                // one managed by the FxFile.
                let object =
                    ObjectStore::open_object(&volume, object_id, HandleOptions::default(), None)
                        .await
                        .expect("open_object failed");
                let data = object.contents(8192).await.expect("read failed");
                data.len() == 1 && data[..] == [123u8]
            };
            assert!(!data_has_persisted().await);

            vol.volume().start_flush_task(
                FlushTaskConfig {
                    mem_normal_period: Duration::from_millis(100),
                    mem_warning_period: Duration::from_millis(100),
                    mem_critical_period: Duration::from_millis(100),
                },
                None,
            );

            let mut wait = 100;
            loop {
                if data_has_persisted().await {
                    break;
                }
                fasync::Timer::new(Duration::from_millis(wait)).await;
                wait *= 2;
            }

            vol.volume().terminate().await;
        }

        filesystem.close().await.expect("close filesystem failed");
        let device = filesystem.take_device().await;
        device.ensure_unique();
    }

    #[fuchsia::test(threads = 2)]
    async fn test_background_flush_with_warning_memory_pressure() {
        // We have to do a bit of set-up ourselves for this test, since we want to be able to access
        // the underlying StoreObjectHandle at the same time as the FxFile which corresponds to it.
        let device = DeviceHolder::new(FakeDevice::new(8192, 512));
        let filesystem = FxFilesystem::new_empty(device).await.unwrap();
        {
            let root_volume = root_volume(filesystem.clone()).await.unwrap();
            let volume =
                root_volume.new_volume("vol", Some(Arc::new(InsecureCrypt::new()))).await.unwrap();
            let mut transaction = filesystem
                .clone()
                .new_transaction(&[], Options::default())
                .await
                .expect("new_transaction failed");
            let object_id = ObjectStore::create_object(
                &volume,
                &mut transaction,
                HandleOptions::default(),
                None,
            )
            .await
            .expect("create_object failed")
            .object_id();
            transaction.commit().await.expect("commit failed");
            let vol =
                FxVolumeAndRoot::new::<FxDirectory>(Weak::new(), volume.clone(), 0).await.unwrap();

            let file = vol
                .volume()
                .get_or_load_node(object_id, ObjectDescriptor::File, None)
                .await
                .expect("get_or_load_node failed")
                .into_any()
                .downcast::<FxFile>()
                .expect("Not a file");

            // Write some data to the file, which will only go to the cache for now.
            write_at(&file, 0, &[123u8]).expect("write_at failed");

            let data_has_persisted = || async {
                // We have to reopen the object each time since this is a distinct handle from the
                // one managed by the FxFile.
                let object =
                    ObjectStore::open_object(&volume, object_id, HandleOptions::default(), None)
                        .await
                        .expect("open_object failed");
                let data = object.contents(8192).await.expect("read failed");
                data.len() == 1 && data[..] == [123u8]
            };
            assert!(!data_has_persisted().await);

            let (watcher_proxy, watcher_server) =
                fidl::endpoints::create_proxy().expect("Failed to create FIDL endpoints");
            let mem_pressure = MemoryPressureMonitor::try_from(watcher_server)
                .expect("Failed to create MemoryPressureMonitor");

            // Configure the flush task to only flush quickly on warning.
            let flush_config = FlushTaskConfig {
                mem_normal_period: Duration::from_secs(20),
                mem_warning_period: Duration::from_millis(100),
                mem_critical_period: Duration::from_secs(20),
            };
            vol.volume().start_flush_task(flush_config, Some(&mem_pressure));

            // Send the memory pressure update.
            let _ = watcher_proxy
                .on_level_changed(MemoryPressureLevel::Warning)
                .await
                .expect("Failed to send memory pressure level change");

            // Wait a bit of time for the flush to occur (but less than the normal and critical
            // periods).
            const MAX_WAIT: Duration = Duration::from_secs(3);
            let wait_increments = Duration::from_millis(400);
            let mut total_waited = Duration::ZERO;

            while total_waited < MAX_WAIT {
                fasync::Timer::new(wait_increments).await;
                total_waited += wait_increments;

                if data_has_persisted().await {
                    break;
                }
            }

            assert!(data_has_persisted().await);

            vol.volume().terminate().await;
        }

        filesystem.close().await.expect("close filesystem failed");
        let device = filesystem.take_device().await;
        device.ensure_unique();
    }

    #[fuchsia::test(threads = 2)]
    async fn test_background_flush_with_critical_memory_pressure() {
        // We have to do a bit of set-up ourselves for this test, since we want to be able to access
        // the underlying StoreObjectHandle at the same time as the FxFile which corresponds to it.
        let device = DeviceHolder::new(FakeDevice::new(8192, 512));
        let filesystem = FxFilesystem::new_empty(device).await.unwrap();
        {
            let root_volume = root_volume(filesystem.clone()).await.unwrap();
            let volume =
                root_volume.new_volume("vol", Some(Arc::new(InsecureCrypt::new()))).await.unwrap();
            let mut transaction = filesystem
                .clone()
                .new_transaction(&[], Options::default())
                .await
                .expect("new_transaction failed");
            let object_id = ObjectStore::create_object(
                &volume,
                &mut transaction,
                HandleOptions::default(),
                None,
            )
            .await
            .expect("create_object failed")
            .object_id();
            transaction.commit().await.expect("commit failed");
            let vol =
                FxVolumeAndRoot::new::<FxDirectory>(Weak::new(), volume.clone(), 0).await.unwrap();

            let file = vol
                .volume()
                .get_or_load_node(object_id, ObjectDescriptor::File, None)
                .await
                .expect("get_or_load_node failed")
                .into_any()
                .downcast::<FxFile>()
                .expect("Not a file");

            // Write some data to the file, which will only go to the cache for now.
            write_at(&file, 0, &[123u8]).expect("write_at failed");

            let data_has_persisted = || async {
                // We have to reopen the object each time since this is a distinct handle from the
                // one managed by the FxFile.
                let object =
                    ObjectStore::open_object(&volume, object_id, HandleOptions::default(), None)
                        .await
                        .expect("open_object failed");
                let data = object.contents(8192).await.expect("read failed");
                data.len() == 1 && data[..] == [123u8]
            };
            assert!(!data_has_persisted().await);

            let (watcher_proxy, watcher_server) =
                fidl::endpoints::create_proxy().expect("Failed to create FIDL endpoints");
            let mem_pressure = MemoryPressureMonitor::try_from(watcher_server)
                .expect("Failed to create MemoryPressureMonitor");

            // Configure the flush task to only flush quickly on warning.
            let flush_config = FlushTaskConfig {
                mem_normal_period: Duration::from_secs(20),
                mem_warning_period: Duration::from_secs(20),
                mem_critical_period: Duration::from_secs(20),
            };
            vol.volume().start_flush_task(flush_config, Some(&mem_pressure));

            // Send the memory pressure update.
            watcher_proxy
                .on_level_changed(MemoryPressureLevel::Critical)
                .await
                .expect("Failed to send memory pressure level change");

            // Critical memory should trigger a flush immediately so expect a flush very quickly.
            const MAX_WAIT: Duration = Duration::from_secs(2);
            let wait_increments = Duration::from_millis(400);
            let mut total_waited = Duration::ZERO;

            while total_waited < MAX_WAIT {
                fasync::Timer::new(wait_increments).await;
                total_waited += wait_increments;

                if data_has_persisted().await {
                    break;
                }
            }

            assert!(data_has_persisted().await);

            vol.volume().terminate().await;
        }

        filesystem.close().await.expect("close filesystem failed");
        let device = filesystem.take_device().await;
        device.ensure_unique();
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_project_limit_persistence() {
        const BYTES_LIMIT_1: u64 = 123456;
        const NODES_LIMIT_1: u64 = 4321;
        const BYTES_LIMIT_2: u64 = 456789;
        const NODES_LIMIT_2: u64 = 9876;
        const VOLUME_NAME: &str = "A";
        const FILE_NAME: &str = "B";
        const PROJECT_ID: u64 = 42;
        let volume_store_id;
        let node_id;
        let mut device = DeviceHolder::new(FakeDevice::new(8192, 512));
        {
            let filesystem = FxFilesystem::new_empty(device).await.unwrap();
            let volumes_directory = VolumesDirectory::new(
                root_volume(filesystem.clone()).await.unwrap(),
                Weak::new(),
                None,
            )
            .await
            .unwrap();

            let volume_and_root = volumes_directory
                .create_volume(VOLUME_NAME, None)
                .await
                .expect("create unencrypted volume failed");
            volume_store_id = volume_and_root.volume().store().store_object_id();

            let (volume_proxy, volume_server_end) =
                fidl::endpoints::create_proxy::<VolumeMarker>().expect("Create proxy to succeed");
            volumes_directory.directory_node().clone().open(
                ExecutionScope::new(),
                fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
                Path::validate_and_split(VOLUME_NAME).unwrap(),
                volume_server_end.into_channel().into(),
            );

            let (volume_dir_proxy, dir_server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
                    .expect("Create dir proxy to succeed");
            volumes_directory
                .serve_volume(&volume_and_root, dir_server_end, false)
                .await
                .expect("serve_volume failed");

            let project_proxy =
                connect_to_protocol_at_dir_svc::<ProjectIdMarker>(&volume_dir_proxy)
                    .expect("Unable to connect to project id service");

            project_proxy
                .set_limit(0, BYTES_LIMIT_1, NODES_LIMIT_1)
                .await
                .unwrap()
                .expect_err("Should not set limits for project id 0");

            project_proxy
                .set_limit(PROJECT_ID, BYTES_LIMIT_1, NODES_LIMIT_1)
                .await
                .unwrap()
                .expect("To set limits");
            {
                let BytesAndNodes { bytes, nodes } =
                    project_proxy.info(PROJECT_ID).await.unwrap().expect("Fetching project info").0;
                assert_eq!(bytes, BYTES_LIMIT_1);
                assert_eq!(nodes, NODES_LIMIT_1);
            }

            let file_proxy = {
                let (root_proxy, root_server_end) =
                    fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
                        .expect("Create dir proxy to succeed");
                volume_dir_proxy
                    .open(
                        fio::OpenFlags::RIGHT_READABLE
                            | fio::OpenFlags::RIGHT_WRITABLE
                            | fio::OpenFlags::DIRECTORY,
                        fio::ModeType::empty(),
                        "root",
                        ServerEnd::new(root_server_end.into_channel()),
                    )
                    .expect("Failed to open volume root");

                open_file_checked(
                    &root_proxy,
                    fio::OpenFlags::CREATE
                        | fio::OpenFlags::RIGHT_READABLE
                        | fio::OpenFlags::RIGHT_WRITABLE
                        | fio::OpenFlags::NOT_DIRECTORY,
                    FILE_NAME,
                )
                .await
            };

            node_id = file_proxy.get_attr().await.unwrap().1.id;

            project_proxy
                .set_for_node(node_id, 0)
                .await
                .unwrap()
                .expect_err("Should not set 0 project id");

            project_proxy
                .set_for_node(node_id, PROJECT_ID)
                .await
                .unwrap()
                .expect("Setting project on node");

            project_proxy
                .set_for_node(node_id, PROJECT_ID)
                .await
                .unwrap()
                .expect_err("Should not be able to reset project for node.");

            project_proxy
                .set_limit(PROJECT_ID, BYTES_LIMIT_2, NODES_LIMIT_2)
                .await
                .unwrap()
                .expect("To set limits");
            {
                let BytesAndNodes { bytes, nodes } =
                    project_proxy.info(PROJECT_ID).await.unwrap().expect("Fetching project info").0;
                assert_eq!(bytes, BYTES_LIMIT_2);
                assert_eq!(nodes, NODES_LIMIT_2);
            }

            assert_eq!(
                project_proxy.get_for_node(node_id).await.unwrap().expect("Checking project"),
                PROJECT_ID
            );

            std::mem::drop(volume_proxy);
            volumes_directory.terminate().await;
            std::mem::drop(volumes_directory);
            filesystem.close().await.expect("close filesystem failed");
            device = filesystem.take_device().await;
        }
        {
            device.ensure_unique();
            device.reopen(false);
            let filesystem = FxFilesystem::open(device as DeviceHolder).await.unwrap();
            fsck(filesystem.clone()).await.expect("Fsck");
            fsck_volume(filesystem.as_ref(), volume_store_id, None).await.expect("Fsck volume");
            let volumes_directory = VolumesDirectory::new(
                root_volume(filesystem.clone()).await.unwrap(),
                Weak::new(),
                None,
            )
            .await
            .unwrap();
            let volume_and_root = volumes_directory
                .mount_volume(VOLUME_NAME, None)
                .await
                .expect("mount unencrypted volume failed");
            let (volume_proxy, volume_server_end) =
                fidl::endpoints::create_proxy::<VolumeMarker>().expect("Create proxy to succeed");
            volumes_directory.directory_node().clone().open(
                ExecutionScope::new(),
                fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
                Path::validate_and_split(VOLUME_NAME).unwrap(),
                volume_server_end.into_channel().into(),
            );

            let project_proxy = {
                let (volume_dir_proxy, dir_server_end) =
                    fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
                        .expect("Create dir proxy to succeed");
                volumes_directory
                    .serve_volume(&volume_and_root, dir_server_end, false)
                    .await
                    .expect("serve_volume failed");

                connect_to_protocol_at_dir_svc::<ProjectIdMarker>(&volume_dir_proxy)
                    .expect("Unable to connect to project id service")
            };

            {
                let BytesAndNodes { bytes, nodes } =
                    project_proxy.info(PROJECT_ID).await.unwrap().expect("Fetching project info").0;
                assert_eq!(bytes, BYTES_LIMIT_2);
                assert_eq!(nodes, NODES_LIMIT_2);
            }

            // Should be unable to clear the project limit, due to being in use.
            project_proxy.clear(PROJECT_ID).await.unwrap().expect("To clear limits");

            assert_eq!(
                project_proxy.get_for_node(node_id).await.unwrap().expect("Checking project"),
                PROJECT_ID
            );
            project_proxy.clear_for_node(node_id).await.unwrap().expect("Clearing project");
            assert_eq!(
                project_proxy.get_for_node(node_id).await.unwrap().expect("Checking project"),
                0
            );

            assert_eq!(
                project_proxy.info(PROJECT_ID).await.unwrap().expect_err("Expect missing limits"),
                Status::NOT_FOUND.into_raw()
            );

            std::mem::drop(volume_proxy);
            volumes_directory.terminate().await;
            std::mem::drop(volumes_directory);
            filesystem.close().await.expect("close filesystem failed");
            device = filesystem.take_device().await;
        }
        device.ensure_unique();
        device.reopen(false);
        let filesystem = FxFilesystem::open(device as DeviceHolder).await.unwrap();
        fsck(filesystem.clone()).await.expect("Fsck");
        fsck_volume(filesystem.as_ref(), volume_store_id, None).await.expect("Fsck volume");
        let volumes_directory = VolumesDirectory::new(
            root_volume(filesystem.clone()).await.unwrap(),
            Weak::new(),
            None,
        )
        .await
        .unwrap();
        let volume_and_root = volumes_directory
            .mount_volume(VOLUME_NAME, None)
            .await
            .expect("mount unencrypted volume failed");
        let (volume_dir_proxy, dir_server_end) =
            fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
                .expect("Create dir proxy to succeed");
        volumes_directory
            .serve_volume(&volume_and_root, dir_server_end, false)
            .await
            .expect("serve_volume failed");
        let project_proxy = connect_to_protocol_at_dir_svc::<ProjectIdMarker>(&volume_dir_proxy)
            .expect("Unable to connect to project id service");
        assert_eq!(
            project_proxy.info(PROJECT_ID).await.unwrap().expect_err("Expect missing limits"),
            Status::NOT_FOUND.into_raw()
        );
        volumes_directory.terminate().await;
        std::mem::drop(volumes_directory);
        filesystem.close().await.expect("close filesystem failed");
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_project_limit_accounting() {
        const BYTES_LIMIT: u64 = 123456;
        const NODES_LIMIT: u64 = 4321;
        const VOLUME_NAME: &str = "A";
        const FILE_NAME: &str = "B";
        const PROJECT_ID: u64 = 42;
        let volume_store_id;
        let mut device = DeviceHolder::new(FakeDevice::new(8192, 512));
        let first_object_id;
        let mut bytes_usage;
        {
            let filesystem = FxFilesystem::new_empty(device).await.unwrap();
            let volumes_directory = VolumesDirectory::new(
                root_volume(filesystem.clone()).await.unwrap(),
                Weak::new(),
                None,
            )
            .await
            .unwrap();

            let volume_and_root = volumes_directory
                .create_volume(VOLUME_NAME, Some(Arc::new(InsecureCrypt::new())))
                .await
                .expect("create unencrypted volume failed");
            volume_store_id = volume_and_root.volume().store().store_object_id();

            let (volume_proxy, volume_server_end) =
                fidl::endpoints::create_proxy::<VolumeMarker>().expect("Create proxy to succeed");
            volumes_directory.directory_node().clone().open(
                ExecutionScope::new(),
                fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
                Path::validate_and_split(VOLUME_NAME).unwrap(),
                volume_server_end.into_channel().into(),
            );

            let (volume_dir_proxy, dir_server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
                    .expect("Create dir proxy to succeed");
            volumes_directory
                .serve_volume(&volume_and_root, dir_server_end, false)
                .await
                .expect("serve_volume failed");

            let project_proxy =
                connect_to_protocol_at_dir_svc::<ProjectIdMarker>(&volume_dir_proxy)
                    .expect("Unable to connect to project id service");

            project_proxy
                .set_limit(PROJECT_ID, BYTES_LIMIT, NODES_LIMIT)
                .await
                .unwrap()
                .expect("To set limits");
            {
                let BytesAndNodes { bytes, nodes } =
                    project_proxy.info(PROJECT_ID).await.unwrap().expect("Fetching project info").0;
                assert_eq!(bytes, BYTES_LIMIT);
                assert_eq!(nodes, NODES_LIMIT);
            }

            let file_proxy = {
                let (root_proxy, root_server_end) =
                    fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
                        .expect("Create dir proxy to succeed");
                volume_dir_proxy
                    .open(
                        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
                        fio::ModeType::empty(),
                        "root",
                        ServerEnd::new(root_server_end.into_channel()),
                    )
                    .expect("Failed to open volume root");

                open_file_checked(
                    &root_proxy,
                    fio::OpenFlags::CREATE
                        | fio::OpenFlags::RIGHT_READABLE
                        | fio::OpenFlags::RIGHT_WRITABLE,
                    FILE_NAME,
                )
                .await
            };

            assert_eq!(
                8192,
                file_proxy
                    .write(&vec![0xff as u8; 8192])
                    .await
                    .expect("FIDL call failed")
                    .map_err(Status::from_raw)
                    .expect("File write was successful")
            );
            file_proxy.sync().await.expect("FIDL call failed").expect("Sync failed.");

            {
                let BytesAndNodes { bytes, nodes } =
                    project_proxy.info(PROJECT_ID).await.unwrap().expect("Fetching project info").1;
                assert_eq!(bytes, 0);
                assert_eq!(nodes, 0);
            }

            let node_id = file_proxy.get_attr().await.unwrap().1.id;
            first_object_id = node_id;
            project_proxy
                .set_for_node(node_id, PROJECT_ID)
                .await
                .unwrap()
                .expect("Setting project on node");

            bytes_usage = {
                let BytesAndNodes { bytes, nodes } =
                    project_proxy.info(PROJECT_ID).await.unwrap().expect("Fetching project info").1;
                assert!(bytes > 0);
                assert_eq!(nodes, 1);
                bytes
            };

            // Grow the file by a block.
            assert_eq!(
                8192,
                file_proxy
                    .write(&vec![0xff as u8; 8192])
                    .await
                    .expect("FIDL call failed")
                    .map_err(Status::from_raw)
                    .expect("File write was successful")
            );
            file_proxy.sync().await.expect("FIDL call failed").expect("Sync failed.");
            bytes_usage = {
                let BytesAndNodes { bytes, nodes } =
                    project_proxy.info(PROJECT_ID).await.unwrap().expect("Fetching project info").1;
                assert!(bytes > bytes_usage);
                assert_eq!(nodes, 1);
                bytes
            };

            std::mem::drop(volume_proxy);
            volumes_directory.terminate().await;
            std::mem::drop(volumes_directory);
            filesystem.close().await.expect("close filesystem failed");
            device = filesystem.take_device().await;
        }
        {
            device.ensure_unique();
            device.reopen(false);
            let filesystem = FxFilesystem::open(device as DeviceHolder).await.unwrap();
            fsck(filesystem.clone()).await.expect("Fsck");
            fsck_volume(filesystem.as_ref(), volume_store_id, Some(Arc::new(InsecureCrypt::new())))
                .await
                .expect("Fsck volume");
            let volumes_directory = VolumesDirectory::new(
                root_volume(filesystem.clone()).await.unwrap(),
                Weak::new(),
                None,
            )
            .await
            .unwrap();
            let volume_and_root = volumes_directory
                .mount_volume(VOLUME_NAME, Some(Arc::new(InsecureCrypt::new())))
                .await
                .expect("mount unencrypted volume failed");
            let (volume_proxy, volume_server_end) =
                fidl::endpoints::create_proxy::<VolumeMarker>().expect("Create proxy to succeed");
            volumes_directory.directory_node().clone().open(
                ExecutionScope::new(),
                fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
                Path::validate_and_split(VOLUME_NAME).unwrap(),
                volume_server_end.into_channel().into(),
            );

            let (root_proxy, project_proxy) = {
                let (volume_dir_proxy, dir_server_end) =
                    fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
                        .expect("Create dir proxy to succeed");
                volumes_directory
                    .serve_volume(&volume_and_root, dir_server_end, false)
                    .await
                    .expect("serve_volume failed");

                let (root_proxy, root_server_end) =
                    fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
                        .expect("Create dir proxy to succeed");
                volume_dir_proxy
                    .open(
                        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
                        fio::ModeType::empty(),
                        "root",
                        ServerEnd::new(root_server_end.into_channel()),
                    )
                    .expect("Failed to open volume root");
                let project_proxy = {
                    connect_to_protocol_at_dir_svc::<ProjectIdMarker>(&volume_dir_proxy)
                        .expect("Unable to connect to project id service")
                };
                (root_proxy, project_proxy)
            };

            {
                let BytesAndNodes { bytes, nodes } =
                    project_proxy.info(PROJECT_ID).await.unwrap().expect("Fetching project info").1;
                assert_eq!(bytes, bytes_usage);
                assert_eq!(nodes, 1);
            }

            assert_eq!(
                project_proxy
                    .get_for_node(first_object_id)
                    .await
                    .unwrap()
                    .expect("Checking project"),
                PROJECT_ID
            );
            root_proxy
                .unlink(FILE_NAME, &fio::UnlinkOptions::default())
                .await
                .expect("FIDL call failed")
                .expect("unlink failed");
            filesystem.graveyard().flush().await;

            {
                let BytesAndNodes { bytes, nodes } =
                    project_proxy.info(PROJECT_ID).await.unwrap().expect("Fetching project info").1;
                assert_eq!(bytes, 0);
                assert_eq!(nodes, 0);
            }

            let file_proxy = open_file_checked(
                &root_proxy,
                fio::OpenFlags::CREATE
                    | fio::OpenFlags::RIGHT_READABLE
                    | fio::OpenFlags::RIGHT_WRITABLE,
                FILE_NAME,
            )
            .await;

            let node_id = file_proxy.get_attr().await.unwrap().1.id;
            project_proxy
                .set_for_node(node_id, PROJECT_ID)
                .await
                .unwrap()
                .expect("Applying project");

            bytes_usage = {
                let BytesAndNodes { bytes, nodes } =
                    project_proxy.info(PROJECT_ID).await.unwrap().expect("Fetching project info").1;
                // Empty file should have less space than the non-empty file from above.
                assert!(bytes < bytes_usage);
                assert_eq!(nodes, 1);
                bytes
            };

            assert_eq!(
                8192,
                file_proxy
                    .write(&vec![0xff as u8; 8192])
                    .await
                    .expect("FIDL call failed")
                    .map_err(Status::from_raw)
                    .expect("File write was successful")
            );
            file_proxy.sync().await.expect("FIDL call failed").expect("Sync failed.");
            bytes_usage = {
                let BytesAndNodes { bytes, nodes } =
                    project_proxy.info(PROJECT_ID).await.unwrap().expect("Fetching project info").1;
                assert!(bytes > bytes_usage);
                assert_eq!(nodes, 1);
                bytes
            };

            // Trim to zero. Bytes should decrease.
            file_proxy.resize(0).await.expect("FIDL call failed").expect("Resize file");
            {
                let BytesAndNodes { bytes, nodes } =
                    project_proxy.info(PROJECT_ID).await.unwrap().expect("Fetching project info").1;
                assert!(bytes < bytes_usage);
                assert_eq!(nodes, 1);
            };

            // Dropping node from project. Usage should go to zero.
            project_proxy
                .clear_for_node(node_id)
                .await
                .expect("FIDL call failed")
                .expect("Clear failed.");
            {
                let BytesAndNodes { bytes, nodes } =
                    project_proxy.info(PROJECT_ID).await.unwrap().expect("Fetching project info").1;
                assert_eq!(bytes, 0);
                assert_eq!(nodes, 0);
            };

            std::mem::drop(volume_proxy);
            volumes_directory.terminate().await;
            std::mem::drop(volumes_directory);
            filesystem.close().await.expect("close filesystem failed");
            device = filesystem.take_device().await;
        }
        device.ensure_unique();
        device.reopen(false);
        let filesystem = FxFilesystem::open(device as DeviceHolder).await.unwrap();
        fsck(filesystem.clone()).await.expect("Fsck");
        fsck_volume(filesystem.as_ref(), volume_store_id, Some(Arc::new(InsecureCrypt::new())))
            .await
            .expect("Fsck volume");
        filesystem.close().await.expect("close filesystem failed");
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_project_node_inheritence() {
        const BYTES_LIMIT: u64 = 123456;
        const NODES_LIMIT: u64 = 4321;
        const VOLUME_NAME: &str = "A";
        const DIR_NAME: &str = "B";
        const SUBDIR_NAME: &str = "C";
        const FILE_NAME: &str = "D";
        const PROJECT_ID: u64 = 42;
        let volume_store_id;
        let mut device = DeviceHolder::new(FakeDevice::new(8192, 512));
        {
            let filesystem = FxFilesystem::new_empty(device).await.unwrap();
            let volumes_directory = VolumesDirectory::new(
                root_volume(filesystem.clone()).await.unwrap(),
                Weak::new(),
                None,
            )
            .await
            .unwrap();

            let volume_and_root = volumes_directory
                .create_volume(VOLUME_NAME, Some(Arc::new(InsecureCrypt::new())))
                .await
                .expect("create unencrypted volume failed");
            volume_store_id = volume_and_root.volume().store().store_object_id();

            let (volume_proxy, volume_server_end) =
                fidl::endpoints::create_proxy::<VolumeMarker>().expect("Create proxy to succeed");
            volumes_directory.directory_node().clone().open(
                ExecutionScope::new(),
                fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
                Path::validate_and_split(VOLUME_NAME).unwrap(),
                volume_server_end.into_channel().into(),
            );

            let (volume_dir_proxy, dir_server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
                    .expect("Create dir proxy to succeed");
            volumes_directory
                .serve_volume(&volume_and_root, dir_server_end, false)
                .await
                .expect("serve_volume failed");

            let project_proxy =
                connect_to_protocol_at_dir_svc::<ProjectIdMarker>(&volume_dir_proxy)
                    .expect("Unable to connect to project id service");

            project_proxy
                .set_limit(PROJECT_ID, BYTES_LIMIT, NODES_LIMIT)
                .await
                .unwrap()
                .expect("To set limits");

            let dir_proxy = {
                let (root_proxy, root_server_end) =
                    fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
                        .expect("Create dir proxy to succeed");
                volume_dir_proxy
                    .open(
                        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
                        fio::ModeType::empty(),
                        "root",
                        ServerEnd::new(root_server_end.into_channel()),
                    )
                    .expect("Failed to open volume root");

                open_dir_checked(
                    &root_proxy,
                    fio::OpenFlags::CREATE
                        | fio::OpenFlags::RIGHT_READABLE
                        | fio::OpenFlags::RIGHT_WRITABLE
                        | fio::OpenFlags::DIRECTORY,
                    DIR_NAME,
                )
                .await
            };
            {
                let node_id = dir_proxy.get_attr().await.unwrap().1.id;
                project_proxy
                    .set_for_node(node_id, PROJECT_ID)
                    .await
                    .unwrap()
                    .expect("Setting project on node");
            }

            let subdir_proxy = open_dir_checked(
                &dir_proxy,
                fio::OpenFlags::CREATE
                    | fio::OpenFlags::RIGHT_READABLE
                    | fio::OpenFlags::RIGHT_WRITABLE
                    | fio::OpenFlags::DIRECTORY,
                SUBDIR_NAME,
            )
            .await;
            {
                let node_id = subdir_proxy.get_attr().await.unwrap().1.id;
                assert_eq!(
                    project_proxy
                        .get_for_node(node_id)
                        .await
                        .unwrap()
                        .expect("Setting project on node"),
                    PROJECT_ID
                );
            }

            let file_proxy = open_file_checked(
                &subdir_proxy,
                fio::OpenFlags::CREATE
                    | fio::OpenFlags::RIGHT_READABLE
                    | fio::OpenFlags::RIGHT_WRITABLE,
                FILE_NAME,
            )
            .await;
            {
                let node_id = file_proxy.get_attr().await.unwrap().1.id;
                assert_eq!(
                    project_proxy
                        .get_for_node(node_id)
                        .await
                        .unwrap()
                        .expect("Setting project on node"),
                    PROJECT_ID
                );
            }

            let BytesAndNodes { nodes, .. } =
                project_proxy.info(PROJECT_ID).await.unwrap().expect("Fetching project info").1;
            assert_eq!(nodes, 3);
            std::mem::drop(volume_proxy);
            volumes_directory.terminate().await;
            std::mem::drop(volumes_directory);
            filesystem.close().await.expect("close filesystem failed");
            device = filesystem.take_device().await;
        }
        device.ensure_unique();
        device.reopen(false);
        let filesystem = FxFilesystem::open(device as DeviceHolder).await.unwrap();
        fsck(filesystem.clone()).await.expect("Fsck");
        fsck_volume(filesystem.as_ref(), volume_store_id, Some(Arc::new(InsecureCrypt::new())))
            .await
            .expect("Fsck volume");
        filesystem.close().await.expect("close filesystem failed");
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_project_listing() {
        const VOLUME_NAME: &str = "A";
        const FILE_NAME: &str = "B";
        const NON_ZERO_PROJECT_ID: u64 = 3;
        let mut device = DeviceHolder::new(FakeDevice::new(8192, 512));
        let volume_store_id;
        {
            let filesystem = FxFilesystem::new_empty(device).await.unwrap();
            let volumes_directory = VolumesDirectory::new(
                root_volume(filesystem.clone()).await.unwrap(),
                Weak::new(),
                None,
            )
            .await
            .unwrap();
            let volume_and_root = volumes_directory
                .create_volume(VOLUME_NAME, None)
                .await
                .expect("create unencrypted volume failed");
            volume_store_id = volume_and_root.volume().store().store_object_id();
            let (volume_proxy, volume_server_end) =
                fidl::endpoints::create_proxy::<VolumeMarker>().expect("Create proxy to succeed");
            volumes_directory.directory_node().clone().open(
                ExecutionScope::new(),
                fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
                Path::validate_and_split(VOLUME_NAME).unwrap(),
                volume_server_end.into_channel().into(),
            );
            let (volume_dir_proxy, dir_server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
                    .expect("Create dir proxy to succeed");
            volumes_directory
                .serve_volume(&volume_and_root, dir_server_end, false)
                .await
                .expect("serve_volume failed");
            let project_proxy =
                connect_to_protocol_at_dir_svc::<ProjectIdMarker>(&volume_dir_proxy)
                    .expect("Unable to connect to project id service");
            // This is just to ensure that the small numbers below can be used for this test.
            assert!(FxVolumeAndRoot::MAX_PROJECT_ENTRIES >= 4);
            // Create a bunch of proxies. 3 more than the limit to ensure pagination.
            let num_entries = u64::try_from(FxVolumeAndRoot::MAX_PROJECT_ENTRIES + 3).unwrap();
            for project_id in 1..=num_entries {
                project_proxy.set_limit(project_id, 1, 1).await.unwrap().expect("To set limits");
            }

            // Add one usage entry to be interspersed with the limit entries. Verifies that the
            // iterator will progress passed it with no effect.
            let file_proxy = {
                let (root_proxy, root_server_end) =
                    fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
                        .expect("Create dir proxy to succeed");
                volume_dir_proxy
                    .open(
                        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
                        fio::ModeType::empty(),
                        "root",
                        ServerEnd::new(root_server_end.into_channel()),
                    )
                    .expect("Failed to open volume root");

                open_file_checked(
                    &root_proxy,
                    fio::OpenFlags::CREATE
                        | fio::OpenFlags::RIGHT_READABLE
                        | fio::OpenFlags::RIGHT_WRITABLE,
                    FILE_NAME,
                )
                .await
            };
            let node_id = file_proxy.get_attr().await.unwrap().1.id;
            project_proxy
                .set_for_node(node_id, NON_ZERO_PROJECT_ID)
                .await
                .unwrap()
                .expect("Setting project on node");
            {
                let BytesAndNodes { nodes, .. } = project_proxy
                    .info(NON_ZERO_PROJECT_ID)
                    .await
                    .unwrap()
                    .expect("Fetching project info")
                    .1;
                assert_eq!(nodes, 1);
            }

            // If this `unwrap()` fails, it is likely the MAX_PROJECT_ENTRIES is too large for fidl.
            let (mut entries, mut next_token) =
                project_proxy.list(None).await.unwrap().expect("To get project listing");
            assert_eq!(entries.len(), FxVolumeAndRoot::MAX_PROJECT_ENTRIES);
            assert!(next_token.is_some());
            assert!(entries.contains(&1));
            assert!(entries.contains(&3));
            assert!(!entries.contains(&num_entries));
            // Page two should have a small set at the end.
            (entries, next_token) = project_proxy
                .list(next_token.as_deref())
                .await
                .unwrap()
                .expect("To get project listing");
            assert_eq!(entries.len(), 3);
            assert!(next_token.is_none());
            assert!(entries.contains(&num_entries));
            assert!(!entries.contains(&1));
            assert!(!entries.contains(&3));
            // Delete a couple and list all again, but one has usage still.
            project_proxy.clear(1).await.unwrap().expect("Clear project");
            project_proxy.clear(3).await.unwrap().expect("Clear project");
            (entries, next_token) =
                project_proxy.list(None).await.unwrap().expect("To get project listing");
            assert_eq!(entries.len(), FxVolumeAndRoot::MAX_PROJECT_ENTRIES);
            assert!(next_token.is_some());
            assert!(!entries.contains(&num_entries));
            assert!(!entries.contains(&1));
            assert!(entries.contains(&3));
            (entries, next_token) = project_proxy
                .list(next_token.as_deref())
                .await
                .unwrap()
                .expect("To get project listing");
            assert_eq!(entries.len(), 2);
            assert!(next_token.is_none());
            assert!(entries.contains(&num_entries));
            // Delete two more to hit the edge case.
            project_proxy.clear(2).await.unwrap().expect("Clear project");
            project_proxy.clear(4).await.unwrap().expect("Clear project");
            (entries, next_token) =
                project_proxy.list(None).await.unwrap().expect("To get project listing");
            assert_eq!(entries.len(), FxVolumeAndRoot::MAX_PROJECT_ENTRIES);
            assert!(next_token.is_none());
            assert!(entries.contains(&num_entries));
            std::mem::drop(volume_proxy);
            volumes_directory.terminate().await;
            std::mem::drop(volumes_directory);
            filesystem.close().await.expect("close filesystem failed");
            device = filesystem.take_device().await;
        }
        device.ensure_unique();
        device.reopen(false);
        let filesystem = FxFilesystem::open(device as DeviceHolder).await.unwrap();
        fsck(filesystem.clone()).await.expect("Fsck");
        fsck_volume(filesystem.as_ref(), volume_store_id, None).await.expect("Fsck volume");
        filesystem.close().await.expect("close filesystem failed");
    }

    #[fuchsia::test(threads = 10)]
    async fn test_unencrypted_volume() {
        let fixture = TestFixture::new_unencrypted().await;
        let root = fixture.root();

        let f =
            open_file_checked(&root, fio::OpenFlags::CREATE | fio::OpenFlags::NOT_DIRECTORY, "foo")
                .await;
        close_file_checked(f).await;

        fixture.close().await;
    }
}
