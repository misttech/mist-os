// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod fvm_container;
mod fxfs_container;

use crate::copier::recursive_copy;
use crate::crypt::fxfs::{self, CryptService};
use crate::crypt::zxcrypt::{UnsealOutcome, ZxcryptDevice};
use crate::crypt::{get_policy, Policy};
use crate::device::constants::{
    BLOBFS_PARTITION_LABEL, BLOBFS_TYPE_GUID, DATA_PARTITION_LABEL, DATA_TYPE_GUID,
    DATA_VOLUME_LABEL, DEFAULT_F2FS_MIN_BYTES, FVM_DRIVER_PATH, LEGACY_DATA_PARTITION_LABEL,
    UNENCRYPTED_VOLUME_LABEL, ZXCRYPT_DRIVER_PATH,
};
use crate::device::{BlockDevice, Device, RegisteredDevices};
use crate::inspect::register_migration_status;
use crate::watcher::{DirSource, Watcher};
use anyhow::{anyhow, bail, Context, Error};
use async_trait::async_trait;
use device_watcher::{recursive_wait, recursive_wait_and_open};
use fidl::endpoints::{create_proxy, ServerEnd};
use fidl_fuchsia_fs_startup::MountOptions;
use fidl_fuchsia_hardware_block_partition::Guid;
use fidl_fuchsia_hardware_block_volume::{VolumeManagerMarker, VolumeMarker};
use fs_management::filesystem::{
    BlockConnector, ServingMultiVolumeFilesystem, ServingSingleVolumeFilesystem, ServingVolume,
};
use fs_management::format::DiskFormat;
use fs_management::partition::fvm_allocate_partition;
use fs_management::{Blobfs, ComponentType, F2fs, FSConfig, Fvm, Fxfs, Minfs};
use fuchsia_component::client::{connect_to_protocol, connect_to_protocol_at_path};
use futures::lock::Mutex;
use fvm_container::FvmContainer;
pub use fxfs_container::FxfsContainer;
use std::collections::HashSet;
use std::sync::Arc;
use uuid::Uuid;
use {fidl_fuchsia_io as fio, fuchsia_async as fasync, fuchsia_inspect as finspect};

const INITIAL_SLICE_COUNT: u64 = 1;

/// Returned from Environment::launch_data to signal when formatting is required.
pub enum ServeFilesystemStatus {
    Serving(Filesystem),
    FormatRequired,
}

/// Environment is a trait that performs actions when a device is matched.
/// Nb: matcher.rs depend on this interface being used in order to mock tests.
#[async_trait]
pub trait Environment: Send + Sync {
    /// Attaches the specified driver to the device.
    async fn attach_driver(&self, device: &mut dyn Device, driver_path: &str) -> Result<(), Error>;

    /// Binds an instance of storage-host to the given device.
    async fn launch_storage_host(&mut self, device: &mut dyn Device) -> Result<(), Error>;

    /// Binds the fvm driver and returns a list of the names of the child partitions.
    async fn bind_and_enumerate_fvm(
        &mut self,
        device: &mut dyn Device,
    ) -> Result<Vec<String>, Error>;

    /// Creates a static instance of Fxfs on `device` and calls serve_multi_volume(). Only creates
    /// the overall Fxfs instance. Mount_blob_volume and mount_data_volume still need to be called.
    async fn mount_fxblob(&mut self, device: &mut dyn Device) -> Result<(), Error>;

    /// Creates a static instance of Fxfs on `device` and calls serve_multi_volume(). Only creates
    /// the overall Fvm instance. Mount_blob_volume and mount_data_volume still need to be called.
    async fn mount_fvm(&mut self, device: &mut dyn Device) -> Result<(), Error>;

    /// Mounts the blob volume on the already mounted container filesystem.
    async fn mount_blob_volume(&mut self) -> Result<(), Error>;

    /// Mounts data volume on the already mounted container filesystem.
    async fn mount_data_volume(&mut self) -> Result<(), Error>;

    /// Called after the fvm driver is bound. Waits for the block driver to bind itself to the
    /// blobfs partition before creating a blobfs BlockDevice, which it passes into mount_blobfs().
    async fn mount_blobfs_on(&mut self, blobfs_partition_name: &str) -> Result<(), Error>;

    /// Called after the fvm driver is bound. Waits for the block driver to bind itself to the
    /// data partition before creating a data BlockDevice, which it then passes into launch_data().
    /// Calls bind_data() on the mounted filesystem.
    async fn mount_data_on(
        &mut self,
        data_partition_name: &str,
        is_fshost_ramdisk: bool,
    ) -> Result<(), Error>;

    /// Wipe and recreate data partition before reformatting with a filesystem.
    async fn format_data(&mut self, fvm_topo_path: &str) -> Result<Filesystem, Error>;

    /// Attempt to migrate |filesystem|, if requested, to some other format.
    /// |device| should refer to the FVM partition for |filesystem|.
    ///
    /// Returns either:
    ///   None if the original filesystem should be used.
    ///   Some(filesystem) a migrated to a different filesystem should be used.
    async fn try_migrate_data(
        &mut self,
        _device: &mut dyn Device,
        _filesystem: &mut Filesystem,
    ) -> Result<Option<Filesystem>, Error> {
        Ok(None)
    }

    /// Binds |filesystem| to the `/data` path. Fails if already bound.
    fn bind_data(&mut self, filesystem: Filesystem) -> Result<(), Error>;

    /// Shreds the data volume, triggering a reformat on reboot.
    /// The data volume must be Fxfs-formatted and must be currently serving.
    async fn shred_data(&mut self) -> Result<(), Error>;

    /// Synchronously shut down all associated filesystems.
    async fn shutdown(&mut self) -> Result<(), Error>;

    /// Returns the registered devices.
    fn registered_devices(&self) -> &Arc<RegisteredDevices>;
}

// Before a filesystem is mounted, we queue requests.
#[derive(Default)]
pub struct FilesystemQueue {
    exposed_dir_queue: Vec<ServerEnd<fio::DirectoryMarker>>,
    crypt_service_exposed_dir: Vec<ServerEnd<fio::DirectoryMarker>>,
}

pub enum Filesystem {
    Queue(FilesystemQueue),
    Serving(ServingSingleVolumeFilesystem),
    ServingMultiVolume(
        // We hold onto crypt service here to avoid it prematurely shutting down.
        // Fxfs may expect to find it in via VFS at a later time and it stops running
        // when all channels are closed.
        #[allow(dead_code)] CryptService,
        ServingMultiVolumeFilesystem,
        String,
    ),
    ServingVolumeInMultiVolume(
        // We hold onto crypt service here to avoid it prematurely shutting down.
        #[allow(dead_code)] Option<CryptService>,
        String,
    ),
    ServingGpt(ServingMultiVolumeFilesystem),
    Shutdown,
}

impl Filesystem {
    fn is_serving(&self) -> bool {
        if let Self::Queue(_) = self {
            false
        } else {
            true
        }
    }

