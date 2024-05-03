// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::fuchsia::{
        component::map_to_raw_status,
        directory::FxDirectory,
        errors::map_to_status,
        fxblob::BlobDirectory,
        memory_pressure::{MemoryPressureLevel, MemoryPressureMonitor},
        volume::{FxVolume, FxVolumeAndRoot, MemoryPressureConfig, RootDir},
        RemoteCrypt,
    },
    anyhow::{anyhow, bail, ensure, Context, Error},
    fidl::endpoints::{DiscoverableProtocolMarker, ServerEnd},
    fidl_fuchsia_fs::{AdminMarker, AdminRequest, AdminRequestStream},
    fidl_fuchsia_fxfs::{
        BlobCreatorMarker, BlobReaderMarker, CheckOptions, MountOptions, ProjectIdMarker,
        VolumeRequest, VolumeRequestStream,
    },
    fidl_fuchsia_io as fio,
    fs_inspect::{FsInspectTree, FsInspectVolume},
    fuchsia_async as fasync,
    fuchsia_zircon::{self as zx, AsHandleRef},
    futures::{stream::FuturesUnordered, StreamExt, TryStreamExt},
    fxfs::{
        errors::FxfsError,
        fsck,
        log::*,
        metrics,
        object_store::{
            transaction::{lock_keys, LockKey, Options},
            volume::RootVolume,
            Directory, ObjectDescriptor, ObjectStore,
        },
    },
    fxfs_crypto::Crypt,
    fxfs_trace::{trace_future_args, TraceFutureExt},
    rustc_hash::FxHashMap as HashMap,
    std::sync::{
        atomic::{AtomicU64, Ordering},
        Arc, OnceLock, Weak,
    },
    vfs::{
        directory::{
            entry_container::{self, MutableDirectory},
            helper::DirectlyMutable,
        },
        path::Path,
    },
};

const MEBIBYTE: u64 = 1024 * 1024;

/// VolumesDirectory is a special pseudo-directory used to enumerate and operate on volumes.
/// Volume creation happens via fuchsia.fxfs.Volumes.Create, rather than open.
///
/// Note that VolumesDirectory assumes exclusive access to |root_volume| and if volumes are
/// manipulated from elsewhere, strange things will happen.
pub struct VolumesDirectory {
    root_volume: RootVolume,
    directory_node: Arc<vfs::directory::immutable::Simple>,
    mounted_volumes: futures::lock::Mutex<HashMap<u64, (String, FxVolumeAndRoot)>>,
    inspect_tree: Weak<FsInspectTree>,
    mem_monitor: Option<MemoryPressureMonitor>,
    // The state of profile recordings. Should be locked *after* mounted_volumes.
    profiling_state: futures::lock::Mutex<Option<(String, fasync::Task<()>)>>,

    /// A running estimate of the number of dirty bytes outstanding in all pager-backed VMOs across
    /// all volumes.
    pager_dirty_bytes_count: AtomicU64,

    // A callback to invoke when a volume is added.  When the volume is removed, this is called
    // again with `None` as the second parameter.
    on_volume_added: OnceLock<Box<dyn Fn(&str, Option<Arc<ObjectStore>>) + Send + Sync>>,
}

/// Operations on VolumesDirectory that cannot be performed concurrently (i.e. most
/// volume creation/removal ops) should exist on this guard instead of VolumesDirectory.
pub struct MountedVolumesGuard<'a> {
    volumes_directory: Arc<VolumesDirectory>,
    mounted_volumes: futures::lock::MutexGuard<'a, HashMap<u64, (String, FxVolumeAndRoot)>>,
}

impl MountedVolumesGuard<'_> {
    /// Creates and mounts a new volume. If |crypt| is set, the volume will be encrypted. The
    /// volume is mounted according to |as_blob|.
    async fn create_and_mount_volume(
        &mut self,
        name: &str,
        crypt: Option<Arc<dyn Crypt>>,
        as_blob: bool,
    ) -> Result<FxVolumeAndRoot, Error> {
        self.volumes_directory
            .root_volume
            .new_volume(name, crypt)
            .await
            .context("failed to create new volume")?;
        // The volume store is unlocked when created, so we don't pass `crypt` when mounting.
        let volume =
            self.mount_volume(name, None, as_blob).await.context("failed to mount volume")?;
        let store_object_id = volume.volume().store().store_object_id();
        self.volumes_directory.add_directory_entry(name, store_object_id);
        Ok(volume)
    }

    /// Mounts an existing volume. `crypt` will be used to unlock the volume if provided.
    /// If `as_blob` is `true`, the volume will be mounted as a blob filesystem, otherwise
    /// it will be treated as a regular fxfs volume.
    async fn mount_volume(
        &mut self,
        name: &str,
        crypt: Option<Arc<dyn Crypt>>,
        as_blob: bool,
    ) -> Result<FxVolumeAndRoot, Error> {
        let store = self.volumes_directory.root_volume.volume(name, crypt).await?;
        ensure!(
            !self.mounted_volumes.contains_key(&store.store_object_id()),
            FxfsError::AlreadyBound
        );
        if as_blob {
            let volume = self
                .mount_store::<BlobDirectory>(name, store, MemoryPressureConfig::default())
                .await?;
            if let Some((profile_name, _)) = &(*self.volumes_directory.profiling_state.lock().await)
            {
                if let Err(e) = volume.volume().record_or_replay_profile(profile_name).await {
                    error!(
                        "Failed to record or replay profile '{}' for volume {}: {:?}",
                        profile_name, name, e
                    );
                }
            }
            Ok(volume)
        } else {
            self.mount_store::<FxDirectory>(name, store, MemoryPressureConfig::default()).await
        }
    }

    // Mounts the given store.  A lock *must* be held on the volume directory.
    async fn mount_store<T: From<Directory<FxVolume>> + RootDir>(
        &mut self,
        name: &str,
        store: Arc<ObjectStore>,
        flush_task_config: MemoryPressureConfig,
    ) -> Result<FxVolumeAndRoot, Error> {
        metrics::object_stores_tracker().register_store(name, Arc::downgrade(&store));
        let store_id = store.store_object_id();
        let unique_id = zx::Event::create();
        let volume = FxVolumeAndRoot::new::<T>(
            Arc::downgrade(&self.volumes_directory),
            store.clone(),
            unique_id.get_koid().unwrap().raw_koid(),
        )
        .await?;
        volume
            .volume()
            .start_background_task(flush_task_config, self.volumes_directory.mem_monitor.as_ref());
        self.mounted_volumes.insert(store_id, (name.to_string(), volume.clone()));
        if let Some(inspect) = self.volumes_directory.inspect_tree.upgrade() {
            inspect.register_volume(
                name.to_string(),
                Arc::downgrade(volume.volume()) as Weak<dyn FsInspectVolume + Send + Sync>,
            )
        }
        if let Some(callback) = self.volumes_directory.on_volume_added.get() {
            callback(name, Some(store));
        }
        Ok(volume)
    }

    async fn remove_volume(&mut self, name: &str) -> Result<(), Error> {
        let store = self.volumes_directory.root_volume.volume_directory().store();
        let transaction = store
            .filesystem()
            .new_transaction(
                lock_keys![LockKey::object(
                    store.store_object_id(),
                    self.volumes_directory.root_volume.volume_directory().object_id(),
                )],
                Options { borrow_metadata_space: true, ..Default::default() },
            )
            .await?;
        let object_id = match self
            .volumes_directory
            .root_volume
            .volume_directory()
            .lookup(name)
            .await?
            .ok_or(FxfsError::NotFound)?
        {
            (object_id, ObjectDescriptor::Volume) => object_id,
            _ => bail!(anyhow!(FxfsError::Inconsistent).context("Expected volume")),
        };
        // Cowardly refuse to delete a mounted volume.
        ensure!(!self.mounted_volumes.contains_key(&object_id), FxfsError::AlreadyBound);
        if let Some(inspect) = self.volumes_directory.inspect_tree.upgrade() {
            inspect.unregister_volume(name.to_string());
        }
        metrics::object_stores_tracker().unregister_store(name);
        let directory_node = self.volumes_directory.directory_node.clone();
        self.volumes_directory
            .root_volume
            .delete_volume(name, transaction, || {
                // This shouldn't fail because the entry should exist.
                directory_node.remove_entry(name, /* must_be_directory: */ false).unwrap();
            })
            .await?;
        Ok(())
    }

    async fn terminate(&mut self) {
        let volumes = std::mem::take(&mut *self.mounted_volumes);
        for (_, (name, volume)) in volumes {
            if let Some(callback) = self.volumes_directory.on_volume_added.get() {
                callback(&name, None);
            }
            volume.volume().terminate().await;
        }
    }

    // Unmounts the volume identified by `store_id`.  The caller should take locks to avoid races if
    // necessary.
    async fn unmount(&mut self, store_id: u64) -> Result<(), Error> {
        let (name, volume) = self.mounted_volumes.remove(&store_id).ok_or(FxfsError::NotFound)?;
        if let Some(callback) = self.volumes_directory.on_volume_added.get() {
            callback(&name, None);
        }
        volume.volume().terminate().await;
        Ok(())
    }
}

