// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        boot_args::BootArgs,
        copier::recursive_copy,
        crypt::{
            fxfs::{self, CryptService},
            get_policy,
            zxcrypt::{UnsealOutcome, ZxcryptDevice},
            Policy,
        },
        device::{
            constants::{
                BLOBFS_PARTITION_LABEL, BLOBFS_TYPE_GUID, DATA_PARTITION_LABEL, DATA_TYPE_GUID,
                DEFAULT_F2FS_MIN_BYTES, FVM_DRIVER_PATH, LEGACY_DATA_PARTITION_LABEL,
                ZXCRYPT_DRIVER_PATH,
            },
            BlockDevice, Device,
        },
        inspect::register_migration_status,
    },
    anyhow::{anyhow, bail, Context, Error},
    async_trait::async_trait,
    device_watcher::{recursive_wait, recursive_wait_and_open},
    fidl::endpoints::{create_proxy, ServerEnd},
    fidl_fuchsia_fxfs::MountOptions,
    fidl_fuchsia_hardware_block_partition::Guid,
    fidl_fuchsia_hardware_block_volume::{VolumeManagerMarker, VolumeMarker},
    fidl_fuchsia_io as fio,
    fs_management::{
        filesystem::{ServingMultiVolumeFilesystem, ServingSingleVolumeFilesystem, ServingVolume},
        format::DiskFormat,
        partition::fvm_allocate_partition,
        Blobfs, ComponentType, F2fs, FSConfig, Fxfs, Minfs,
    },
    fuchsia_async as fasync,
    fuchsia_component::client::{connect_to_protocol, connect_to_protocol_at_path},
    fuchsia_zircon as zx,
    futures::lock::Mutex,
    std::{collections::HashSet, sync::Arc},
    uuid::Uuid,
};

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

    /// Binds the fvm driver and returns a list of the names of the child partitions.
    async fn bind_and_enumerate_fvm(
        &mut self,
        device: &mut dyn Device,
    ) -> Result<Vec<String>, Error>;

    /// Creates a static instance of Fxfs on `device` and calls serve_multi_volume(). Only creates
    /// the overall Fxfs instance. Mount_blob_volume and mount_data_volume still need to be called.
    async fn mount_fxblob(&mut self, device: &mut dyn Device) -> Result<(), Error>;

    /// Mounts Fxblob's blob volume on the given device.
    async fn mount_blob_volume(&mut self) -> Result<(), Error>;

    /// Mounts Fxblob's data volume on the given device.
    async fn mount_data_volume(&mut self) -> Result<(), Error>;

    /// Called after the fvm driver is bound. Waits for the block driver to bind itself to the
    /// blobfs partition before creating a blobfs BlockDevice, which it passes into mount_blobfs().
    async fn mount_blobfs_on(&mut self, blobfs_partition_name: &str) -> Result<(), Error>;

    /// Called after the fvm driver is bound. Waits for the block driver to bind itself to the
    /// data partition before creating a data BlockDevice, which it then passes into launch_data().
    /// Calls bind_data() on the mounted filesystem.
    async fn mount_data_on(&mut self, data_partition_name: &str) -> Result<(), Error>;

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
}

// Before a filesystem is mounted, we queue requests.
pub enum Filesystem {
    Queue(Vec<ServerEnd<fio::DirectoryMarker>>),
    Serving(ServingSingleVolumeFilesystem),
    ServingMultiVolume(CryptService, ServingMultiVolumeFilesystem, String),
    ServingVolumeInFxblob(Option<CryptService>, String),
}

impl Filesystem {
    fn is_serving(&self) -> bool {
        if let Self::Queue(_) = self {
            false
        } else {
            true
        }
    }