    pub fn crypt_service(&mut self) -> Result<Option<fio::DirectoryProxy>, Error> {
        let (proxy, server) = create_proxy::<fio::DirectoryMarker>()?;
        match self {
            Filesystem::Queue(queue) => queue.crypt_service_exposed_dir.push(server),
            Filesystem::Serving(_) => bail!(anyhow!("filesystem doesn't have crypt service")),
            Filesystem::ServingMultiVolume(crypt_service, ..) => {
                crypt_service.exposed_dir().clone2(server.into_channel().into())?
            }
            Filesystem::ServingVolumeInMultiVolume(crypt_service_option, ..) => {
                match crypt_service_option {
                    Some(s) => s.exposed_dir().clone2(server.into_channel().into())?,
                    None => return Ok(None),
                }
            }
            Filesystem::ServingGpt(..) => bail!(anyhow!("GPT has no crypt service")),
            Filesystem::Shutdown => bail!(anyhow!("filesystem is shutting down")),
        }
        Ok(Some(proxy))
    }

    pub fn exposed_dir(
        &mut self,
        serving_fs: Option<&mut ServingMultiVolumeFilesystem>,
    ) -> Result<fio::DirectoryProxy, Error> {
        let (proxy, server) = create_proxy::<fio::DirectoryMarker>()?;
        match self {
            Filesystem::Queue(queue) => queue.exposed_dir_queue.push(server),
            Filesystem::Serving(fs) => fs.exposed_dir().clone2(server.into_channel().into())?,
            Filesystem::ServingMultiVolume(_, fs, data_volume_name) => fs
                .volume(&data_volume_name)
                .ok_or_else(|| anyhow!("data volume {} not found", data_volume_name))?
                .exposed_dir()
                .clone2(server.into_channel().into())?,
            Filesystem::ServingVolumeInMultiVolume(_, volume_name) => serving_fs
                .unwrap()
                .volume(&volume_name)
                .ok_or_else(|| anyhow!("volume {volume_name} not found"))?
                .exposed_dir()
                .clone2(server.into_channel().into())?,
            Filesystem::ServingGpt(fs) => fs.exposed_dir().clone2(server.into_channel().into())?,
            Filesystem::Shutdown => bail!(anyhow!("filesystem is shutting down")),
        }
        Ok(proxy)
    }

    pub fn root(
        &mut self,
        serving_fs: Option<&mut ServingMultiVolumeFilesystem>,
    ) -> Result<fio::DirectoryProxy, Error> {
        let root = fuchsia_fs::directory::open_directory_async(
            &self.exposed_dir(serving_fs).context("failed to get exposed dir")?,
            "root",
            fio::PERM_READABLE | fio::Flags::PERM_INHERIT_WRITE | fio::Flags::PERM_INHERIT_EXECUTE,
        )
        .context("failed to open the root directory")?;
        Ok(root)
    }

    pub fn volume(&mut self, volume_name: &str) -> Option<&mut ServingVolume> {
        match self {
            Filesystem::ServingMultiVolume(_, fs, _) => fs.volume_mut(&volume_name),
            Filesystem::ServingVolumeInMultiVolume(..) => unreachable!(),
            _ => None,
        }
    }

    fn queue(&mut self) -> Option<&mut FilesystemQueue> {
        match self {
            Filesystem::Queue(queue) => Some(queue),
            _ => None,
        }
    }

    pub async fn shutdown(
        &mut self,
        serving_fs: Option<&mut ServingMultiVolumeFilesystem>,
    ) -> Result<(), Error> {
        let old = std::mem::replace(self, Filesystem::Shutdown);
        match old {
            Filesystem::Queue(_) => Ok(()),
            Filesystem::Serving(fs) => fs.shutdown().await.context("shutdown failed"),
            Filesystem::ServingMultiVolume(_, fs, _) => {
                fs.shutdown().await.context("shutdown failed")
            }
            Filesystem::ServingVolumeInMultiVolume(_, volume_name) => {
                serving_fs.unwrap().shutdown_volume(&volume_name).await.context("shutdown failed")
            }
            Filesystem::ServingGpt(fs) => fs.shutdown().await.context("shutdown failed"),
            Filesystem::Shutdown => Err(anyhow!("double shutdown!")),
        }
    }
}

/// Trait that captures the common interface between different multi-volume filesystems (there
/// should be concrete implementations for Fxfs and Fvm).
#[async_trait]
pub trait Container: Send + Sync {
    /// Returns fs_management's wrapper around the container filesystem.
    fn fs(&mut self) -> &mut ServingMultiVolumeFilesystem;

    /// Converts into fs_management's wrapper.
    fn into_fs(self: Box<Self>) -> ServingMultiVolumeFilesystem;

    /// Returns the label used for the blob volume.
    fn blobfs_volume_label(&self) -> &'static str;

    /// Called to check the blob volume if required.
    async fn maybe_check_blob_volume(&mut self) -> Result<(), Error> {
        // Default to not checking.
        Ok(())
    }

    /// Returns the current set of volumes for this container.
    async fn get_volumes(&mut self) -> Result<Vec<String>, Error> {
        // We expect the startup protocol to work with fxblob. There are no options for
        // reformatting the entire fxfs partition now that blobfs is one of the volumes.
        let volumes_dir = fuchsia_fs::directory::open_directory(
            self.fs().exposed_dir(),
            "volumes",
            fio::Flags::empty(),
        )
        .await
        .context("opening volumes directory")?;
        Ok(fuchsia_fs::directory::readdir(&volumes_dir)
            .await
            .context("reading volumes directory")?
            .into_iter()
            .map(|e| e.name)
            .collect())
    }

    /// Serves the data volume on this container.
    async fn serve_data(&mut self, launcher: &FilesystemLauncher) -> Result<Filesystem, Error>;

    /// Recreates the data volume.
    async fn format_data(&mut self, launcher: &FilesystemLauncher) -> Result<Filesystem, Error>;

    /// Typically called by `format_data` implementations to remove all the non-blob volumes.
    async fn remove_all_non_blob_volumes(&mut self) -> Result<(), Error> {
        let blobfs_volume_label = self.blobfs_volume_label();
        let fs = self.fs();
        let volumes_dir = fuchsia_fs::directory::open_directory_async(
            fs.exposed_dir(),
            "volumes",
            fio::Flags::empty(),
        )?;
        let volumes = fuchsia_fs::directory::readdir(&volumes_dir).await?;
        for volume in volumes {
            if &volume.name != blobfs_volume_label {
                // Unmount mounted non-blob volumes.
                if fs.volume(&volume.name).is_some() {
                    fs.shutdown_volume(&volume.name).await.with_context(|| {
                        format!("unable to shutdown volume \"{}\" for reformat", volume.name)
                    })?;
                }
                // Remove any non-blob volumes.
                fs.remove_volume(&volume.name)
                    .await
                    .with_context(|| format!("failed to remove volume: {}", volume.name))?;
            }
        }
        Ok(())
    }

    /// Called to shred the encryption keys for the data volume.
    async fn shred_data(&mut self) -> Result<(), Error>;
}

/// This trait exists to make working with `container` easier below and avoid the
/// somewhat hard to read repeated implementation you see below.
trait MaybeFs {
    fn maybe_fs(&mut self) -> Option<&mut ServingMultiVolumeFilesystem>;
}