impl VolumesDirectory {
    /// Fills the VolumesDirectory with all volumes in |root_volume|.  No volume is opened during
    /// this.
    pub async fn new(
        root_volume: RootVolume,
        inspect_tree: Weak<FsInspectTree>,
        mem_monitor: Option<MemoryPressureMonitor>,
    ) -> Result<Arc<Self>, Error> {
        let layer_set = root_volume.volume_directory().store().tree().layer_set();
        let mut merger = layer_set.merger();
        let me = Arc::new(Self {
            root_volume,
            directory_node: vfs::directory::immutable::simple(),
            mounted_volumes: futures::lock::Mutex::new(HashMap::default()),
            inspect_tree,
            mem_monitor,
            profiling_state: futures::lock::Mutex::new(None),
            pager_dirty_bytes_count: AtomicU64::new(0),
            on_volume_added: OnceLock::new(),
        });
        let mut iter = me.root_volume.volume_directory().iter(&mut merger).await?;
        while let Some((name, store_id, object_descriptor)) = iter.get() {
            ensure!(*object_descriptor == ObjectDescriptor::Volume, FxfsError::Inconsistent);

            me.add_directory_entry(&name, store_id);

            iter.advance().await?;
        }
        Ok(me)
    }

    pub async fn delete_profile(
        self: &Arc<Self>,
        volume: &str,
        profile: &str,
    ) -> Result<(), zx::Status> {
        for (_, (vol_name, volume_and_root)) in &*(self.mounted_volumes.lock().await) {
            if vol_name == volume {
                let dir = Arc::new(FxDirectory::new(
                    None,
                    volume_and_root
                        .volume()
                        .get_profile_directory()
                        .await
                        .map_err(map_to_status)?,
                ));
                return dir.unlink(profile, false).await;
            }
        }
        warn!("Volume '{}' not found while deleting profile '{}'", volume, profile);
        Err(zx::Status::NOT_FOUND)
    }

    async fn finish_profiling(self: &Arc<Self>) {
        let volumes = self.mounted_volumes.lock().await;
        let mut state = self.profiling_state.lock().await;
        for (_, (_, volume_and_root)) in &*volumes {
            volume_and_root.volume().finish_profiling().await;
        }
        *state = None;
    }

    pub async fn record_or_replay_profile(
        self: Arc<Self>,
        name: String,
        duration_secs: u32,
    ) -> Result<(), Error> {
        // Volumes lock is taken first to provide consistent lock ordering with mounting a volume.
        let volumes = self.mounted_volumes.lock().await;
        let mut state = self.profiling_state.lock().await;
        if state.is_none() {
            for (_, (volume_name, volume_and_root)) in &*volumes {
                if volume_and_root.root().clone().into_any().downcast::<BlobDirectory>().is_ok() {
                    // Just log the errors, don't stop half-way.
                    if let Err(e) = volume_and_root.volume().record_or_replay_profile(&name).await {
                        error!(
                            "Failed to record or replay profile '{}' for volume {}: {:?}",
                            &name, volume_name, e
                        );
                    }
                }
            }
            let this = self.clone();
            let timer_task = fasync::Task::spawn(async move {
                fasync::Timer::new(fasync::Duration::from_seconds(duration_secs.into())).await;
                this.finish_profiling().await;
            });
            *state = Some((name, timer_task));
            Ok(())
        } else {
            // Consistency in the recording and replaying cannot be ensured at the volume level
            // if more than one operation can be in flight at a time.
            Err(anyhow!(FxfsError::AlreadyExists).context("Profile operation already in progress."))
        }
    }

    /// Returns the directory node which can be used to provide connections for e.g. enumerating
    /// entries in the VolumesDirectory.
    /// Directly manipulating the entries in this node will result in strange behaviour.
    pub fn directory_node(&self) -> &Arc<vfs::directory::immutable::Simple> {
        &self.directory_node
    }