    pub fn exposed_dir(
        &mut self,
        serving_fs: Option<&mut ServingMultiVolumeFilesystem>,
    ) -> Result<fio::DirectoryProxy, Error> {
        let (proxy, server) = create_proxy::<fio::DirectoryMarker>()?;
        match self {
            Filesystem::Queue(queue) => queue.push(server),
            Filesystem::Serving(fs) => fs
                .exposed_dir()
                .clone(fio::OpenFlags::CLONE_SAME_RIGHTS, server.into_channel().into())?,
            Filesystem::ServingMultiVolume(_, fs, data_volume_name) => fs
                .volume(&data_volume_name)
                .ok_or(anyhow!("data volume {} not found", data_volume_name))?
                .exposed_dir()
                .clone(fio::OpenFlags::CLONE_SAME_RIGHTS, server.into_channel().into())?,
            Filesystem::ServingVolumeInFxblob(_, data_volume_name) => serving_fs
                .unwrap()
                .volume(&data_volume_name)
                .ok_or(anyhow!("data volume {} not found", data_volume_name))?
                .exposed_dir()
                .clone(fio::OpenFlags::CLONE_SAME_RIGHTS, server.into_channel().into())?,
        }
        Ok(proxy)
    }

    pub fn root(
        &mut self,
        serving_fs: Option<&mut ServingMultiVolumeFilesystem>,
    ) -> Result<fio::DirectoryProxy, Error> {
        let root = fuchsia_fs::directory::open_directory_no_describe(
            &self.exposed_dir(serving_fs).context("failed to get exposed dir")?,
            "root",
            fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::POSIX_EXECUTABLE
                | fio::OpenFlags::POSIX_WRITABLE,
        )
        .context("failed to open the root directory")?;
        Ok(root)
    }

    fn volume(&mut self, volume_name: &str) -> Option<&mut ServingVolume> {
        match self {
            Filesystem::ServingMultiVolume(_, fs, _) => fs.volume_mut(&volume_name),
            Filesystem::ServingVolumeInFxblob(..) => unreachable!(),
            _ => None,
        }
    }

    fn queue(&mut self) -> Option<&mut Vec<ServerEnd<fio::DirectoryMarker>>> {
        match self {
            Filesystem::Queue(queue) => Some(queue),
            _ => None,
        }
    }

    pub async fn shutdown(
        self,
        serving_fs: Option<&mut ServingMultiVolumeFilesystem>,
    ) -> Result<(), Error> {
        match self {
            Filesystem::Queue(_) => Ok(()),
            Filesystem::Serving(fs) => fs.shutdown().await.context("shutdown failed"),
            Filesystem::ServingMultiVolume(_, fs, _) => {
                fs.shutdown().await.context("shutdown failed")
            }
            Filesystem::ServingVolumeInFxblob(_, volume_name) => {
                serving_fs.unwrap().shutdown_volume(&volume_name).await.context("shutdown failed")
            }
        }
    }
}

/// Implements the Environment trait and keeps track of mounted filesystems.
pub struct FshostEnvironment {
    config: Arc<fshost_config::Config>,
    // `fxblob` is set inside mount_fxblob() and represents the overall Fxfs instance which
    // contains both a data and blob volume.
    fxblob: Option<ServingMultiVolumeFilesystem>,
    blobfs: Filesystem,
    data: Filesystem,
    fvm: Option<(/*topological_path*/ String, /*device_directory*/ fio::DirectoryProxy)>,
    launcher: Arc<FilesystemLauncher>,
    /// This lock can be taken and device.path() added to the vector to have them
    /// ignored the next time they appear to the Watcher/Matcher code.
    matcher_lock: Arc<Mutex<HashSet<String>>>,
    inspector: fuchsia_inspect::Inspector,
}