impl MaybeFs for Option<Box<dyn Container>> {
    fn maybe_fs(&mut self) -> Option<&mut ServingMultiVolumeFilesystem> {
        self.as_mut().map(|c| c.fs())
    }
}

/// Implements the Environment trait and keeps track of mounted filesystems.
pub struct FshostEnvironment {
    config: Arc<fshost_config::Config>,
    // When storage-host is enabled, the GPT is run as a component.
    gpt: Filesystem,
    // `container` is set inside mount_fxblob() or mount_fvm() and represents the overall
    // Fxfs/Fvm instance which contains both a data and blob volume.
    container: Option<Box<dyn Container>>,
    blobfs: Filesystem,
    data: Filesystem,
    fvm: Option<(/*topological_path*/ String, /*device_directory*/ fio::DirectoryProxy)>,
    launcher: Arc<FilesystemLauncher>,
    /// This lock can be taken and device.path() added to the vector to have them
    /// ignored the next time they appear to the Watcher/Matcher code.
    matcher_lock: Arc<Mutex<HashSet<String>>>,
    inspector: finspect::Inspector,
    watcher: Watcher,
    registered_devices: Arc<RegisteredDevices>,
}

impl FshostEnvironment {
    pub fn new(
        config: Arc<fshost_config::Config>,
        matcher_lock: Arc<Mutex<HashSet<String>>>,
        inspector: fuchsia_inspect::Inspector,
        watcher: Watcher,
    ) -> Self {
        let corruption_events = inspector.root().create_child("corruption_events");
        Self {
            config: config.clone(),
            gpt: Filesystem::Queue(FilesystemQueue::default()),
            container: None,
            blobfs: Filesystem::Queue(FilesystemQueue::default()),
            data: Filesystem::Queue(FilesystemQueue::default()),
            fvm: None,
            launcher: Arc::new(FilesystemLauncher { config, corruption_events }),
            matcher_lock,
            inspector,
            watcher,
            registered_devices: Arc::new(RegisteredDevices::default()),
        }
    }

    fn get_fvm(&self) -> Result<(&String, &fio::DirectoryProxy), Error> {
        debug_assert!(
            self.fvm.is_some(),
            "fvm was not initialized, ensure `bind_and_enumerate_fvm()` was called!"
        );
        if let Some((ref fvm_topo_path, ref fvm_dir)) = self.fvm {
            return Ok((fvm_topo_path, fvm_dir));
        }
        bail!("fvm was not initialized");
    }

    /// Returns a proxy for the exposed dir of the Blobfs filesystem.  This can be called before
    /// Blobfs is mounted and it will get routed once Blobfs is mounted.
    pub fn blobfs_exposed_dir(&mut self) -> Result<fio::DirectoryProxy, Error> {
        self.blobfs.exposed_dir(self.container.maybe_fs())
    }

    /// Returns a proxy for the exposed dir of the data filesystem.  This can be called before
    /// "/data" is mounted and it will get routed once the data partition is mounted.
    pub fn data_exposed_dir(&mut self) -> Result<fio::DirectoryProxy, Error> {
        self.data.exposed_dir(self.container.maybe_fs())
    }

    /// Returns a proxy for the exposed dir of the GPT.  This must be called before
    /// the GPT is mounted and it will get routed once bound.
    pub fn gpt_exposed_dir(&mut self) -> Result<fio::DirectoryProxy, Error> {
        self.gpt.exposed_dir(None)
    }

    /// Returns a proxy for the exposed dir of the data filesystem's crypt service. This can be
    /// called before "/data" is mounted and it will get routed once the data partition is
    /// mounted and the crypt service becomes available. Fails for single volume filesystems.
    pub fn crypt_service_exposed_dir(&mut self) -> Result<Option<fio::DirectoryProxy>, Error> {
        self.data.crypt_service()
    }

    /// Returns a proxy for the root of the data filesystem.  This can be called before "/data" is
    /// mounted and it will get routed once the data partition is mounted.
    pub fn data_root(&mut self) -> Result<fio::DirectoryProxy, Error> {
        self.data.root(self.container.maybe_fs())
    }

    pub fn launcher(&self) -> Arc<FilesystemLauncher> {
        self.launcher.clone()
    }

    /// Set the max partition size for data
    async fn apply_data_partition_limits(&self, device: &mut dyn Device) {
        if !device.is_fshost_ramdisk() {
            if let Err(error) = device.set_partition_max_bytes(self.config.data_max_bytes).await {
                tracing::warn!(%error, "Failed to set max partition size for data");
            }
        }
    }

    /// Formats a device with the specified disk format. Normally we only format the configured
    /// format but this level of abstraction lets us select the format for use in migration code
    /// paths.
    async fn format_data_with_disk_format(
        &mut self,
        format: DiskFormat,
        device: &mut dyn Device,
    ) -> Result<Filesystem, Error> {
        // Potentially bind and format zxcrypt first.
        let mut zxcrypt_device;
        let device = if (self.config.use_disk_migration && format == DiskFormat::Zxcrypt)
            || self.launcher.requires_zxcrypt(format, device.is_fshost_ramdisk())
        {
            tracing::info!(
                path = device.path(),
                "Formatting zxcrypt before formatting inner data partition.",
            );
            let ignore_paths = &mut *self.matcher_lock.lock().await;
            self.attach_driver(device, ZXCRYPT_DRIVER_PATH).await?;
            zxcrypt_device =
                Box::new(ZxcryptDevice::format(device).await.context("zxcrypt format failed")?);
            ignore_paths.insert(zxcrypt_device.topological_path().to_string());
            zxcrypt_device.as_mut()
        } else {
            device
        };

        // Set the max partition size for data
        self.apply_data_partition_limits(device).await;

        let filesystem = match format {
            DiskFormat::Fxfs => {
                let config =
                    Fxfs { component_type: ComponentType::StaticChild, ..Default::default() };
                self.launcher.format_data(device, config).await?
            }
            DiskFormat::F2fs => {
                let config =
                    F2fs { component_type: ComponentType::StaticChild, ..Default::default() };
                self.launcher.format_data(device, config).await?
            }
            DiskFormat::Minfs => {
                let config =
                    Minfs { component_type: ComponentType::StaticChild, ..Default::default() };
                self.launcher.format_data(device, config).await?
            }
            format => {
                tracing::warn!("Unsupported format {:?}", format);
                return Err(anyhow!("Cannot format filesystem"));
            }
        };

        Ok(filesystem)
    }