    // This serves as an exclusive lock for operations that manipulate the set of mounted volumes.
    async fn lock<'a>(self: &'a Arc<Self>) -> MountedVolumesGuard<'a> {
        MountedVolumesGuard {
            volumes_directory: self.clone(),
            mounted_volumes: self.mounted_volumes.lock().await,
        }
    }

    fn add_directory_entry(self: &Arc<Self>, name: &str, store_id: u64) {
        let weak = Arc::downgrade(self);
        let name_owned = Arc::new(name.to_string());
        self.directory_node
            .add_entry(
                name,
                vfs::service::host(move |requests| {
                    let weak = weak.clone();
                    let name = name_owned.clone();
                    async move {
                        if let Some(me) = weak.upgrade() {
                            let _ =
                                me.handle_volume_requests(name.as_ref(), requests, store_id).await;
                        }
                    }
                }),
            )
            .unwrap();
    }

    /// Creates and mounts a new volume. If |crypt| is set, the volume will be encrypted. The
    /// volume is mounted according to |as_blob|.
    pub async fn create_and_mount_volume(
        self: &Arc<Self>,
        name: &str,
        crypt: Option<Arc<dyn Crypt>>,
        as_blob: bool,
    ) -> Result<FxVolumeAndRoot, Error> {
        self.lock().await.create_and_mount_volume(name, crypt, as_blob).await
    }

    /// Mounts an existing volume. `crypt` will be used to unlock the volume if provided.
    /// If `as_blob` is `true`, the volume will be mounted as a blob filesystem, otherwise
    /// it will be treated as a regular fxfs volume.
    pub async fn mount_volume(
        self: &Arc<Self>,
        name: &str,
        crypt: Option<Arc<dyn Crypt>>,
        as_blob: bool,
    ) -> Result<FxVolumeAndRoot, Error> {
        self.lock().await.mount_volume(name, crypt, as_blob).await
    }

    /// Removes a volume. The volume must exist but encrypted volume keys are not required.
    pub async fn remove_volume(self: &Arc<Self>, name: &str) -> Result<(), Error> {
        self.lock().await.remove_volume(name).await
    }

    /// Terminates all opened volumes.
    pub async fn terminate(self: &Arc<Self>) {
        self.finish_profiling().await;
        self.lock().await.terminate().await
    }

    /// Serves the given volume on `outgoing_dir_server_end`.
    pub fn serve_volume(
        self: &Arc<Self>,
        volume: &FxVolumeAndRoot,
        outgoing_dir_server_end: ServerEnd<fio::DirectoryMarker>,
        as_blob: bool,
    ) -> Result<(), Error> {
        let outgoing_dir = vfs::directory::immutable::simple();

        outgoing_dir.add_entry("root", volume.root().clone().as_directory_entry())?;
        let svc_dir = vfs::directory::immutable::simple();
        outgoing_dir.add_entry("svc", svc_dir.clone())?;

        let store_id = volume.volume().store().store_object_id();
        let me = self.clone();
        svc_dir.add_entry(
            AdminMarker::PROTOCOL_NAME,
            vfs::service::host(move |requests| {
                let me = me.clone();
                async move {
                    let _ = me.handle_admin_requests(requests, store_id).await;
                }
            }),
        )?;
        let project_handler = volume.clone();
        svc_dir.add_entry(
            ProjectIdMarker::PROTOCOL_NAME,
            vfs::service::host(move |requests| {
                let project_handler = project_handler.clone();
                async move {
                    let _ = project_handler.handle_project_id_requests(requests).await;
                }
            }),
        )?;
        if as_blob {
            let root = volume.root().clone();
            svc_dir.add_entry(
                BlobCreatorMarker::PROTOCOL_NAME,
                vfs::service::host(move |r| root.clone().handle_blob_creator_requests(r)),
            )?;
            let root = volume.root().clone();
            svc_dir.add_entry(
                BlobReaderMarker::PROTOCOL_NAME,
                vfs::service::host(move |r| root.clone().handle_blob_reader_requests(r)),
            )?;
        }

        // Use the volume's scope here which should be OK for now.  In theory the scope represents a
        // filesystem instance and the pseudo filesystem we are using is arguably a different
        // filesystem to the volume we are exporting.  The reality is that it only matters for
        // GetToken and mutable methods which are not supported by the immutable version of Simple.
        let scope = volume.volume().scope().clone();
        let mut flags = fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE;
        if as_blob {
            flags |= fio::OpenFlags::RIGHT_EXECUTABLE;
        }
        entry_container::Directory::open(
            outgoing_dir,
            scope.clone(),
            flags,
            Path::dot(),
            outgoing_dir_server_end.into_channel().into(),
        );

        // Automatically unmount when all channels are closed.
        let me = self.clone();
        fasync::Task::spawn(async move {
            scope.wait().await;
            info!(store_id, "Last connection to volume closed, shutting down");

            let mut mounted_volumes = me.lock().await;
            let root_store = me.root_volume.volume_directory().store();
            let fs = root_store.filesystem();
            let _guard = fs
                .lock_manager()
                .txn_lock(lock_keys![LockKey::object(
                    root_store.store_object_id(),
                    me.root_volume.volume_directory().object_id(),
                )])
                .await;

            if let Err(e) = mounted_volumes.unmount(store_id).await {
                warn!(?e, store_id, "Failed to unmount volume");
            }
        })
        .detach();

        info!(store_id, "Serving volume");
        Ok(())
    }

    /// Creates and serves the volume with the given name.
    pub async fn create_and_serve_volume(
        self: &Arc<Self>,
        name: &str,
        outgoing_directory: ServerEnd<fio::DirectoryMarker>,
        mount_options: MountOptions,
    ) -> Result<(), Error> {
        let mut guard = self.lock().await;
        let MountOptions { crypt, as_blob } = mount_options;
        let crypt = if let Some(crypt) = crypt {
            Some(Arc::new(RemoteCrypt::new(crypt)) as Arc<dyn Crypt>)
        } else {
            None
        };
        let volume = guard.create_and_mount_volume(&name, crypt, as_blob).await?;
        self.serve_volume(&volume, outgoing_directory, as_blob).context("failed to serve volume")
    }

    async fn handle_volume_requests(
        self: &Arc<Self>,
        name: &str,
        mut requests: VolumeRequestStream,
        store_id: u64,
    ) -> Result<(), Error> {
        while let Some(request) = requests.try_next().await? {
            match request {
                VolumeRequest::Check { responder, options } => {
                    responder.send(self.handle_check(store_id, options).await.map_err(|error| {
                        error!(?error, store_id, "Failed to check volume");
                        map_to_raw_status(error)
                    }))?
                }
                VolumeRequest::Mount { responder, outgoing_directory, options } => responder.send(
                    self.handle_mount(name, store_id, outgoing_directory, options).await.map_err(
                        |error| {
                            error!(?error, name, store_id, "Failed to mount volume");
                            map_to_raw_status(error)
                        },
                    ),
                )?,
                VolumeRequest::SetLimit { responder, bytes } => responder.send(
                    self.handle_set_limit(store_id, bytes).await.map_err(|error| {
                        error!(?error, store_id, "Failed to set volume limit");
                        map_to_raw_status(error)
                    }),
                )?,
                VolumeRequest::GetLimit { responder } => {
                    responder.send(Ok(self.handle_get_limit(store_id).await))?
                }
            }
        }
        Ok(())
    }

    fn is_flush_required_to_dirty(&self, byte_count: u64) -> bool {
        let mem_pressure = self
            .mem_monitor
            .as_ref()
            .map(|mem_monitor| mem_monitor.level())
            .unwrap_or(MemoryPressureLevel::Normal);
        if !matches!(mem_pressure, MemoryPressureLevel::Critical) {
            return false;
        }

        let total_dirty = self.pager_dirty_bytes_count.load(Ordering::Acquire);
        total_dirty + byte_count >= Self::get_max_pager_dirty_when_mem_critical()
    }

    /// Reports that a certain number of bytes will be dirtied in a pager-backed VMO. If the memory
    /// pressure level is critical and fxfs has lots of dirty pages then a new task will be spawned
    /// in `volume` to flush the dirty pages before `mark_dirty` is called. If the memory pressure
    /// level is not critical then `mark_dirty` will be synchronously called.
    pub fn report_pager_dirty(
        self: Arc<Self>,
        byte_count: u64,
        volume: Arc<FxVolume>,
        mark_dirty: impl FnOnce() + Send + 'static,
    ) {
        if !self.is_flush_required_to_dirty(byte_count) {
            self.pager_dirty_bytes_count.fetch_add(byte_count, Ordering::AcqRel);
            mark_dirty();
        } else {
            volume.spawn(
                async move {
                    let volumes = self.mounted_volumes.lock().await;

                    // Re-check the number of outstanding pager dirty bytes because another thread
                    // could have raced and flushed the volumes first.
                    if self.is_flush_required_to_dirty(byte_count) {
                        debug!(
                            "Flushing all volumes. Memory pressure is critical & dirty pager bytes \
                            ({} MiB) >= limit ({} MiB)",
                            self.pager_dirty_bytes_count.load(Ordering::Acquire) / MEBIBYTE,
                            Self::get_max_pager_dirty_when_mem_critical() / MEBIBYTE
                        );

                        let flushes = FuturesUnordered::new();
                        for (_, vol_and_root) in volumes.values() {
                            let vol = vol_and_root.volume().clone();
                            flushes.push(async move {
                                vol.flush_all_files().await;
                            });
                        }

                        flushes.collect::<()>().await;
                    }
                    self.pager_dirty_bytes_count.fetch_add(byte_count, Ordering::AcqRel);
                    mark_dirty();
                }
                .trace(trace_future_args!(c"flush-before-mark-dirty")),
            )
        }
    }

    /// Reports that a certain number of bytes were cleaned in a pager-backed VMO.
    pub fn report_pager_clean(&self, byte_count: u64) {
        let prev_dirty = self.pager_dirty_bytes_count.fetch_sub(byte_count, Ordering::AcqRel);

        if prev_dirty < byte_count {
            // An unlikely scenario, but if there was an underflow, reset the pager dirty bytes to
            // zero.
            self.pager_dirty_bytes_count.store(0, Ordering::Release);
        }
    }

    /// Gets the maximum amount of bytes to allow to be dirty when memory pressure is CRITICAL.
    fn get_max_pager_dirty_when_mem_critical() -> u64 {
        // Only allow up to 1% of available physical memory
        zx::system_get_physmem() / 100
    }

    async fn handle_check(
        self: &Arc<Self>,
        store_id: u64,
        options: CheckOptions,
    ) -> Result<(), Error> {
        let fs = self.root_volume.volume_directory().store().filesystem();
        let crypt = if let Some(crypt) = options.crypt {
            Some(Arc::new(RemoteCrypt::new(crypt)) as Arc<dyn Crypt>)
        } else {
            None
        };
        let result = fsck::fsck_volume(fs.as_ref(), store_id, crypt).await?;
        // TODO(b/311550633): Stash result in inspect.
        tracing::info!(%store_id, "{:?}", result);
        Ok(())
    }

    async fn handle_set_limit(self: &Arc<Self>, store_id: u64, bytes: u64) -> Result<(), Error> {
        let fs = self.root_volume.volume_directory().store().filesystem();
        let mut transaction = fs.clone().new_transaction(lock_keys![], Options::default()).await?;
        fs.allocator().set_bytes_limit(&mut transaction, store_id, bytes).await?;
        transaction.commit().await?;
        Ok(())
    }

    async fn handle_get_limit(self: &Arc<Self>, store_id: u64) -> u64 {
        let fs = self.root_volume.volume_directory().store().filesystem();
        fs.allocator().get_bytes_limit(store_id).await.unwrap_or_default()
    }

    async fn handle_mount(
        self: &Arc<Self>,
        name: &str,
        store_id: u64,
        outgoing_directory: ServerEnd<fio::DirectoryMarker>,
        options: MountOptions,
    ) -> Result<(), Error> {
        tracing::info!(%name, %store_id, ?options, "Received mount request");
        let crypt = if let Some(crypt) = options.crypt {
            Some(Arc::new(RemoteCrypt::new(crypt)) as Arc<dyn Crypt>)
        } else {
            None
        };
        let volume = self
            .mount_volume(name, crypt, options.as_blob)
            .await
            .context("failed to mount volume")?;
        self.serve_volume(&volume, outgoing_directory, options.as_blob)
            .context("failed to serve volume")
    }

    async fn handle_admin_requests(
        self: &Arc<Self>,
        mut stream: AdminRequestStream,
        store_id: u64,
    ) -> Result<(), Error> {
        // If the Admin protocol ever supports more methods, this should change to a while.
        if let Some(request) = stream.try_next().await.context("Reading request")? {
            match request {
                AdminRequest::Shutdown { responder } => {
                    info!(store_id, "Received shutdown request for volume");

                    let root_store = self.root_volume.volume_directory().store();
                    let fs = root_store.filesystem();
                    let guard = fs
                        .lock_manager()
                        .txn_lock(lock_keys![LockKey::object(
                            root_store.store_object_id(),
                            self.root_volume.volume_directory().object_id(),
                        )])
                        .await;
                    let me = self.clone();

                    // unmount will indirectly call scope.shutdown which will drop the task that we
                    // are running on, so we spawn onto a new task that won't get dropped.  An
                    // alternative would be to separate the execution-scopes for the volume and
                    // pseudo-filesystem.
                    fasync::Task::spawn(async move {
                        let _ = stream;
                        let _ = guard;
                        let _ = me.lock().await.unmount(store_id).await;
                        responder
                            .send()
                            .unwrap_or_else(|e| warn!("Failed to send shutdown response: {}", e));
                    })
                    .detach();
                }
            }
        }
        Ok(())
    }

    /// Sets a callback which is invoked when a volume is added.  When the volume is removed, this
    /// is called again with `None` as the second parameter.
    /// Note that this can only be set once per VolumesDirectory; repeated calls will panic.
    pub fn set_on_mount_callback<F: Fn(&str, Option<Arc<ObjectStore>>) + Send + Sync + 'static>(
        &self,
        callback: F,
    ) {
        self.on_volume_added.set(Box::new(callback)).ok().unwrap();
    }
}