impl FshostEnvironment {
    pub fn new(
        config: Arc<fshost_config::Config>,
        boot_args: BootArgs,
        ramdisk_prefix: Option<String>,
        matcher_lock: Arc<Mutex<HashSet<String>>>,
        inspector: fuchsia_inspect::Inspector,
    ) -> Self {
        Self {
            config: config.clone(),
            fxblob: None,
            blobfs: Filesystem::Queue(Vec::new()),
            data: Filesystem::Queue(Vec::new()),
            fvm: None,
            launcher: Arc::new(FilesystemLauncher { config, boot_args, ramdisk_prefix }),
            matcher_lock,
            inspector,
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
        self.blobfs.exposed_dir(self.fxblob.as_mut())
    }

    /// Returns a proxy for the exposed dir of the data filesystem.  This can be called before
    /// "/data" is mounted and it will get routed once the data partition is mounted.
    pub fn data_exposed_dir(&mut self) -> Result<fio::DirectoryProxy, Error> {
        self.data.exposed_dir(self.fxblob.as_mut())
    }

    /// Returns a proxy for the root of the data filesystem.  This can be called before "/data" is
    /// mounted and it will get routed once the data partition is mounted.
    pub fn data_root(&mut self) -> Result<fio::DirectoryProxy, Error> {
        self.data.root(self.fxblob.as_mut())
    }

    pub fn launcher(&self) -> Arc<FilesystemLauncher> {
        self.launcher.clone()
    }

    /// Set the max partition size for data
    async fn apply_data_partition_limits(&self, device: &mut dyn Device) {
        if !self.launcher.is_ramdisk_device(device) {
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
            || self.launcher.requires_zxcrypt(format, device)
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

        tracing::info!(path = device.path(), format = format.as_str(), "Formatting");

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
            fio::OpenFlags::RIGHT_READABLE,
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
                &device.fvm_path().ok_or(anyhow!("Not an fvm device"))?,
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
        let queue = self.blobfs.queue().ok_or(anyhow!("blobfs already mounted"))?;

        let mut fs = self.launcher.serve_blobfs(device).await?;

        let exposed_dir = fs.exposed_dir(None)?;
        for server in queue.drain(..) {
            exposed_dir.clone(fio::OpenFlags::CLONE_SAME_RIGHTS, server.into_channel().into())?;
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
            tracing::info!(format = format.as_str(), "launching detected format");
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
            || self.launcher.requires_zxcrypt(format, device)
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

    async fn bind_and_enumerate_fvm(
        &mut self,
        device: &mut dyn Device,
    ) -> Result<Vec<String>, Error> {
        // Attach the FVM driver and connect to the VolumeManager.
        self.attach_driver(device, FVM_DRIVER_PATH).await?;
        let fvm_dir = fuchsia_fs::directory::open_in_namespace(
            &device.topological_path(),
            fuchsia_fs::OpenFlags::RIGHT_READABLE,
        )?;
        let fvm_volume_manager_proxy =
            recursive_wait_and_open::<VolumeManagerMarker>(&fvm_dir, "/fvm")
                .await
                .context("failed to connect to the VolumeManager")?;
        // Call VolumeManager::GetInfo in order to ensure all partition entries are visible.
        // TODO(https://fxbug.dev/126961): Right now, we rely on get_info() completing to ensure
        // that fvm child partitions are visible in devfs. This should be revised when DF supports
        // another way of safely enumerating child partitions.
        zx::ok(fvm_volume_manager_proxy.get_info().await.context("transport error on get_info")?.0)
            .context("get_info failed")?;

        let fvm_topo_path = format!("{}/fvm", device.topological_path());
        let fvm_dir = fuchsia_fs::directory::open_in_namespace(
            &fvm_topo_path,
            fuchsia_fs::OpenFlags::RIGHT_READABLE,
        )?;
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
        let serving_fs = self.launcher.serve_fxblob(device).await?;
        self.fxblob = Some(serving_fs);
        Ok(())
    }

    async fn mount_blob_volume(&mut self) -> Result<(), Error> {
        let _ = self.blobfs.queue().ok_or_else(|| anyhow!("blobfs partition already mounted"))?;

        let multi_vol_fs =
            self.fxblob.as_mut().ok_or_else(|| anyhow!("ServingMultiVolumeFilesystem is None"))?;
        let () = multi_vol_fs
            .check_volume("blob", None)
            .await
            .context("Failed to verify the blob volume")?;
        let blobfs = multi_vol_fs
            .open_volume("blob", MountOptions { crypt: None, as_blob: true })
            .await
            .context("Failed to open the blob volume")?;
        let exposed_dir = blobfs.exposed_dir();
        let queue = self.blobfs.queue().ok_or(anyhow!("blobfs already mounted"))?;
        for server in queue.drain(..) {
            exposed_dir.clone(fio::OpenFlags::CLONE_SAME_RIGHTS, server.into_channel().into())?;
        }
        self.blobfs = Filesystem::ServingVolumeInFxblob(None, "blob".to_string());
        Ok(())
    }

    async fn mount_data_volume(&mut self) -> Result<(), Error> {
        let _ = self.data.queue().ok_or_else(|| anyhow!("data partition already mounted"))?;

        let multi_vol_fs =
            self.fxblob.as_mut().ok_or_else(|| anyhow!("ServingMultiVolumeFilesystem is None"))?;
        let mut filesystem = self.launcher.serve_data_fxblob(multi_vol_fs).await?;

        let queue = self.data.queue().unwrap();
        let exposed_dir = filesystem.exposed_dir(self.fxblob.as_mut())?;
        for server in queue.drain(..) {
            exposed_dir.clone(fio::OpenFlags::CLONE_SAME_RIGHTS, server.into_channel().into())?;
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

    async fn mount_data_on(&mut self, data_partition_name: &str) -> Result<(), Error> {
        let (fvm_topo_path, fvm_dir) = self.get_fvm()?;
        let data_topo_path = format!("{}/{data_partition_name}/block", fvm_topo_path);
        recursive_wait(fvm_dir, &format!("{data_partition_name}/block"))
            .await
            .context("failed to bind block driver to the data device")?;
        let mut device = BlockDevice::new(data_topo_path)
            .await
            .context("failed to create blobfs block device")?;
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
                self.format_data(&device.fvm_path().ok_or(anyhow!("Not an fvm device"))?).await?
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
            .context("reset fvm")?;
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
        for server in queue.drain(..) {
            exposed_dir.clone(fio::OpenFlags::CLONE_SAME_RIGHTS, server.into_channel().into())?;
        }
        self.data = filesystem;
        Ok(())
    }

    async fn shred_data(&mut self) -> Result<(), Error> {
        if self.config.data_filesystem_format != "fxfs" {
            return Err(anyhow!("Can't shred data; not fxfs"));
        }
        if !self.data.is_serving() {
            return Err(anyhow!("Can't shred data; not already mounted"));
        }
        // Erase the keybag.
        let unencrypted = if let Some(fxblob) = &mut self.fxblob {
            fxblob.volume_mut("unencrypted")
        } else {
            self.data.volume("unencrypted")
        }
        .context("Failed to find unencrypted volume")?;
        let dir = fuchsia_fs::directory::open_directory(
            unencrypted.root(),
            "keys",
            fio::OpenFlags::RIGHT_WRITABLE,
        )
        .await
        .context("Failed to open keys dir")?;
        dir.unlink("fxfs-data", &fio::UnlinkOptions::default())
            .await?
            .map_err(|e| anyhow!(zx::Status::from_raw(e)))
            .context("Failed to remove keybag")?;
        Ok(())
    }
}

#[derive(Debug)]
struct ReformatRequired(Error);
impl std::error::Error for ReformatRequired {}
impl std::fmt::Display for ReformatRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "{:?}", self)
    }
}

pub struct FilesystemLauncher {
    config: Arc<fshost_config::Config>,
    boot_args: BootArgs,
    ramdisk_prefix: Option<String>,
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

    /// This helper method returns true if the given device is a ramdisk.
    /// We want to enforce partition limits and use zxcrypt only on non-ramdisk devices.
    pub fn is_ramdisk_device(&self, device: &dyn Device) -> bool {
        self.ramdisk_prefix
            .as_ref()
            .map_or(false, |prefix| device.topological_path().starts_with(prefix))
    }

    pub fn requires_zxcrypt(&self, format: DiskFormat, device: &dyn Device) -> bool {
        match format {
            // Fxfs never has zxcrypt underneath
            DiskFormat::Fxfs => false,
            _ if self.config.no_zxcrypt => false,
            // No point using zxcrypt for ramdisk devices.
            _ if self.is_ramdisk_device(device) => false,
            _ => true,
        }
    }

    pub fn get_blobfs_config(&self) -> Blobfs {
        Blobfs {
            write_compression_algorithm: self.boot_args.blobfs_write_compression_algorithm(),
            cache_eviction_policy_override: self.boot_args.blobfs_eviction_policy(),
            allow_delivery_blobs: self.config.blobfs_allow_delivery_blobs,
            ..Default::default()
        }
    }

    pub async fn serve_blobfs(&self, device: &mut dyn Device) -> Result<Filesystem, Error> {
        tracing::info!(path = %device.path(), "Mounting /blob");

        // Setting max partition size for blobfs
        if !self.is_ramdisk_device(device) {
            if let Err(e) = device.set_partition_max_bytes(self.config.blobfs_max_bytes).await {
                tracing::warn!("Failed to set max partition size for blobfs: {:?}", e);
            };
        }

        let config = Blobfs {
            component_type: fs_management::ComponentType::StaticChild,
            ..self.get_blobfs_config()
        };
        let fs = fs_management::filesystem::Filesystem::new(device.reopen_controller()?, config)
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
        let fs = fs_management::filesystem::Filesystem::new(device.reopen_controller()?, config);
        self.serve_data_from(device, fs).await
    }

    // NB: keep these larger functions monomorphized, otherwise they cause significant code size
    // increases.
    async fn serve_data_from(
        &self,
        device: &mut dyn Device,
        mut fs: fs_management::filesystem::Filesystem,
    ) -> Result<ServeFilesystemStatus, Error> {
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

        if self.config.check_filesystems {
            tracing::info!(?format, "fsck started");
            if let Err(error) = fs.fsck().await {
                self.report_corruption(format, &error);
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
                    let (crypt_service, volume_name, _) =
                        fxfs::unlock_data_volume(&mut serving_multi_vol_fs, &self.config).await?;
                    Ok(Filesystem::ServingMultiVolume(
                        crypt_service,
                        serving_multi_vol_fs,
                        volume_name,
                    ))
                }
                _ => Ok(Filesystem::Serving(fs.serve().await?)),
            }
        };
        match serve_fut.await {
            Ok(fs) => Ok(ServeFilesystemStatus::Serving(fs)),
            Err(error) => {
                self.report_corruption(format, &error);
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
        let fvm_directory_proxy = fuchsia_fs::directory::open_in_namespace(
            &fvm_topo_path,
            fio::OpenFlags::RIGHT_READABLE,
        )?;
        let fvm_volume_manager_proxy =
            connect_to_protocol_at_path::<VolumeManagerMarker>(&fvm_topo_path)
                .context("Failed to connect to the fvm VolumeManagerProxy")?;

        let dir_entries = fuchsia_fs::directory::readdir(&fvm_directory_proxy).await?;
        for entry in dir_entries {
            // Destroy all fvm partitions aside from blobfs
            if !entry.name.contains("blobfs") && !entry.name.contains("device") {
                let entry_path = format!("{fvm_topo_path}/{}/block", entry.name);
                let entry_volume_proxy =
                    connect_to_protocol_at_path::<VolumeMarker>(&entry_path)
                        .context("Failed to connect to the partition VolumeProxy")?;
                ignore_paths.insert(entry_path.to_string());
                let status = entry_volume_proxy
                    .destroy()
                    .await
                    .context("Failed to destroy the data partition")?;
                zx::Status::ok(status).context("destroy() returned an error")?;
                tracing::info!(topological_path = %entry_path, "Destroyed partition");
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
        device: &mut dyn Device,
    ) -> Result<ServingMultiVolumeFilesystem, Error> {
        let mut fs = fs_management::filesystem::Filesystem::new(
            device.reopen_controller()?,
            Fxfs { component_type: ComponentType::StaticChild, ..Default::default() },
        );
        fs.serve_multi_volume().await
    }

    /// Serves the data volume from Fxblob, formatting any non-blob volumes as needed.
    pub async fn serve_data_fxblob(
        &self,
        serving_multi_vol_fs: &mut ServingMultiVolumeFilesystem,
    ) -> Result<Filesystem, Error> {
        let mut format_needed = false;
        tracing::info!(expected_format = "fxfs", "Mounting /data");

        // We expect the startup protocol to work with fxblob. There are no options for
        // reformatting the entire fxfs partition now that blobfs is one of the volumes.
        let volumes_dir = fuchsia_fs::directory::open_directory(
            serving_multi_vol_fs.exposed_dir(),
            "volumes",
            fuchsia_fs::OpenFlags::empty(),
        )
        .await
        .context("opening volumes directory")?;
        let volumes = fuchsia_fs::directory::readdir(&volumes_dir)
            .await
            .context("reading volumes directory")?;
        let needed =
            HashSet::from(["blob".to_string(), "data".to_string(), "unencrypted".to_string()]);
        let mut found = HashSet::new();
        for volume in volumes {
            found.insert(volume.name);
        }

        if found != needed {
            format_needed = true;
        }

        if format_needed {
            Ok(self.format_data_in_fxblob(serving_multi_vol_fs).await?)
        } else {
            match fxfs::unlock_data_volume(serving_multi_vol_fs, &self.config).await {
                Ok((crypt_service, volume_name, _)) => {
                    Ok(Filesystem::ServingVolumeInFxblob(Some(crypt_service), volume_name))
                }
                Err(error) => {
                    self.report_corruption(DiskFormat::Fxfs, &error);
                    tracing::error!(
                        ?error,
                        "unlock_data_volume failed. Reformatting the data and unencrypted volumes."
                    );
                    Ok(self.format_data_in_fxblob(serving_multi_vol_fs).await?)
                }
            }
        }
    }

    async fn format_data_in_fxblob(
        &self,
        serving_multi_vol_fs: &mut ServingMultiVolumeFilesystem,
    ) -> Result<Filesystem, Error> {
        // Reset fxfs volumes before reinitializing.
        let volumes_dir = fuchsia_fs::directory::open_directory_no_describe(
            serving_multi_vol_fs.exposed_dir(),
            "volumes",
            fuchsia_fs::OpenFlags::empty(),
        )?;
        let volumes = fuchsia_fs::directory::readdir(&volumes_dir).await?;
        for volume in volumes {
            if volume.name != "blob".to_string() {
                // Unmount mounted non-blob volumes.
                if serving_multi_vol_fs.volume(&volume.name).is_some() {
                    serving_multi_vol_fs.shutdown_volume(&volume.name).await?;
                }
                // Remove any non-blob volumes.
                serving_multi_vol_fs
                    .remove_volume(&volume.name)
                    .await
                    .context(format!("failed to remove volume: {:?}", volume.name))?;
            }
        }

        let (crypt_service, volume_name, _) =
            fxfs::init_data_volume(serving_multi_vol_fs, &self.config)
                .await
                .context("initializing data volume encryption")?;
        let filesystem = Filesystem::ServingVolumeInFxblob(Some(crypt_service), volume_name);

        Ok(filesystem)
    }

    pub async fn format_data<FSC: FSConfig>(
        &self,
        device: &mut dyn Device,
        config: FSC,
    ) -> Result<Filesystem, Error> {
        let fs = fs_management::filesystem::Filesystem::new(device.reopen_controller()?, config);
        self.format_data_from(device, fs).await
    }

    async fn format_data_from(
        &self,
        device: &mut dyn Device,
        mut fs: fs_management::filesystem::Filesystem,
    ) -> Result<Filesystem, Error> {
        let format = fs.config().disk_format();
        tracing::info!(?format, "Formatting");
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

    fn report_corruption(&self, format: DiskFormat, error: &Error) {
        tracing::error!(?format, ?error, "FILESYSTEM CORRUPTION DETECTED!");
        tracing::error!(
            "Please file a bug to the Storage component in http://fxbug.dev, including a \
            device snapshot collected with `ffx target snapshot` if possible.",
        );

        fasync::Task::spawn(async move {
            let proxy = if let Ok(proxy) =
                connect_to_protocol::<fidl_fuchsia_feedback::CrashReporterMarker>()
            {
                proxy
            } else {
                tracing::error!("Failed to connect to crash report service");
                return;
            };
            let report = fidl_fuchsia_feedback::CrashReport {
                program_name: Some(format.as_str().to_owned()),
                crash_signature: Some(format!("fuchsia-{}-corruption", format.as_str())),
                is_fatal: Some(false),
                ..Default::default()
            };
            if let Err(e) = proxy.file_report(report).await {
                tracing::error!(?e, "Failed to file crash report");
            }
        })
        .detach();
    }
}