    async fn try_migrate_data_internal(
        &mut self,
        device: &mut dyn Device,
        filesystem: &mut Filesystem,
    ) -> Result<Option<Filesystem>, Error> {
        // Take note of the original device GUID. We will mark it inactive on success.
        let device_guid = device.partition_instance().await.map(|guid| *guid).ok();

        // The filesystem may be zxcrypt wrapped so we can't use device.content_type() here.
        // We query the FS directly.
        let device_format = match filesystem {
            Filesystem::Serving(filesystem) => {
                if let Some(vfs_type) =
                    fidl_fuchsia_fs::VfsType::from_primitive(filesystem.query().await?.fs_type)
                {
                    match vfs_type {
                        fidl_fuchsia_fs::VfsType::Minfs => DiskFormat::Minfs,
                        fidl_fuchsia_fs::VfsType::F2Fs => DiskFormat::F2fs,
                        fidl_fuchsia_fs::VfsType::Fxfs => DiskFormat::Fxfs,
                        _ => {
                            return Ok(None);
                        }
                    }
                } else {
                    return Ok(None);
                }
            }
            Filesystem::ServingMultiVolume(_, _, _) => DiskFormat::Fxfs,
            _ => {
                return Ok(None);
            }
        };

        let root = filesystem.root(None)?;

        // Read desired format from fs_switch, use config as default.
        let desired_format = match fuchsia_fs::directory::open_file(
            &root,
            "fs_switch",
            fio::PERM_READABLE,
        )
        .await
        {
            Ok(file) => {
                let mut desired_format = fuchsia_fs::file::read_to_string(&file).await?;
                desired_format = desired_format.trim_end().to_string();
                // "toggle" is a special format request that flip-flops between fxfs and minfs.
                if desired_format.as_str() == "toggle" {
                    desired_format =
                        if device_format == DiskFormat::Fxfs { "minfs" } else { "fxfs" }
                            .to_string();
                }
                desired_format
            }
            Err(error) => {
                tracing::info!(%error, default_format=self.config.data_filesystem_format.as_str(),
                    "fs_switch open failed.");
                self.config.data_filesystem_format.to_string()
            }
        };
        let desired_format = match desired_format.as_str().into() {
            DiskFormat::Fxfs => DiskFormat::Fxfs,
            DiskFormat::F2fs => DiskFormat::F2fs,
            _ => DiskFormat::Minfs,
        };

        if device_format != desired_format {
            tracing::info!(
                device_format = device_format.as_str(),
                desired_format = desired_format.as_str(),
                "Attempting migration"
            );

            let volume_manager = connect_to_protocol_at_path::<VolumeManagerMarker>(
                &device.fvm_path().ok_or_else(|| anyhow!("Not an fvm device"))?,
            )
            .context("Failed to connect to fvm volume manager")?;
            let new_instance_guid = *uuid::Uuid::new_v4().as_bytes();

            // Note that if we are using fxfs or f2fs we should always be setting data_max_bytes
            // because these filesystems cannot automatically grow/shrink themselves.
            let slices = self.config.data_max_bytes / self.config.fvm_slice_size;
            if slices == 0 {
                bail!("data_max_bytes not set. Cannot migrate.");
            }
            tracing::info!(slices, "Allocating new partition");
            let new_data_partition_controller = fvm_allocate_partition(
                &volume_manager,
                DATA_TYPE_GUID,
                new_instance_guid,
                DATA_PARTITION_LABEL,
                fidl_fuchsia_hardware_block_volume::ALLOCATE_PARTITION_FLAG_INACTIVE,
                slices,
            )
            .await
            .context("Allocate migration partition.")?;

            let device_path = new_data_partition_controller
                .get_topological_path()
                .await?
                .map_err(zx::Status::from_raw)?;

            let mut new_device =
                BlockDevice::from_proxy(new_data_partition_controller, device_path).await?;

            let mut new_filesystem =
                self.format_data_with_disk_format(desired_format, &mut new_device).await?;

            recursive_copy(&filesystem.root(None)?, &new_filesystem.root(None)?)
                .await
                .context("copy data")?;

            // Ensure the watcher won't process the device we just added.
            {
                let mut ignore_paths = self.matcher_lock.lock().await;
                ignore_paths.insert(new_device.topological_path().to_string());
            }

            // Mark the old partition inactive (deletion at next boot).
            if let Some(old_instance_guid) = device_guid {
                zx::Status::ok(
                    volume_manager
                        .activate(
                            &Guid { value: old_instance_guid },
                            &Guid { value: new_instance_guid },
                        )
                        .await?,
                )?;
            }
            Ok(Some(new_filesystem))
        } else {
            Ok(None)
        }
    }

    /// Mounts Blobfs on the given device.
    async fn mount_blobfs(&mut self, device: &mut dyn Device) -> Result<(), Error> {
        let queue = self.blobfs.queue().ok_or_else(|| anyhow!("blobfs already mounted"))?;

        let mut fs = self.launcher.serve_blobfs(device).await?;

        let exposed_dir = fs.exposed_dir(None)?;
        for server in queue.exposed_dir_queue.drain(..) {
            exposed_dir.clone2(server.into_channel().into())?;
        }
        self.blobfs = fs;
        Ok(())
    }

    /// Launch data partition on the given device.
    /// If formatting is required, returns ServeFilesystemStatus::FormatRequired.
    async fn launch_data(
        &mut self,
        device: &mut dyn Device,
    ) -> Result<ServeFilesystemStatus, Error> {
        let _ = self.data.queue().ok_or_else(|| anyhow!("data partition already mounted"))?;

        let mut format: DiskFormat = if self.config.use_disk_migration {
            let format = device.content_format().await?;
            tracing::info!(
                format = format.as_str(),
                "Using detected disk format to potentially \
                migrate data"
            );
            format
        } else {
            match self.config.data_filesystem_format.as_str().into() {
                DiskFormat::Fxfs => DiskFormat::Fxfs,
                DiskFormat::F2fs => DiskFormat::F2fs,
                // Default to minfs if we don't match expected filesystems.
                _ => DiskFormat::Minfs,
            }
        };

        // Potentially bind and unseal zxcrypt before serving data.
        let mut zxcrypt_device = None;
        let device = if (self.config.use_disk_migration && format == DiskFormat::Zxcrypt)
            || self.launcher.requires_zxcrypt(format, device.is_fshost_ramdisk())
        {
            tracing::info!(path = device.path(), "Attempting to unseal zxcrypt device",);
            let ignore_paths = &mut *self.matcher_lock.lock().await;
            self.attach_driver(device, ZXCRYPT_DRIVER_PATH).await?;
            let mut new_device = match ZxcryptDevice::unseal(device).await? {
                UnsealOutcome::Unsealed(device) => device,
                UnsealOutcome::FormatRequired => {
                    tracing::warn!("failed to unseal zxcrypt, format required");
                    return Ok(ServeFilesystemStatus::FormatRequired);
                }
            };
            // If we are using content sniffing to identify the filesystem, we have to do it again
            // after unsealing zxcrypt.
            if self.config.use_disk_migration {
                format = new_device.content_format().await?;
                tracing::info!(format = format.as_str(), "detected zxcrypt wrapped format");
            }
            ignore_paths.insert(new_device.topological_path().to_string());
            zxcrypt_device = Some(Box::new(new_device));
            zxcrypt_device.as_mut().unwrap().as_mut()
        } else {
            device
        };

        // Set the max partition size for data
        self.apply_data_partition_limits(device).await;

        let filesystem = match format {
            DiskFormat::Fxfs => {
                let config =
                    Fxfs { component_type: ComponentType::StaticChild, ..Default::default() };
                self.launcher.serve_data(device, config).await
            }
            DiskFormat::F2fs => {
                let config =
                    F2fs { component_type: ComponentType::StaticChild, ..Default::default() };
                self.launcher.serve_data(device, config).await
            }
            DiskFormat::Minfs => {
                let config =
                    Minfs { component_type: ComponentType::StaticChild, ..Default::default() };
                self.launcher.serve_data(device, config).await
            }
            format => {
                tracing::warn!(format = format.as_str(), "Unsupported filesystem");
                Ok(ServeFilesystemStatus::FormatRequired)
            }
        }?;

        if let ServeFilesystemStatus::FormatRequired = filesystem {
            if let Some(device) = zxcrypt_device {
                tracing::info!(path = device.path(), "Resealing zxcrypt device due to error.");
                device.seal().await?;
            }
        }

        Ok(filesystem)
    }
}