#[cfg(test)]
mod tests {
    use {
        crate::fuchsia::{testing::open_file_checked, volumes_directory::VolumesDirectory},
        fidl::endpoints::{create_proxy, create_request_stream, ServerEnd},
        fidl_fuchsia_fs::AdminMarker,
        fidl_fuchsia_fxfs::{KeyPurpose, MountOptions, VolumeMarker, VolumeProxy},
        fidl_fuchsia_io as fio, fuchsia_async as fasync,
        fuchsia_component::client::connect_to_protocol_at_dir_svc,
        fuchsia_fs::file,
        fuchsia_zircon::Status,
        futures::join,
        fxfs::{
            errors::FxfsError, filesystem::FxFilesystem, fsck::fsck,
            object_store::allocator::Allocator, object_store::volume::root_volume,
        },
        fxfs_crypto::Crypt,
        fxfs_insecure_crypto::InsecureCrypt,
        rand::Rng as _,
        std::{
            sync::{atomic::Ordering, Arc, Weak},
            time::Duration,
        },
        storage_device::{fake_device::FakeDevice, DeviceHolder},
        vfs::{directory::entry_container::Directory, execution_scope::ExecutionScope, path::Path},
    };

    #[fuchsia::test]
    async fn test_volume_creation() {
        let device = DeviceHolder::new(FakeDevice::new(8192, 512));
        let filesystem = FxFilesystem::new_empty(device).await.unwrap();
        let volumes_directory = VolumesDirectory::new(
            root_volume(filesystem.clone()).await.unwrap(),
            Weak::new(),
            None,
        )
        .await
        .unwrap();

        let crypt = Arc::new(InsecureCrypt::new()) as Arc<dyn Crypt>;
        {
            let vol = volumes_directory
                .create_and_mount_volume("encrypted", Some(crypt.clone()), false)
                .await
                .expect("create encrypted volume failed");
            vol.volume().store().store_object_id()
        };

        volumes_directory.terminate().await;
        std::mem::drop(volumes_directory);
        filesystem.close().await.expect("close filesystem failed");
        let device = filesystem.take_device().await;
        device.reopen(false);
        let filesystem = FxFilesystem::open(device).await.unwrap();
        let volumes_directory = VolumesDirectory::new(
            root_volume(filesystem.clone()).await.unwrap(),
            Weak::new(),
            None,
        )
        .await
        .unwrap();

        let error = volumes_directory
            .create_and_mount_volume("encrypted", Some(crypt.clone()), false)
            .await
            .err()
            .expect("Creating existing encrypted volume should fail");
        assert!(FxfsError::AlreadyExists.matches(&error));
    }

    #[fuchsia::test]
    async fn test_dirty_pages_accumulate_in_parent() {
        let device = DeviceHolder::new(FakeDevice::new(8192, 512));
        let filesystem = FxFilesystem::new_empty(device).await.unwrap();
        let volumes_directory = VolumesDirectory::new(
            root_volume(filesystem.clone()).await.unwrap(),
            Weak::new(),
            None,
        )
        .await
        .unwrap();

        let crypt = Arc::new(InsecureCrypt::new()) as Arc<dyn Crypt>;
        let vol = volumes_directory
            .create_and_mount_volume("encrypted", Some(crypt.clone()), false)
            .await
            .expect("create encrypted volume failed");
        let old_dirty = volumes_directory.pager_dirty_bytes_count.load(Ordering::SeqCst);

        let new_dirty = {
            let (root, server_end) =
                create_proxy::<fio::DirectoryMarker>().expect("create_proxy failed");
            vol.root().clone().as_directory().open(
                vol.volume().scope().clone(),
                fio::OpenFlags::DIRECTORY
                    | fio::OpenFlags::RIGHT_READABLE
                    | fio::OpenFlags::RIGHT_WRITABLE,
                Path::dot(),
                ServerEnd::new(server_end.into_channel()),
            );

            let f = open_file_checked(
                &root,
                fio::OpenFlags::CREATE
                    | fio::OpenFlags::RIGHT_WRITABLE
                    | fio::OpenFlags::NOT_DIRECTORY,
                "foo",
            )
            .await;
            let buf = vec![0xaa as u8; 8192];
            file::write(&f, buf.as_slice()).await.expect("Write");
            // It's important to check the dirty bytes before closing the file, as closing can
            // trigger a flush.
            volumes_directory.pager_dirty_bytes_count.load(Ordering::SeqCst)
        };
        assert_ne!(old_dirty, new_dirty);

        volumes_directory.terminate().await;
        std::mem::drop(volumes_directory);
        filesystem.close().await.expect("close filesystem failed");
    }

    #[fuchsia::test]
    async fn test_volume_reopen() {
        let device = DeviceHolder::new(FakeDevice::new(8192, 512));
        let filesystem = FxFilesystem::new_empty(device).await.unwrap();
        let volumes_directory = VolumesDirectory::new(
            root_volume(filesystem.clone()).await.unwrap(),
            Weak::new(),
            None,
        )
        .await
        .unwrap();

        let crypt = Arc::new(InsecureCrypt::new()) as Arc<dyn Crypt>;
        let volume_id = {
            let vol = volumes_directory
                .create_and_mount_volume("encrypted", Some(crypt.clone()), false)
                .await
                .expect("create encrypted volume failed");
            vol.volume().store().store_object_id()
        };

        volumes_directory.terminate().await;
        std::mem::drop(volumes_directory);
        filesystem.close().await.expect("close filesystem failed");
        let device = filesystem.take_device().await;
        device.reopen(false);
        let filesystem = FxFilesystem::open(device).await.unwrap();
        let volumes_directory = VolumesDirectory::new(
            root_volume(filesystem.clone()).await.unwrap(),
            Weak::new(),
            None,
        )
        .await
        .unwrap();

        {
            let vol = volumes_directory
                .mount_volume("encrypted", Some(crypt.clone()), false)
                .await
                .expect("open existing encrypted volume failed");
            assert_eq!(vol.volume().store().store_object_id(), volume_id);
        }

        volumes_directory.terminate().await;
        std::mem::drop(volumes_directory);
        filesystem.close().await.expect("close filesystem failed");
    }

    #[fuchsia::test]
    async fn test_volume_creation_unencrypted() {
        let device = DeviceHolder::new(FakeDevice::new(8192, 512));
        let filesystem = FxFilesystem::new_empty(device).await.unwrap();
        let volumes_directory = VolumesDirectory::new(
            root_volume(filesystem.clone()).await.unwrap(),
            Weak::new(),
            None,
        )
        .await
        .unwrap();

        {
            let vol = volumes_directory
                .create_and_mount_volume("unencrypted", None, false)
                .await
                .expect("create unencrypted volume failed");
            vol.volume().store().store_object_id()
        };

        volumes_directory.terminate().await;
        std::mem::drop(volumes_directory);
        filesystem.close().await.expect("close filesystem failed");
        let device = filesystem.take_device().await;
        device.reopen(false);
        let filesystem = FxFilesystem::open(device).await.unwrap();
        let volumes_directory = VolumesDirectory::new(
            root_volume(filesystem.clone()).await.unwrap(),
            Weak::new(),
            None,
        )
        .await
        .unwrap();

        let error = volumes_directory
            .create_and_mount_volume("unencrypted", None, false)
            .await
            .err()
            .expect("Creating existing unencrypted volume should fail");
        assert!(FxfsError::AlreadyExists.matches(&error));

        volumes_directory.terminate().await;
        std::mem::drop(volumes_directory);
        filesystem.close().await.expect("close filesystem failed");
    }