#[async_trait]
impl Environment for FshostEnvironment {
    async fn attach_driver(&self, device: &mut dyn Device, driver_path: &str) -> Result<(), Error> {
        self.launcher.attach_driver(device, driver_path).await
    }

    async fn launch_storage_host(&mut self, device: &mut dyn Device) -> Result<(), Error> {
        let queue = self.gpt.queue().ok_or_else(|| anyhow!("GPT already bound"))?;
        let filesystem = fs_management::filesystem::Filesystem::from_boxed_config(
            device.block_connector()?,
            Box::new(fs_management::Gpt::dynamic_child()),
        )
        .serve_multi_volume()
        .await
        .context("Failed to start GPT")?;
        let exposed_dir = filesystem.exposed_dir();
        for server in queue.exposed_dir_queue.drain(..) {
            exposed_dir.clone2(server.into_channel().into())?;
        }
        let partitions_dir = fuchsia_fs::directory::open_directory(
            &exposed_dir,
            "partitions",
            fio::Flags::PERM_CONNECT | fio::Flags::PERM_ENUMERATE | fio::Flags::PERM_TRAVERSE,
        )
        .await?;
        const PARTITION_SERVICE_SUFFIX: &str = "/svc/fuchsia.storagehost.PartitionService";
        self.watcher
            .add_source(Box::new(DirSource::with_suffix(partitions_dir, PARTITION_SERVICE_SUFFIX)))
            .await
            .context("Failed to watch gpt partitions dir")?;
        self.gpt = Filesystem::ServingGpt(filesystem);
        Ok(())
    }

    async fn bind_and_enumerate_fvm(
        &mut self,
        device: &mut dyn Device,
    ) -> Result<Vec<String>, Error> {
        // Attach the FVM driver and connect to the VolumeManager.
        self.attach_driver(device, FVM_DRIVER_PATH).await?;
        let fvm_dir = fuchsia_fs::directory::open_in_namespace(
            &device.topological_path(),
            fio::PERM_READABLE,
        )?;
        let fvm_volume_manager_proxy =
            recursive_wait_and_open::<VolumeManagerMarker>(&fvm_dir, "/fvm")
                .await
                .context("failed to connect to the VolumeManager")?;

        // **NOTE**: We must call VolumeManager::GetInfo() to ensure all partitions are visible when
        // we enumerate them below. See https://fxbug.dev/42077585 for more information.
        zx::ok(fvm_volume_manager_proxy.get_info().await.context("transport error on get_info")?.0)
            .context("get_info failed")?;

        let fvm_topo_path = format!("{}/fvm", device.topological_path());
        let fvm_dir = fuchsia_fs::directory::open_in_namespace(&fvm_topo_path, fio::PERM_READABLE)?;
        let dir_entries = fuchsia_fs::directory::readdir(&fvm_dir).await?;

        self.fvm = Some((fvm_topo_path, fvm_dir));
        Ok(dir_entries.into_iter().map(|entry| entry.name).collect())
    }

    async fn mount_fxblob(&mut self, device: &mut dyn Device) -> Result<(), Error> {
        tracing::info!(
            path = %device.path(),
            expected_format = "fxfs",
            "Mounting fxblob"
        );
        let serving_fs = self.launcher.serve_fxblob(device.block_connector()?).await?;
        self.container = Some(Box::new(FxfsContainer::new(serving_fs)));
        Ok(())
    }

    async fn mount_fvm(&mut self, device: &mut dyn Device) -> Result<(), Error> {
        tracing::info!(
            path = %device.path(),
            expected_format = "fvm",
            "Mounting fvm"
        );
        let serving_fs = self.launcher.serve_fvm(device).await?;
        self.container = Some(Box::new(FvmContainer::new(serving_fs, device.is_fshost_ramdisk())));
        Ok(())
    }

    async fn mount_blob_volume(&mut self) -> Result<(), Error> {
        let _ = self.blobfs.queue().ok_or_else(|| anyhow!("blobfs partition already mounted"))?;

        let container = self.container.as_mut().ok_or_else(|| anyhow!("Missing container!"))?;

        container.maybe_check_blob_volume().await?;

        let label = container.blobfs_volume_label();
        let blobfs = container
            .fs()
            .open_volume(
                label,
                MountOptions {
                    as_blob: Some(true),
                    uri: Some(format!("#meta/blobfs.cm")),
                    ..MountOptions::default()
                },
            )
            .await
            .context("Failed to open the blob volume")?;
        let exposed_dir = blobfs.exposed_dir();
        let queue = self.blobfs.queue().ok_or_else(|| anyhow!("blobfs already mounted"))?;
        for server in queue.exposed_dir_queue.drain(..) {
            exposed_dir.clone2(server.into_channel().into())?;
        }
        self.blobfs = Filesystem::ServingVolumeInMultiVolume(None, label.to_string());
        if let Err(e) = container.fs().set_byte_limit(label, self.config.blobfs_max_bytes).await {
            tracing::warn!("Failed to set byte limit for the blob volume: {:?}", e);
        }
        Ok(())
    }

    async fn mount_data_volume(&mut self) -> Result<(), Error> {
        let _ = self.data.queue().ok_or_else(|| anyhow!("data partition already mounted"))?;

        let container = self.container.as_mut().ok_or_else(|| anyhow!("Missing container!"))?;
        let mut filesystem = container.serve_data(&self.launcher).await?;
        if let Err(e) =
            container.fs().set_byte_limit(DATA_VOLUME_LABEL, self.config.data_max_bytes).await
        {
            tracing::warn!("Failed to set byte limit for the data volume: {:?}", e);
        }
        let queue = self.data.queue().unwrap();
        let exposed_dir = filesystem.exposed_dir(Some(container.fs()))?;
        for server in queue.exposed_dir_queue.drain(..) {
            exposed_dir.clone2(server.into_channel().into())?;
        }
        if let Some(crypt_service) = filesystem.crypt_service()? {
            for server in queue.crypt_service_exposed_dir.drain(..) {
                crypt_service.clone2(server.into_channel().into())?;
            }
        }
        self.data = filesystem;
        Ok(())
    }