    #[fuchsia::test]
    async fn test_volume_reopen_unencrypted() {
        let device = DeviceHolder::new(FakeDevice::new(8192, 512));
        let filesystem = FxFilesystem::new_empty(device).await.unwrap();
        let volumes_directory = VolumesDirectory::new(
            root_volume(filesystem.clone()).await.unwrap(),
            Weak::new(),
            None,
        )
        .await
        .unwrap();

        let volume_id = {
            let vol = volumes_directory
                .create_and_mount_volume("unencrypted", None, false)
                .await
                .expect("create unencrypted volume failed");
            vol.volume().store().store_object_id()
        };

        volumes_directory.terminate().await;
        std::mem::drop(volumes_directory);
        filesystem.close().await.expect("close filesystem failed");
        let device = filesystem.take_device().await;
        device.reopen(false);
        let filesystem = FxFilesystem::open(device).await.unwrap();
        let volumes_directory = VolumesDirectory::new(
            root_volume(filesystem.clone()).await.unwrap(),
            Weak::new(),
            None,
        )
        .await
        .unwrap();

        {
            let vol = volumes_directory
                .mount_volume("unencrypted", None, false)
                .await
                .expect("open existing unencrypted volume failed");
            assert_eq!(vol.volume().store().store_object_id(), volume_id);
        }

        volumes_directory.terminate().await;
        std::mem::drop(volumes_directory);
        filesystem.close().await.expect("close filesystem failed");
    }