    async fn mount_blobfs_on(&mut self, blobfs_partition_name: &str) -> Result<(), Error> {
        let (fvm_topo_path, fvm_dir) = self.get_fvm()?;
        let blobfs_topo_path = format!("{}/{blobfs_partition_name}/block", fvm_topo_path);
        recursive_wait(fvm_dir, &format!("{blobfs_partition_name}/block"))
            .await
            .context("failed to bind block driver to blobfs device")?;
        let mut device = BlockDevice::new(blobfs_topo_path)
            .await
            .context("failed to create blobfs block device")?;
        let (label, type_guid) =
            (device.partition_label().await?.to_string(), *device.partition_type().await?);
        if !(label == BLOBFS_PARTITION_LABEL && type_guid == BLOBFS_TYPE_GUID) {
            tracing::error!(
                "incorrect parameters for blobfs partition: label = {}, type = {:?}",
                label,
                type_guid
            );
            bail!("blobfs partition has incorrect label/guid");
        }
        self.mount_blobfs(&mut device).await
    }

    async fn mount_data_on(
        &mut self,
        data_partition_name: &str,
        is_fshost_ramdisk: bool,
    ) -> Result<(), Error> {
        let (fvm_topo_path, fvm_dir) = self.get_fvm()?;
        let data_topo_path = format!("{}/{data_partition_name}/block", fvm_topo_path);
        recursive_wait(fvm_dir, &format!("{data_partition_name}/block"))
            .await
            .context("failed to bind block driver to the data device")?;
        let mut device = BlockDevice::new(data_topo_path)
            .await
            .context("failed to create blobfs block device")?;
        device.set_fshost_ramdisk(is_fshost_ramdisk);
        let (label, type_guid) =
            (device.partition_label().await?.to_string(), *device.partition_type().await?);
        if !((label == DATA_PARTITION_LABEL || label == LEGACY_DATA_PARTITION_LABEL)
            && type_guid == DATA_TYPE_GUID)
        {
            tracing::error!(
                "incorrect parameters for data partition: label = {}, type = {:?}",
                label,
                type_guid
            );
            bail!("data partition has incorrect label/guid");
        }

        let fs = match self.launch_data(&mut device).await? {
            ServeFilesystemStatus::Serving(mut filesystem) => {
                // If this build supports migrating data partition, try now, failing back to
                // just using `filesystem` in the case of any error. Non-migration builds
                // should return Ok(None).
                match self.try_migrate_data(&mut device, &mut filesystem).await {
                    Ok(Some(new_filesystem)) => {
                        // Migration successful.
                        filesystem.shutdown(None).await.unwrap_or_else(|error| {
                            tracing::error!(
                                ?error,
                                "Failed to shutdown original filesystem after migration"
                            );
                        });
                        new_filesystem
                    }
                    Ok(None) => filesystem, // Migration not requested.
                    Err(error) => {
                        // Migration failed.
                        tracing::error!(?error, "Failed to migrate filesystem");
                        // TODO: Log migration failure metrics.
                        // Continue with the original (unmigrated) filesystem.
                        filesystem
                    }
                }
            }
            ServeFilesystemStatus::FormatRequired => {
                self.format_data(&device.fvm_path().ok_or_else(|| anyhow!("Not an fvm device"))?)
                    .await?
            }
        };
        self.bind_data(fs)
    }

    async fn format_data(&mut self, fvm_topo_path: &str) -> Result<Filesystem, Error> {
        // Reset FVM partition first, ensuring we blow away any existing zxcrypt volume.
        let mut device = self
            .launcher
            .reset_fvm_partition(fvm_topo_path, &mut *self.matcher_lock.lock().await)
            .await
            .with_context(|| {
                format!("Failed to reset non-blob FVM partitions on {}", fvm_topo_path)
            })?;
        let device = device.as_mut();

        // Default to minfs if we don't match expected filesystems.
        let format: DiskFormat = match self.config.data_filesystem_format.as_str().into() {
            DiskFormat::Fxfs => DiskFormat::Fxfs,
            DiskFormat::F2fs => DiskFormat::F2fs,
            _ => DiskFormat::Minfs,
        };

        // Rotate hardware derived key before formatting if we follow a Tee policy
        if get_policy().await? != Policy::Null {
            tracing::info!("Rotate hardware derived key before formatting");
            // Hardware derived keys are not rotatable on certain devices.
            // TODO(b/271166111): Assert hard fail when we know rotating the key should work.
            match kms_stateless::rotate_hardware_derived_key(kms_stateless::KeyInfo::new("zxcrypt"))
                .await
            {
                Ok(()) => {}
                Err(kms_stateless::Error::TeeCommandNotSupported(
                    kms_stateless::TaKeysafeCommand::RotateHardwareDerivedKey,
                )) => {
                    tracing::warn!("The device does not support rotatable hardware keys.")
                }
                Err(e) => {
                    tracing::warn!("Rotate hardware key failed with error {:?}.", e)
                }
            }
        }

        self.format_data_with_disk_format(format, device).await
    }

    async fn try_migrate_data(
        &mut self,
        device: &mut dyn Device,
        filesystem: &mut Filesystem,
    ) -> Result<Option<Filesystem>, Error> {
        if !self.config.use_disk_migration {
            return Ok(None);
        }

        let res = self.try_migrate_data_internal(device, filesystem).await;
        if let Err(error) = &res {
            tracing::warn!(%error, "migration failed");
            if let Some(status) = error.downcast_ref::<zx::Status>().clone() {
                register_migration_status(self.inspector.root(), *status).await;
            } else {
                register_migration_status(self.inspector.root(), zx::Status::INTERNAL).await;
            }
        } else {
            register_migration_status(self.inspector.root(), zx::Status::OK).await;
        }
        res
    }

    fn bind_data(&mut self, mut filesystem: Filesystem) -> Result<(), Error> {
        let _ = self.data.queue().ok_or_else(|| anyhow!("data partition already mounted"))?;

        let queue = self.data.queue().unwrap();
        let exposed_dir = filesystem.exposed_dir(None)?;
        for server in queue.exposed_dir_queue.drain(..) {
            exposed_dir.clone2(server.into_channel().into())?;
        }
        match &filesystem {
            Filesystem::ServingMultiVolume(..) | Filesystem::ServingVolumeInMultiVolume(..) => {
                if let Some(crypt_service_exposed_dir) = filesystem.crypt_service()? {
                    for server in queue.crypt_service_exposed_dir.drain(..) {
                        crypt_service_exposed_dir.clone2(server.into_channel().into())?;
                    }
                }
            }
            _ => {}
        }

        self.data = filesystem;
        Ok(())
    }

    async fn shred_data(&mut self) -> Result<(), Error> {
        if !self.data.is_serving() {
            return Err(anyhow!("Can't shred data; not already mounted"));
        }
        if let Some(container) = self.container.as_mut() {
            container.shred_data().await
        } else {
            if self.config.data_filesystem_format != "fxfs" {
                return Err(anyhow!("Can't shred data; not fxfs"));
            }
            fxfs::shred_key_bag(
                self.data
                    .volume(UNENCRYPTED_VOLUME_LABEL)
                    .context("Failed to find unencrypted volume")?,
            )
            .await
        }
    }

    async fn shutdown(&mut self) -> Result<(), Error> {
        // If we encounter an error, log it, but continue trying to shut down the remaining
        // filesystems.
        self.blobfs.shutdown(self.container.maybe_fs()).await.unwrap_or_else(|error| {
            tracing::error!(?error, "failed to shut down blobfs");
        });
        self.data.shutdown(self.container.maybe_fs()).await.unwrap_or_else(|error| {
            tracing::error!(?error, "failed to shut down data");
        });
        if let Some(container) = self.container.take() {
            container.into_fs().shutdown().await.unwrap_or_else(|error| {
                tracing::error!(?error, "failed to shut down fxfs");
            })
        }
        self.gpt = Filesystem::Queue(FilesystemQueue::default());
        Ok(())
    }

    fn registered_devices(&self) -> &Arc<RegisteredDevices> {
        &self.registered_devices
    }
}

pub struct FilesystemLauncher {
    config: Arc<fshost_config::Config>,
    corruption_events: finspect::Node,
}

impl FilesystemLauncher {
    pub async fn attach_driver(
        &self,
        device: &mut dyn Device,
        driver_path: &str,
    ) -> Result<(), Error> {
        tracing::info!(path = %device.path(), %driver_path, "Binding driver to device");
        match device.controller().bind(driver_path).await?.map_err(zx::Status::from_raw) {
            Err(e) if e == zx::Status::ALREADY_BOUND => {
                // It's fine if we get an ALREADY_BOUND error.
                tracing::info!(path = %device.path(), %driver_path,
                    "Ignoring ALREADY_BOUND error.");
                Ok(())
            }
            Err(e) => Err(e.into()),
            Ok(()) => Ok(()),
        }
    }

    pub fn requires_zxcrypt(&self, format: DiskFormat, is_ramdisk: bool) -> bool {
        match format {
            // Fxfs never has zxcrypt underneath
            DiskFormat::Fxfs => false,
            _ if self.config.no_zxcrypt => false,
            // No point using zxcrypt for ramdisk devices.
            _ if is_ramdisk => false,
            _ => true,
        }
    }

    pub fn get_blobfs_config(&self) -> Blobfs {
        Blobfs {
            write_compression_algorithm: self
                .config
                .blobfs_write_compression_algorithm
                .as_str()
                .into(),
            cache_eviction_policy_override: self
                .config
                .blobfs_cache_eviction_policy
                .as_str()
                .into(),
            ..Default::default()
        }
    }

    pub async fn serve_blobfs(&self, device: &mut dyn Device) -> Result<Filesystem, Error> {
        tracing::info!(path = %device.path(), "Mounting /blob");

        // Setting max partition size for blobfs
        if !device.is_fshost_ramdisk() {
            if let Err(e) = device.set_partition_max_bytes(self.config.blobfs_max_bytes).await {
                tracing::warn!("Failed to set max partition size for blobfs: {:?}", e);
            };
        }

        let config = Blobfs {
            component_type: fs_management::ComponentType::StaticChild,
            ..self.get_blobfs_config()
        };
        let fs = fs_management::filesystem::Filesystem::from_boxed_config(
            device.block_connector()?,
            Box::new(config),
        )
        .serve()
        .await
        .context("serving blobfs")?;

        Ok(Filesystem::Serving(fs))
    }

    pub async fn serve_data<FSC: FSConfig>(
        &self,
        device: &mut dyn Device,
        config: FSC,
    ) -> Result<ServeFilesystemStatus, Error> {
        let fs = fs_management::filesystem::Filesystem::from_boxed_config(
            device.block_connector()?,
            Box::new(config),
        );
        let format = fs.config().disk_format();
        tracing::info!(
            path = %device.path(),
            expected_format = ?format,
            "Mounting /data"
        );

        let detected_format = device.content_format().await?;
        if detected_format != format {
            tracing::info!(
                ?detected_format,
                expected_format = ?format,
                "Expected format not detected. Reformatting.",
            );
            return Ok(ServeFilesystemStatus::FormatRequired);
        }
        self.serve_data_from(fs).await
    }

    // NB: keep these larger functions monomorphized, otherwise they cause significant code size
    // increases.
    pub async fn serve_data_from(
        &self,
        mut fs: fs_management::filesystem::Filesystem,
    ) -> Result<ServeFilesystemStatus, Error> {
        let format = fs.config().disk_format();
        if self.config.check_filesystems {
            tracing::info!(?format, "fsck started");
            if let Err(error) = fs.fsck().await {
                self.report_corruption(format.as_str(), &error);
                if self.config.format_data_on_corruption {
                    tracing::info!("Reformatting filesystem, expect data loss...");
                    return Ok(ServeFilesystemStatus::FormatRequired);
                } else {
                    tracing::error!(?format, "format on corruption is disabled, not continuing");
                    return Err(error);
                }
            } else {
                tracing::info!(?format, "fsck completed OK");
            }
        }

        // Wrap the serving in an async block so we can catch all errors.
        let serve_fut = async {
            match format {
                DiskFormat::Fxfs => {
                    let mut serving_multi_vol_fs = fs.serve_multi_volume().await?;
                    match fxfs::unlock_data_volume(&mut serving_multi_vol_fs, &self.config).await? {
                        Some((crypt_service, volume_name, _)) => {
                            Ok(ServeFilesystemStatus::Serving(Filesystem::ServingMultiVolume(
                                crypt_service,
                                serving_multi_vol_fs,
                                volume_name,
                            )))
                        }
                        // If unlocking returns none, the keybag got deleted by something.
                        None => {
                            tracing::warn!(
                                "keybag not found. Perhaps the keys were shredded? \
                                Reformatting the data volume."
                            );
                            Ok(ServeFilesystemStatus::FormatRequired)
                        }
                    }
                }
                _ => Ok(ServeFilesystemStatus::Serving(Filesystem::Serving(fs.serve().await?))),
            }
        };
        match serve_fut.await {
            Ok(fs) => Ok(fs),
            Err(error) => {
                self.report_corruption(format.as_str(), &error);
                if self.config.format_data_on_corruption {
                    tracing::info!("Reformatting filesystem, expect data loss...");
                    Ok(ServeFilesystemStatus::FormatRequired)
                } else {
                    tracing::error!(?format, "format on corruption is disabled, not continuing");
                    Err(error)
                }
            }
        }
    }