    #[fuchsia::test]
    async fn test_volume_enumeration() {
        let device = DeviceHolder::new(FakeDevice::new(8192, 512));
        let filesystem = FxFilesystem::new_empty(device).await.unwrap();
        let volumes_directory = VolumesDirectory::new(
            root_volume(filesystem.clone()).await.unwrap(),
            Weak::new(),
            None,
        )
        .await
        .unwrap();

        // Add an encrypted volume...
        let crypt = Arc::new(InsecureCrypt::new()) as Arc<dyn Crypt>;
        {
            volumes_directory
                .create_and_mount_volume("encrypted", Some(crypt.clone()), false)
                .await
                .expect("create encrypted volume failed");
        };
        // And an unencrypted volume.
        {
            volumes_directory
                .create_and_mount_volume("unencrypted", None, false)
                .await
                .expect("create unencrypted volume failed");
        };

        // Restart, so that we can test enumeration of unopened volumes.
        volumes_directory.terminate().await;
        std::mem::drop(volumes_directory);
        filesystem.close().await.expect("close filesystem failed");
        let device = filesystem.take_device().await;
        device.reopen(false);
        let filesystem = FxFilesystem::open(device).await.unwrap();
        let volumes_directory = VolumesDirectory::new(
            root_volume(filesystem.clone()).await.unwrap(),
            Weak::new(),
            None,
        )
        .await
        .unwrap();

        let readdir = |dir: Arc<fio::DirectoryProxy>| async move {
            let status = dir.rewind().await.expect("FIDL call failed");
            Status::ok(status).expect("rewind failed");
            let (status, buf) = dir.read_dirents(fio::MAX_BUF).await.expect("FIDL call failed");
            Status::ok(status).expect("read_dirents failed");
            let mut entries = vec![];
            for res in fuchsia_fs::directory::parse_dir_entries(&buf) {
                entries.push(res.expect("Failed to parse entry").name);
            }
            entries
        };

        let (dir_proxy, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
            .expect("Create proxy to succeed");
        let dir_proxy = Arc::new(dir_proxy);

        volumes_directory.directory_node().clone().open(
            ExecutionScope::new(),
            fio::OpenFlags::DIRECTORY | fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY,
            Path::dot(),
            ServerEnd::new(dir_server_end.into_channel()),
        );

        let entries = readdir(dir_proxy.clone()).await;
        assert_eq!(entries, [".", "encrypted", "unencrypted"]);

        let _vol = volumes_directory
            .mount_volume("encrypted", Some(crypt.clone()), false)
            .await
            .expect("Open encrypted volume failed");

        // Ensure that the behaviour is the same after we've opened a volume.
        let entries = readdir(dir_proxy).await;
        assert_eq!(entries, [".", "encrypted", "unencrypted"]);

        volumes_directory.terminate().await;
        std::mem::drop(volumes_directory);
        filesystem.close().await.expect("close filesystem failed");
    }

    #[fuchsia::test]
    async fn test_deleted_encrypted_volume_while_mounted() {
        const VOLUME_NAME: &str = "encrypted";

        let device = DeviceHolder::new(FakeDevice::new(8192, 512));
        let filesystem = FxFilesystem::new_empty(device).await.unwrap();
        let crypt = Arc::new(InsecureCrypt::new()) as Arc<dyn Crypt>;
        let volumes_directory = VolumesDirectory::new(
            root_volume(filesystem.clone()).await.unwrap(),
            Weak::new(),
            None,
        )
        .await
        .unwrap();
        volumes_directory
            .create_and_mount_volume(VOLUME_NAME, Some(crypt.clone()), false)
            .await
            .expect("create encrypted volume failed");
        // We have the volume mounted so delete attempts should fail.
        assert!(FxfsError::AlreadyBound.matches(
            &volumes_directory
                .remove_volume(VOLUME_NAME)
                .await
                .err()
                .expect("Deleting volume should fail")
        ));
        volumes_directory.terminate().await;
        std::mem::drop(volumes_directory);
        filesystem.close().await.expect("close filesystem failed");
    }

    #[fuchsia::test]
    async fn test_mount_volume_using_volume_protocol() {
        let device = DeviceHolder::new(FakeDevice::new(8192, 512));
        let filesystem = FxFilesystem::new_empty(device).await.unwrap();
        let volumes_directory = VolumesDirectory::new(
            root_volume(filesystem.clone()).await.unwrap(),
            Weak::new(),
            None,
        )
        .await
        .unwrap();

        let crypt = Arc::new(InsecureCrypt::new()) as Arc<dyn Crypt>;
        let store_id = {
            let vol = volumes_directory
                .create_and_mount_volume("encrypted", Some(crypt.clone()), false)
                .await
                .expect("create encrypted volume failed");
            vol.volume().store().store_object_id()
        };
        volumes_directory.lock().await.unmount(store_id).await.expect("unmount failed");

        let (volume_proxy, volume_server_end) =
            fidl::endpoints::create_proxy::<VolumeMarker>().expect("Create proxy to succeed");
        volumes_directory.directory_node().clone().open(
            ExecutionScope::new(),
            fio::OpenFlags::empty(),
            Path::validate_and_split("encrypted").unwrap(),
            volume_server_end.into_channel().into(),
        );

        let (dir_proxy, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
            .expect("Create proxy to succeed");

        let crypt_service = fxfs_crypt::CryptService::new();
        crypt_service
            .add_wrapping_key(0, fxfs_insecure_crypto::DATA_KEY.to_vec())
            .expect("add_wrapping_key failed");
        crypt_service
            .add_wrapping_key(1, fxfs_insecure_crypto::METADATA_KEY.to_vec())
            .expect("add_wrapping_key failed");
        crypt_service.set_active_key(KeyPurpose::Data, 0).expect("set_active_key failed");
        crypt_service.set_active_key(KeyPurpose::Metadata, 1).expect("set_active_key failed");
        let (client1, stream1) = create_request_stream().expect("create_endpoints failed");
        let (client2, stream2) = create_request_stream().expect("create_endpoints failed");

        join!(
            async {
                volume_proxy
                    .mount(dir_server_end, MountOptions { crypt: Some(client1), as_blob: false })
                    .await
                    .expect("mount (fidl) failed")
                    .expect("mount failed");

                open_file_checked(
                    &dir_proxy,
                    fio::OpenFlags::RIGHT_READABLE
                        | fio::OpenFlags::RIGHT_WRITABLE
                        | fio::OpenFlags::CREATE,
                    "root/test",
                )
                .await;

                // Attempting to mount again should fail with ALREADY_BOUND.
                let (_dir_proxy, dir_server_end) =
                    fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
                        .expect("Create proxy to succeed");

                assert_eq!(
                    Status::from_raw(
                        volume_proxy
                            .mount(
                                dir_server_end,
                                MountOptions { crypt: Some(client2), as_blob: false },
                            )
                            .await
                            .expect("mount (fidl) failed")
                            .expect_err("mount succeeded")
                    ),
                    Status::ALREADY_BOUND
                );

                std::mem::drop(dir_proxy);

                // The volume should get unmounted a short time later.
                let mut count = 0;
                loop {
                    if volumes_directory.mounted_volumes.lock().await.is_empty() {
                        break;
                    }
                    count += 1;
                    assert!(count <= 100);
                    fasync::Timer::new(Duration::from_millis(100)).await;
                }
            },
            async {
                crypt_service
                    .handle_request(fxfs_crypt::Services::Crypt(stream1))
                    .await
                    .expect("handle_request failed");
                crypt_service
                    .handle_request(fxfs_crypt::Services::Crypt(stream2))
                    .await
                    .expect("handle_request failed");
            }
        );
        // Make sure the background thread that actually calls terminate() on the volume finishes
        // before exiting the test. terminate() should be a no-op since we already verified
        // mounted_directories is empty, but the volume's terminate() future in the background task
        // may still be outstanding. As both the background task and VolumesDirectory::terminate()
        // hold the write lock, we use that to block until the background task has completed.
        volumes_directory.terminate().await;
    }

    #[fuchsia::test]
    #[ignore] // TODO(b/293917849) re-enable this test when de-flaked

    async fn test_volume_dir_races() {
        let device = DeviceHolder::new(FakeDevice::new(8192, 512));
        let filesystem = FxFilesystem::new_empty(device).await.unwrap();
        let volumes_directory = VolumesDirectory::new(
            root_volume(filesystem.clone()).await.unwrap(),
            Weak::new(),
            None,
        )
        .await
        .unwrap();

        let crypt = Arc::new(InsecureCrypt::new()) as Arc<dyn Crypt>;
        let store_id = {
            let vol = volumes_directory
                .create_and_mount_volume("encrypted", Some(crypt.clone()), false)
                .await
                .expect("create encrypted volume failed");
            vol.volume().store().store_object_id()
        };
        volumes_directory.lock().await.unmount(store_id).await.expect("unmount failed");

        let (volume_proxy, volume_server_end) =
            fidl::endpoints::create_proxy::<VolumeMarker>().expect("Create proxy to succeed");
        volumes_directory.directory_node().clone().open(
            ExecutionScope::new(),
            fio::OpenFlags::empty(),
            Path::validate_and_split("encrypted").unwrap(),
            volume_server_end.into_channel().into(),
        );

        let crypt_service = Arc::new(fxfs_crypt::CryptService::new());
        crypt_service
            .add_wrapping_key(0, fxfs_insecure_crypto::DATA_KEY.to_vec())
            .expect("add_wrapping_key failed");
        crypt_service
            .add_wrapping_key(1, fxfs_insecure_crypto::METADATA_KEY.to_vec())
            .expect("add_wrapping_key failed");
        crypt_service.set_active_key(KeyPurpose::Data, 0).expect("set_active_key failed");
        crypt_service.set_active_key(KeyPurpose::Metadata, 1).expect("set_active_key failed");
        let (client1, stream1) = create_request_stream().expect("create_endpoints failed");
        let (client2, stream2) = create_request_stream().expect("create_endpoints failed");
        let crypt_service_clone = crypt_service.clone();
        let crypt_task1 = fasync::Task::spawn(async move {
            crypt_service_clone
                .handle_request(fxfs_crypt::Services::Crypt(stream1))
                .await
                .expect("handle_request failed");
        });
        let crypt_task2 = fasync::Task::spawn(async move {
            crypt_service
                .handle_request(fxfs_crypt::Services::Crypt(stream2))
                .await
                .expect("handle_request failed");
        });

        // Create two tasks each of mount and remove, and one to recreate the volume, so that we get
        // to exercise a wide variety of concurrent actions.
        // Delay remove and create a bit, since mount is slower due to FIDL.
        join!(
            async {
                let (_dir_proxy, dir_server_end) =
                    fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
                        .expect("Create proxy to succeed");
                if let Err(status) = volume_proxy
                    .mount(dir_server_end, MountOptions { crypt: Some(client1), as_blob: false })
                    .await
                    .expect("mount (fidl) failed")
                {
                    let status = Status::from_raw(status);
                    if status != Status::NOT_FOUND && status != Status::ALREADY_BOUND {
                        assert!(false, "Unexpected status {:}", status);
                    }
                }
            },
            async {
                let (_dir_proxy, dir_server_end) =
                    fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
                        .expect("Create proxy to succeed");
                if let Err(status) = volume_proxy
                    .mount(dir_server_end, MountOptions { crypt: Some(client2), as_blob: false })
                    .await
                    .expect("mount (fidl) failed")
                {
                    let status = Status::from_raw(status);
                    if status != Status::NOT_FOUND && status != Status::ALREADY_BOUND {
                        assert!(false, "Unexpected status {:}", status);
                    }
                }
            },
            async {
                let volumes_directory = volumes_directory.clone();
                let wait_time = rand::thread_rng().gen_range(0..5);
                fasync::Timer::new(Duration::from_millis(wait_time)).await;
                if let Err(err) = volumes_directory.remove_volume("encrypted").await {
                    assert!(
                        FxfsError::NotFound.matches(&err) || FxfsError::AlreadyBound.matches(&err),
                        "Unexpected error {:?}",
                        err
                    );
                }
            },
            async {
                let volumes_directory = volumes_directory.clone();
                let wait_time = rand::thread_rng().gen_range(0..5);
                fasync::Timer::new(Duration::from_millis(wait_time)).await;
                if let Err(err) = volumes_directory.remove_volume("encrypted").await {
                    assert!(
                        FxfsError::NotFound.matches(&err) || FxfsError::AlreadyBound.matches(&err),
                        "Unexpected error {:?}",
                        err
                    );
                }
            },
            async {
                let volumes_directory = volumes_directory.clone();
                let wait_time = rand::thread_rng().gen_range(0..5);
                fasync::Timer::new(Duration::from_millis(wait_time)).await;
                let mut guard = volumes_directory.lock().await;
                match guard.create_and_mount_volume("encrypted", Some(crypt.clone()), false).await {
                    Ok(vol) => {
                        let store_id = vol.volume().store().store_object_id();
                        std::mem::drop(vol);
                        guard.unmount(store_id).await.expect("unmount failed");
                    }
                    Err(err) => {
                        assert!(
                            FxfsError::AlreadyExists.matches(&err)
                                || FxfsError::AlreadyBound.matches(&err),
                            "Unexpected error {:?}",
                            err
                        );
                    }
                }
            }
        );
        std::mem::drop(crypt_task1);
        std::mem::drop(crypt_task2);
        // Make sure the background thread that actually calls terminate() on the volume finishes
        // before exiting the test. terminate() should be a no-op since we already verified
        // mounted_directories is empty, but the volume's terminate() future in the background task
        // may still be outstanding. As both the background task and VolumesDirectory::terminate()
        // hold the write lock, we use that to block until the background task has completed.
        volumes_directory.terminate().await;
    }

    #[fuchsia::test]
    async fn test_shutdown_volume() {
        let device = DeviceHolder::new(FakeDevice::new(8192, 512));
        let filesystem = FxFilesystem::new_empty(device).await.unwrap();
        let volumes_directory = VolumesDirectory::new(
            root_volume(filesystem.clone()).await.unwrap(),
            Weak::new(),
            None,
        )
        .await
        .unwrap();

        let crypt = Arc::new(InsecureCrypt::new()) as Arc<dyn Crypt>;
        let vol = volumes_directory
            .create_and_mount_volume("encrypted", Some(crypt.clone()), false)
            .await
            .expect("create encrypted volume failed");

        let (dir_proxy, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
            .expect("Create proxy to succeed");

        volumes_directory.serve_volume(&vol, dir_server_end, false).expect("serve_volume failed");

        let admin_proxy = connect_to_protocol_at_dir_svc::<AdminMarker>(&dir_proxy)
            .expect("Unable to connect to admin service");

        admin_proxy.shutdown().await.expect("shutdown failed");

        assert!(volumes_directory.mounted_volumes.lock().await.is_empty());
    }

    #[fuchsia::test]
    async fn test_byte_limit_persistence() {
        const BYTES_LIMIT_1: u64 = 123456;
        const BYTES_LIMIT_2: u64 = 456789;
        const VOLUME_NAME: &str = "A";
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

            volumes_directory
                .create_and_mount_volume(VOLUME_NAME, None, false)
                .await
                .expect("create unencrypted volume failed");

            let (volume_proxy, volume_server_end) =
                fidl::endpoints::create_proxy::<VolumeMarker>().expect("Create proxy to succeed");
            volumes_directory.directory_node().clone().open(
                ExecutionScope::new(),
                fio::OpenFlags::empty(),
                Path::validate_and_split(VOLUME_NAME).unwrap(),
                volume_server_end.into_channel().into(),
            );

            volume_proxy.set_limit(BYTES_LIMIT_1).await.unwrap().expect("To set limits");
            {
                let limits = (filesystem.allocator() as Arc<Allocator>).owner_byte_limits();
                assert_eq!(limits.len(), 1);
                assert_eq!(limits[0].1, BYTES_LIMIT_1);
            }

            volume_proxy.set_limit(BYTES_LIMIT_2).await.unwrap().expect("To set limits");
            {
                let limits = (filesystem.allocator() as Arc<Allocator>).owner_byte_limits();
                assert_eq!(limits.len(), 1);
                assert_eq!(limits[0].1, BYTES_LIMIT_2);
            }
            std::mem::drop(volume_proxy);
            volumes_directory.terminate().await;
            std::mem::drop(volumes_directory);
            filesystem.close().await.expect("close filesystem failed");
            device = filesystem.take_device().await;
        }
        device.ensure_unique();
        device.reopen(false);
        {
            let filesystem = FxFilesystem::open(device as DeviceHolder).await.unwrap();
            fsck(filesystem.clone()).await.expect("Fsck");
            let volumes_directory = VolumesDirectory::new(
                root_volume(filesystem.clone()).await.unwrap(),
                Weak::new(),
                None,
            )
            .await
            .unwrap();
            {
                let limits = (filesystem.allocator() as Arc<Allocator>).owner_byte_limits();
                assert_eq!(limits.len(), 1);
                assert_eq!(limits[0].1, BYTES_LIMIT_2);
            }
            volumes_directory.remove_volume(VOLUME_NAME).await.expect("Volume deletion failed");
            {
                let limits = (filesystem.allocator() as Arc<Allocator>).owner_byte_limits();
                assert_eq!(limits.len(), 0);
            }
            volumes_directory.terminate().await;
            std::mem::drop(volumes_directory);
            filesystem.close().await.expect("close filesystem failed");
            device = filesystem.take_device().await;
        }
        device.ensure_unique();
        device.reopen(false);
        let filesystem = FxFilesystem::open(device as DeviceHolder).await.unwrap();
        fsck(filesystem.clone()).await.expect("Fsck");
        let limits = (filesystem.allocator() as Arc<Allocator>).owner_byte_limits();
        assert_eq!(limits.len(), 0);
    }

    struct VolumeInfo {
        volume_proxy: VolumeProxy,
        file_proxy: fio::FileProxy,
    }

    impl VolumeInfo {
        async fn new(volumes_directory: &Arc<VolumesDirectory>, name: &'static str) -> Self {
            let volume = volumes_directory
                .create_and_mount_volume(name, None, false)
                .await
                .expect("create unencrypted volume failed");

            let (volume_dir_proxy, dir_server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
                    .expect("Create dir proxy to succeed");
            volumes_directory
                .serve_volume(&volume, dir_server_end, false)
                .expect("serve_volume failed");

            let (volume_proxy, volume_server_end) =
                fidl::endpoints::create_proxy::<VolumeMarker>().expect("Create proxy to succeed");
            volumes_directory.directory_node().clone().open(
                ExecutionScope::new(),
                fio::OpenFlags::empty(),
                Path::validate_and_split(name).unwrap(),
                volume_server_end.into_channel().into(),
            );

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

            let file_proxy = open_file_checked(
                &root_proxy,
                fio::OpenFlags::CREATE
                    | fio::OpenFlags::RIGHT_READABLE
                    | fio::OpenFlags::RIGHT_WRITABLE
                    | fio::OpenFlags::NOT_DIRECTORY,
                "foo",
            )
            .await;
            VolumeInfo { volume_proxy, file_proxy }
        }
    }

    #[fuchsia::test]
    async fn test_limit_bytes() {
        const BYTES_LIMIT: u64 = 262_144; // 256KiB
        const BLOCK_SIZE: usize = 8192; // 8KiB
        let device = DeviceHolder::new(FakeDevice::new(BLOCK_SIZE.try_into().unwrap(), 512));
        let filesystem = FxFilesystem::new_empty(device).await.unwrap();
        let volumes_directory = VolumesDirectory::new(
            root_volume(filesystem.clone()).await.unwrap(),
            Weak::new(),
            None,
        )
        .await
        .unwrap();

        let vol = VolumeInfo::new(&volumes_directory, "foo").await;
        vol.volume_proxy.set_limit(BYTES_LIMIT).await.unwrap().expect("To set limits");

        let zeros = vec![0u8; BLOCK_SIZE];
        // First write should succeed.
        assert_eq!(
            <u64 as TryInto<usize>>::try_into(
                vol.file_proxy
                    .write(&zeros)
                    .await
                    .expect("Failed Write message")
                    .expect("Failed write")
            )
            .unwrap(),
            BLOCK_SIZE
        );
        // Likely to run out of space before writing the full limit due to overheads.
        for _ in (BLOCK_SIZE..BYTES_LIMIT as usize).step_by(BLOCK_SIZE) {
            match vol.file_proxy.write(&zeros).await.expect("Failed Write message") {
                Err(_) => break,
                Ok(b) if b < BLOCK_SIZE.try_into().unwrap() => break,
                _ => (),
            };
        }

        // Any further writes should fail with out of space.
        assert_eq!(
            vol.file_proxy
                .write(&zeros)
                .await
                .expect("Failed write message")
                .expect_err("Write should have been limited"),
            Status::NO_SPACE.into_raw()
        );

        // Double the limit and try again. We should have write space again.
        vol.volume_proxy.set_limit(BYTES_LIMIT * 2).await.unwrap().expect("To set limits");
        assert_eq!(
            <u64 as TryInto<usize>>::try_into(
                vol.file_proxy
                    .write(&zeros)
                    .await
                    .expect("Failed Write message")
                    .expect("Failed write")
            )
            .unwrap(),
            BLOCK_SIZE
        );

        vol.file_proxy.close().await.unwrap().expect("Failed to close file");
        volumes_directory.terminate().await;
        std::mem::drop(volumes_directory);
        filesystem.close().await.expect("close filesystem failed");
    }

    #[fuchsia::test]
    async fn test_limit_bytes_two_hit_device_limit() {
        const BYTES_LIMIT: u64 = 3_145_728; // 3MiB
        const BLOCK_SIZE: usize = 8192; // 8KiB
        const BLOCK_COUNT: u32 = 512;
        let device =
            DeviceHolder::new(FakeDevice::new(BLOCK_SIZE.try_into().unwrap(), BLOCK_COUNT));
        let filesystem = FxFilesystem::new_empty(device).await.unwrap();
        let volumes_directory = VolumesDirectory::new(
            root_volume(filesystem.clone()).await.unwrap(),
            Weak::new(),
            None,
        )
        .await
        .unwrap();

        let a = VolumeInfo::new(&volumes_directory, "foo").await;
        let b = VolumeInfo::new(&volumes_directory, "bar").await;
        a.volume_proxy.set_limit(BYTES_LIMIT).await.unwrap().expect("To set limits");
        b.volume_proxy.set_limit(BYTES_LIMIT).await.unwrap().expect("To set limits");
        let mut a_written: u64 = 0;
        let mut b_written: u64 = 0;

        // Write chunks of BLOCK_SIZE.
        let zeros = vec![0u8; BLOCK_SIZE];

        // First write should succeed for both.
        assert_eq!(
            <u64 as TryInto<usize>>::try_into(
                a.file_proxy
                    .write(&zeros)
                    .await
                    .expect("Failed Write message")
                    .expect("Failed write")
            )
            .unwrap(),
            BLOCK_SIZE
        );
        a_written += BLOCK_SIZE as u64;
        assert_eq!(
            <u64 as TryInto<usize>>::try_into(
                b.file_proxy
                    .write(&zeros)
                    .await
                    .expect("Failed Write message")
                    .expect("Failed write")
            )
            .unwrap(),
            BLOCK_SIZE
        );
        b_written += BLOCK_SIZE as u64;

        // Likely to run out of space before writing the full limit due to overheads.
        for _ in (BLOCK_SIZE..BYTES_LIMIT as usize).step_by(BLOCK_SIZE) {
            match a.file_proxy.write(&zeros).await.expect("Failed Write message") {
                Err(_) => break,
                Ok(bytes) => {
                    a_written += bytes;
                    if bytes < BLOCK_SIZE.try_into().unwrap() {
                        break;
                    }
                }
            };
        }
        // Any further writes should fail with out of space.
        assert_eq!(
            a.file_proxy
                .write(&zeros)
                .await
                .expect("Failed write message")
                .expect_err("Write should have been limited"),
            Status::NO_SPACE.into_raw()
        );

        // Now write to the second volume. Likely to run out of space before writing the full limit
        // due to overheads.
        for _ in (BLOCK_SIZE..BYTES_LIMIT as usize).step_by(BLOCK_SIZE) {
            match b.file_proxy.write(&zeros).await.expect("Failed Write message") {
                Err(_) => break,
                Ok(bytes) => {
                    b_written += bytes;
                    if bytes < BLOCK_SIZE.try_into().unwrap() {
                        break;
                    }
                }
            };
        }
        // Any further writes should fail with out of space.
        assert_eq!(
            b.file_proxy
                .write(&zeros)
                .await
                .expect("Failed write message")
                .expect_err("Write should have been limited"),
            Status::NO_SPACE.into_raw()
        );

        // Second volume should have failed very early.
        assert!(BLOCK_SIZE as u64 * BLOCK_COUNT as u64 - BYTES_LIMIT >= b_written);
        // First volume should have gotten further.
        assert!(BLOCK_SIZE as u64 * BLOCK_COUNT as u64 - BYTES_LIMIT <= a_written);

        a.file_proxy.close().await.unwrap().expect("Failed to close file");
        b.file_proxy.close().await.unwrap().expect("Failed to close file");
        volumes_directory.terminate().await;
        std::mem::drop(volumes_directory);
        filesystem.close().await.expect("close filesystem failed");
    }

    #[fuchsia::test(threads = 10)]
    async fn test_profile_start() {
        const PREMOUNT_BLOB: &str = "premount_blob";
        const PREMOUNT_NOBLOB: &str = "premount_noblob";
        const LIVE_BLOB: &str = "live_blob";
        const LIVE_NOBLOB: &str = "live_noblob";

        const RECORDING_NAME: &str = "foo";

        let device = {
            let device = DeviceHolder::new(FakeDevice::new(8192, 512));
            let filesystem = FxFilesystem::new_empty(device).await.unwrap();
            let volumes_directory = VolumesDirectory::new(
                root_volume(filesystem.clone()).await.unwrap(),
                Weak::new(),
                None,
            )
            .await
            .unwrap();
            volumes_directory.create_and_mount_volume(PREMOUNT_BLOB, None, true).await.unwrap();
            volumes_directory.create_and_mount_volume(PREMOUNT_NOBLOB, None, false).await.unwrap();
            volumes_directory.create_and_mount_volume(LIVE_BLOB, None, true).await.unwrap();
            volumes_directory.create_and_mount_volume(LIVE_NOBLOB, None, false).await.unwrap();

            volumes_directory.terminate().await;
            std::mem::drop(volumes_directory);
            filesystem.close().await.expect("Filesystem close");
            filesystem.take_device().await
        };

        device.ensure_unique();
        device.reopen(false);
        let device = {
            let filesystem = FxFilesystem::open(device as DeviceHolder).await.unwrap();
            let volumes_directory = VolumesDirectory::new(
                root_volume(filesystem.clone()).await.unwrap(),
                Weak::new(),
                None,
            )
            .await
            .unwrap();

            // Premount two volumes.
            let _premount_blob = volumes_directory
                .mount_volume(PREMOUNT_BLOB, None, true)
                .await
                .expect("Reopen volume");
            let _premount_noblob = volumes_directory
                .mount_volume(PREMOUNT_NOBLOB, None, false)
                .await
                .expect("Reopen volume");

            // Start the recording, let it run a really long time, it doesn't need to end for this
            // test. If it does wait this long then it should trigger test timeouts.
            volumes_directory
                .clone()
                .record_or_replay_profile(RECORDING_NAME.to_owned(), 600)
                .await
                .expect("Recording");

            // Live mount two volumes.
            let _live_blob =
                volumes_directory.mount_volume(LIVE_BLOB, None, true).await.expect("Reopen volume");
            let _live_noblob = volumes_directory
                .mount_volume(LIVE_NOBLOB, None, false)
                .await
                .expect("Reopen volume");

            // Don't explicitly stop the recording, ensure that terminate is able to progress.
            volumes_directory.terminate().await;
            std::mem::drop(volumes_directory);
            filesystem.close().await.expect("Filesystem close");
            filesystem.take_device().await
        };

        device.ensure_unique();
        device.reopen(false);
        let filesystem = FxFilesystem::open(device as DeviceHolder).await.unwrap();
        {
            let volumes_directory = VolumesDirectory::new(
                root_volume(filesystem.clone()).await.unwrap(),
                Weak::new(),
                None,
            )
            .await
            .unwrap();

            let _premount_blob = volumes_directory
                .mount_volume(PREMOUNT_BLOB, None, true)
                .await
                .expect("Reopen volume");
            let _premount_noblob = volumes_directory
                .mount_volume(PREMOUNT_NOBLOB, None, false)
                .await
                .expect("Reopen volume");
            let _live_blob =
                volumes_directory.mount_volume(LIVE_BLOB, None, true).await.expect("Reopen volume");
            let _live_noblob = volumes_directory
                .mount_volume(LIVE_NOBLOB, None, false)
                .await
                .expect("Reopen volume");

            // Verify which recordings ran based on the saved recordings.
            volumes_directory
                .delete_profile(PREMOUNT_BLOB, RECORDING_NAME)
                .await
                .expect("Finding profile to delete.");
            volumes_directory
                .delete_profile(PREMOUNT_NOBLOB, RECORDING_NAME)
                .await
                .expect_err("Profile should not exist");
            volumes_directory
                .delete_profile(LIVE_BLOB, RECORDING_NAME)
                .await
                .expect("Finding profile to delete.");
            volumes_directory
                .delete_profile(LIVE_NOBLOB, RECORDING_NAME)
                .await
                .expect_err("Profile should not exist");

            volumes_directory.terminate().await;
        }

        filesystem.close().await.expect("Filesystem close");
    }

    #[fuchsia::test(threads = 10)]
    async fn test_profile_stop() {
        let device = DeviceHolder::new(FakeDevice::new(8192, 512));
        let filesystem = FxFilesystem::new_empty(device).await.unwrap();
        let volumes_directory = VolumesDirectory::new(
            root_volume(filesystem.clone()).await.unwrap(),
            Weak::new(),
            None,
        )
        .await
        .unwrap();
        let volume = volumes_directory.create_and_mount_volume("foo", None, true).await.unwrap();

        // Run the recording with no time at all and ensure that it still shuts down properly.
        volumes_directory
            .clone()
            .record_or_replay_profile("foo".to_owned(), 0)
            .await
            .expect("Recording");

        // Delete will succeed once the profile is completed.
        while volumes_directory.delete_profile("foo", "foo").await.is_err() {
            fasync::Timer::new(Duration::from_millis(10)).await;
        }

        std::mem::drop(volume);
        volumes_directory.terminate().await;
        std::mem::drop(volumes_directory);
        filesystem.close().await.expect("Filesystem close");
    }

    #[fuchsia::test(threads = 10)]
    async fn test_delete_profile() {
        let device = DeviceHolder::new(FakeDevice::new(8192, 512));
        let filesystem = FxFilesystem::new_empty(device).await.unwrap();
        let volumes_directory = VolumesDirectory::new(
            root_volume(filesystem.clone()).await.unwrap(),
            Weak::new(),
            None,
        )
        .await
        .unwrap();
        let volume = volumes_directory.create_and_mount_volume("foo", None, true).await.unwrap();

        volumes_directory
            .clone()
            .record_or_replay_profile("foo".to_owned(), 600)
            .await
            .expect("Recording");

        // Missing volume name.
        assert_eq!(
            volumes_directory.delete_profile("bar", "foo").await.expect_err("File shouldn't exist"),
            Status::NOT_FOUND
        );

        // Missing Profile name.
        assert_eq!(
            volumes_directory.delete_profile("foo", "bar").await.expect_err("File shouldn't exist"),
            Status::NOT_FOUND
        );

        // Deletion fails as the file is not visible until after the recording completes.
        assert_eq!(
            volumes_directory.delete_profile("foo", "foo").await.expect_err("File shouldn't exist"),
            Status::NOT_FOUND
        );

        volumes_directory.finish_profiling().await;

        // Deletion should now succeed as the profile will be placed as part of `finish_profiling()`
        volumes_directory.delete_profile("foo", "foo").await.expect("Deleting");

        // Deletion fails as the file shouldn't exist anymore.
        assert_eq!(
            volumes_directory.delete_profile("foo", "foo").await.expect_err("File shouldn't exist"),
            Status::NOT_FOUND
        );

        std::mem::drop(volume);
        volumes_directory.terminate().await;
        std::mem::drop(volumes_directory);
        filesystem.close().await.expect("Filesystem close");
    }
}