    // Destroy all non-blob fvm partitions and reallocate only the data partition. Called on the
    // reformatting codepath. Takes the topological path of an fvm device with the fvm driver
    // bound.
    async fn reset_fvm_partition(
        &self,
        fvm_topo_path: &str,
        ignore_paths: &mut HashSet<String>,
    ) -> Result<Box<dyn Device>, Error> {
        tracing::info!(path = fvm_topo_path, "Resetting fvm partitions");
        let fvm_directory_proxy =
            fuchsia_fs::directory::open_in_namespace(&fvm_topo_path, fio::PERM_READABLE)?;
        let fvm_volume_manager_proxy =
            connect_to_protocol_at_path::<VolumeManagerMarker>(&fvm_topo_path)
                .context("Failed to connect to the fvm VolumeManagerProxy")?;

        // **NOTE**: We must call VolumeManager::GetInfo() to ensure all partitions are visible when
        // we enumerate them below. See https://fxbug.dev/42077585 for more information.
        zx::ok(fvm_volume_manager_proxy.get_info().await.context("transport error on get_info")?.0)
            .context("get_info failed")?;

        let dir_entries = fuchsia_fs::directory::readdir(&fvm_directory_proxy).await?;
        for entry in dir_entries {
            // Destroy all fvm partitions aside from blobfs
            if !entry.name.contains("blobfs") && !entry.name.contains("device") {
                let entry_volume_proxy = recursive_wait_and_open::<VolumeMarker>(
                    &fvm_directory_proxy,
                    &format!("{}/block", entry.name),
                )
                .await
                .with_context(|| format!("Failed to open partition {}", entry.name))?;
                ignore_paths.insert(format!("{fvm_topo_path}/{}/block", entry.name));
                let status = entry_volume_proxy
                    .destroy()
                    .await
                    .with_context(|| format!("Failed to destroy partition {}", entry.name))?;
                zx::Status::ok(status).context("destroy() returned an error")?;
                tracing::info!(partition = %entry.name, "Destroyed partition");
            }
        }

        // Recreate the data partition
        let data_partition_controller = fvm_allocate_partition(
            &fvm_volume_manager_proxy,
            DATA_TYPE_GUID,
            *Uuid::new_v4().as_bytes(),
            DATA_PARTITION_LABEL,
            0,
            INITIAL_SLICE_COUNT,
        )
        .await
        .context("Failed to allocate fvm data partition")?;

        let device_path = data_partition_controller
            .get_topological_path()
            .await?
            .map_err(zx::Status::from_raw)?;

        ignore_paths.insert(device_path.to_string());
        Ok(Box::new(BlockDevice::from_proxy(data_partition_controller, device_path).await?))
    }

    /// Starts serving Fxblob without opening any volumes.
    pub async fn serve_fxblob(
        &self,
        block_connector: Box<dyn BlockConnector>,
    ) -> Result<ServingMultiVolumeFilesystem, Error> {
        let mut fs = fs_management::filesystem::Filesystem::from_boxed_config(
            block_connector,
            Box::new(Fxfs {
                component_type: ComponentType::StaticChild,
                startup_profiling_seconds: Some(60),
                ..Default::default()
            }),
        );
        if self.config.check_filesystems {
            tracing::info!("fsck started for fxblob");
            if let Err(error) = fs.fsck().await {
                self.report_corruption("fxfs", &error);
                return Err(error);
            } else {
                tracing::info!("fsck completed OK for fxblob");
            }
        }
        fs.serve_multi_volume().await
    }

    /// Starts serving Fvm without opening any volumes.
    pub async fn serve_fvm(
        &self,
        device: &mut dyn Device,
    ) -> Result<ServingMultiVolumeFilesystem, Error> {
        let mut fs = fs_management::filesystem::Filesystem::from_boxed_config(
            device.block_connector()?,
            Box::new(Fvm { component_type: ComponentType::StaticChild }),
        );
        fs.serve_multi_volume().await
    }

    pub async fn format_data<FSC: FSConfig>(
        &self,
        device: &mut dyn Device,
        config: FSC,
    ) -> Result<Filesystem, Error> {
        let fs = fs_management::filesystem::Filesystem::from_boxed_config(
            device.block_connector()?,
            Box::new(config),
        );
        self.format_data_from(device, fs).await
    }

    async fn format_data_from(
        &self,
        device: &mut dyn Device,
        mut fs: fs_management::filesystem::Filesystem,
    ) -> Result<Filesystem, Error> {
        let format = fs.config().disk_format();
        tracing::info!(path = device.path(), format = format.as_str(), "Formatting");
        match format {
            DiskFormat::Fxfs => {
                let target_bytes = self.config.data_max_bytes;
                tracing::info!(target_bytes, "Resizing data volume");
                let allocated_bytes =
                    device.resize(target_bytes).await.context("format volume resize")?;
                if allocated_bytes < target_bytes {
                    tracing::warn!(
                        target_bytes,
                        allocated_bytes,
                        "Allocated less space than desired"
                    );
                }
            }
            DiskFormat::F2fs => {
                let target_bytes =
                    std::cmp::max(self.config.data_max_bytes, DEFAULT_F2FS_MIN_BYTES);
                tracing::info!(target_bytes, "Resizing data volume");
                let allocated_bytes =
                    device.resize(target_bytes).await.context("format volume resize")?;
                if allocated_bytes < DEFAULT_F2FS_MIN_BYTES {
                    tracing::error!(
                        minimum_bytes = DEFAULT_F2FS_MIN_BYTES,
                        allocated_bytes,
                        "Not enough space for f2fs"
                    )
                }
                if allocated_bytes < target_bytes {
                    tracing::warn!(
                        target_bytes,
                        allocated_bytes,
                        "Allocated less space than desired"
                    );
                }
            }
            _ => (),
        }

        fs.format().await.context("formatting data partition")?;

        tracing::info!(path = device.path(), format = format.as_str(), "Serving");
        let filesystem = if let DiskFormat::Fxfs = format {
            let mut serving_multi_vol_fs =
                fs.serve_multi_volume().await.context("serving multi volume data partition")?;
            let (crypt_service, volume_name, _) =
                fxfs::init_data_volume(&mut serving_multi_vol_fs, &self.config)
                    .await
                    .context("initializing data volume encryption")?;
            Filesystem::ServingMultiVolume(crypt_service, serving_multi_vol_fs, volume_name)
        } else {
            Filesystem::Serving(fs.serve().await.context("serving single volume data partition")?)
        };

        Ok(filesystem)
    }

    fn report_corruption(&self, format: &str, error: &Error) {
        tracing::error!(format, ?error, "FILESYSTEM CORRUPTION DETECTED!");
        tracing::error!(
            "Please file a bug to the Storage component in http://fxbug.dev, including a \
            device snapshot collected with `ffx target snapshot` if possible.",
        );

        let report = fidl_fuchsia_feedback::CrashReport {
            program_name: Some(format.to_string()),
            crash_signature: Some(format!("fuchsia-{format}-corruption")),
            is_fatal: Some(false),
            ..Default::default()
        };

        fasync::Task::spawn(async move {
            let proxy = if let Ok(proxy) =
                connect_to_protocol::<fidl_fuchsia_feedback::CrashReporterMarker>()
            {
                proxy
            } else {
                tracing::error!("Failed to connect to crash report service");
                return;
            };
            if let Err(e) = proxy.file_report(report).await {
                tracing::error!(?e, "Failed to file crash report");
            }
        })
        .detach();

        // NOTE: If a corruption event has already been recorded, this will turn into a no-op, which
        // means we'd short count the number of corruption events.  Given that this should only
        // occur no more than once per-boot, this doesn't seem worth fixing.
        self.corruption_events.record_uint(format, 1);
    }
}
