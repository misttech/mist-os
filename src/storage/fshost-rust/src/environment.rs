// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        boot_args::BootArgs,
        crypt::{
            fxfs::{self, CryptService, UnlockResult},
            zxcrypt::{UnsealOutcome, ZxcryptDevice},
        },
        device::{
            constants::{DEFAULT_F2FS_MIN_BYTES, ZXCRYPT_DRIVER_PATH},
            Device,
        },
    },
    anyhow::{anyhow, Context, Error},
    async_trait::async_trait,
    either::Either,
    fidl::endpoints::{create_proxy, ClientEnd, ServerEnd},
    fidl_fuchsia_device::ControllerMarker,
    fidl_fuchsia_hardware_block_partition::Guid,
    fidl_fuchsia_hardware_block_volume::VolumeManagerMarker,
    fidl_fuchsia_io as fio,
    fs_management::{
        filesystem::{ServingMultiVolumeFilesystem, ServingSingleVolumeFilesystem, ServingVolume},
        format::DiskFormat,
        Blobfs, ComponentType, F2fs, FSConfig, Fxfs, Minfs,
    },
    fuchsia_async as fasync,
    fuchsia_component::client::{connect_to_protocol, connect_to_protocol_at_path},
    fuchsia_zircon as zx,
    std::sync::Arc,
};

/// Environment is a trait that performs actions when a device is matched.
/// Nb: matcher.rs depend on this interface being used in order to mock tests.
#[async_trait]
pub trait Environment: Send + Sync {
    /// Attaches the specified driver to the device.
    async fn attach_driver(
        &mut self,
        device: &mut dyn Device,
        driver_path: &str,
    ) -> Result<(), Error>;

    /// Calls ZxCryptDevice::unseal, but allows for mocking.
    async fn unseal_zxcrypt(&mut self, device: &mut dyn Device) -> Result<UnsealOutcome, Error>;

    /// Calls ZxcryptDevice::format(), but allows for mocking.
    async fn format_zxcrypt(&mut self, device: &mut dyn Device) -> Result<ZxcryptDevice, Error>;

    /// Mounts Blobfs on the given device.
    async fn mount_blobfs(&mut self, device: &mut dyn Device) -> Result<(), Error>;

    /// Mounts the data partition on the given device.
    async fn mount_data(&mut self, device: &mut dyn Device) -> Result<(), Error>;
}

// Before a filesystem is mounted, we queue requests.
pub enum Filesystem {
    Queue(Vec<ServerEnd<fio::DirectoryMarker>>),
    Serving(ServingSingleVolumeFilesystem),
    ServingMultiVolume(CryptService, ServingMultiVolumeFilesystem, String),
}

impl Filesystem {
    pub fn root(&mut self) -> Result<fio::DirectoryProxy, Error> {
        let (proxy, server) = create_proxy::<fio::DirectoryMarker>()?;
        match self {
            Filesystem::Queue(queue) => queue.push(server),
            Filesystem::Serving(fs) => {
                fs.root().clone(fio::OpenFlags::CLONE_SAME_RIGHTS, server.into_channel().into())?
            }
            Filesystem::ServingMultiVolume(_, fs, data_volume_name) => fs
                .volume(&data_volume_name)
                .ok_or(anyhow!("data volume {} not found", data_volume_name))?
                .root()
                .clone(fio::OpenFlags::CLONE_SAME_RIGHTS, server.into_channel().into())?,
        }
        Ok(proxy)
    }

    fn volume(&mut self, volume_name: &str) -> Result<Option<&mut ServingVolume>, Error> {
        match self {
            Filesystem::ServingMultiVolume(_, fs, _) => Ok(fs.volume_mut(&volume_name)),
            _ => Ok(None),
        }
    }

    fn queue(&mut self) -> Option<&mut Vec<ServerEnd<fio::DirectoryMarker>>> {
        match self {
            Filesystem::Queue(queue) => Some(queue),
            _ => None,
        }
    }

    pub async fn shutdown(self) -> Result<(), Error> {
        match self {
            Filesystem::Queue(_) => Ok(()),
            Filesystem::Serving(fs) => fs.shutdown().await.context("shutdown failed"),
            Filesystem::ServingMultiVolume(_, fs, _) => {
                fs.shutdown().await.context("shutdown failed")
            }
        }
    }
}

/// Implements the Environment trait and keeps track of mounted filesystems.
pub struct FshostEnvironment {
    config: Arc<fshost_config::Config>,
    ramdisk_prefix: Option<String>,
    blobfs: Filesystem,
    data: Filesystem,
    launcher: Arc<FilesystemLauncher>,
}

impl FshostEnvironment {
    pub fn new(
        config: Arc<fshost_config::Config>,
        boot_args: BootArgs,
        ramdisk_prefix: Option<String>,
    ) -> Self {
        Self {
            config: config.clone(),
            ramdisk_prefix: ramdisk_prefix.clone(),
            blobfs: Filesystem::Queue(Vec::new()),
            data: Filesystem::Queue(Vec::new()),
            launcher: Arc::new(FilesystemLauncher { config, boot_args, ramdisk_prefix }),
        }
    }

    /// Returns a proxy for the root of the Blobfs filesystem.  This can be called before Blobfs is
    /// mounted and it will get routed once Blobfs is mounted.
    pub fn blobfs_root(&mut self) -> Result<fio::DirectoryProxy, Error> {
        self.blobfs.root()
    }

    /// Returns a proxy for the root of the data filesystem.  This can be called before "/data"
    /// is mounted and it will get routed once the data partition is mounted.
    pub fn data_root(&mut self) -> Result<fio::DirectoryProxy, Error> {
        self.data.root()
    }

    pub fn launcher(&self) -> Arc<FilesystemLauncher> {
        self.launcher.clone()
    }
}

#[async_trait]
impl Environment for FshostEnvironment {
    async fn attach_driver(
        &mut self,
        device: &mut dyn Device,
        driver_path: &str,
    ) -> Result<(), Error> {
        self.launcher.attach_driver(device, driver_path).await
    }

    async fn unseal_zxcrypt(&mut self, device: &mut dyn Device) -> Result<UnsealOutcome, Error> {
        ZxcryptDevice::unseal(device).await
    }

    async fn format_zxcrypt(&mut self, device: &mut dyn Device) -> Result<ZxcryptDevice, Error> {
        ZxcryptDevice::format(device).await
    }

    async fn mount_blobfs(&mut self, device: &mut dyn Device) -> Result<(), Error> {
        let queue = self.blobfs.queue().ok_or(anyhow!("blobfs already mounted"))?;

        let mut fs = self.launcher.serve_blobfs(device).await?;

        let root_dir = fs.root()?;
        for server in queue.drain(..) {
            root_dir.clone(fio::OpenFlags::CLONE_SAME_RIGHTS, server.into_channel().into())?;
        }
        self.blobfs = fs;
        Ok(())
    }

    async fn mount_data(&mut self, device: &mut dyn Device) -> Result<(), Error> {
        let _ = self.data.queue().ok_or_else(|| anyhow!("data partition already mounted"))?;

        // Default to minfs if we don't match expected filesystems.
        let format: DiskFormat = match self.config.data_filesystem_format.as_str().into() {
            DiskFormat::Fxfs => DiskFormat::Fxfs,
            DiskFormat::F2fs => DiskFormat::F2fs,
            _ => DiskFormat::Minfs,
        };

        // Set the max partition size for data
        if self
            .ramdisk_prefix
            .as_ref()
            .map_or(true, |prefix| !device.topological_path().starts_with(prefix))
        {
            if let Err(e) = set_partition_max_size(device, self.config.data_max_bytes).await {
                tracing::warn!(?e, "Failed to set max partition size for data");
            };
        }

        // Potentially bind zxcrypt before serving data.
        let mut zxcrypt_device;
        let device = match format {
            // Fxfs never has zxcrypt underneath
            DiskFormat::Fxfs => device,
            // Skip zxcrypt in these configurations.
            _ if self.config.no_zxcrypt => device,
            _ if self.config.fvm_ramdisk => device,
            // Otherwise, we need to bind a zxcrypt device first.
            _ => {
                self.attach_driver(device, ZXCRYPT_DRIVER_PATH).await?;
                zxcrypt_device = Box::new(match self.unseal_zxcrypt(device).await? {
                    UnsealOutcome::Unsealed(device) => device,
                    UnsealOutcome::FormatRequired => {
                        tracing::warn!("failed to unseal zxcrypt, reformatting");
                        self.format_zxcrypt(device).await?
                    }
                });
                zxcrypt_device.as_mut()
            }
        };

        let mut filesystem = match format {
            DiskFormat::Fxfs => {
                let config =
                    Fxfs { component_type: ComponentType::StaticChild, ..Default::default() };
                self.launcher.serve_data(device, config).await?
            }
            DiskFormat::F2fs => {
                let config =
                    F2fs { component_type: ComponentType::StaticChild, ..Default::default() };
                self.launcher.serve_data(device, config).await?
            }
            DiskFormat::Minfs => {
                let config =
                    Minfs { component_type: ComponentType::StaticChild, ..Default::default() };
                self.launcher.serve_data(device, config).await?
            }
            _ => unreachable!(),
        };

        // TODO(fxbug.dev/122966): shred_volume relies on the unencrypted volume being bound in the
        // namespace. This should be reevaluated when keybag takes a proxy, but for now this is the
        // fastest fix.
        if format == DiskFormat::Fxfs {
            // If the unencrypted volume doesn't exist, assume we are using legacy crypto, in which
            // case we skip this step.
            let _: Option<()> = filesystem.volume("unencrypted")?.map(|volume| {
                volume.bind_to_path("/main_fxfs_unencrypted_volume").unwrap_or_else(|error| {
                    tracing::warn!(?error, "failed to bind unencrypted volume to namespace")
                })
            });
        }

        let queue = self.data.queue().unwrap();
        let root_dir = filesystem.root()?;
        for server in queue.drain(..) {
            root_dir.clone(fio::OpenFlags::CLONE_SAME_RIGHTS, server.into_channel().into())?;
        }
        self.data = filesystem;
        Ok(())
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
        // TODO(https://fxbug.dev/112484): this relies on multiplexing.
        let controller: ClientEnd<ControllerMarker> = device.client_end()?.into_channel().into();
        let controller = controller.into_proxy()?;
        controller.bind(driver_path).await?.map_err(zx::Status::from_raw)?;
        Ok(())
    }

    pub fn get_blobfs_config(&self) -> Blobfs {
        Blobfs {
            write_compression_algorithm: self.boot_args.blobfs_write_compression_algorithm(),
            cache_eviction_policy_override: self.boot_args.blobfs_eviction_policy(),
            sandbox_decompression: self.config.sandbox_decompression,
            ..Default::default()
        }
    }

    pub async fn serve_blobfs(&self, device: &mut dyn Device) -> Result<Filesystem, Error> {
        tracing::info!(path = %device.path(), "Mounting /blob");

        // Setting max partition size for blobfs
        if self
            .ramdisk_prefix
            .as_ref()
            .map_or(true, |prefix| !device.topological_path().starts_with(prefix))
        {
            if let Err(e) = set_partition_max_size(device, self.config.blobfs_max_bytes).await {
                tracing::warn!("Failed to set max partition size for blobfs: {:?}", e);
            };
        }

        let config = Blobfs {
            component_type: fs_management::ComponentType::StaticChild,
            ..self.get_blobfs_config()
        };
        let block = device.client_end()?;
        let fs = fs_management::filesystem::Filesystem::from_block_device(block, config)
            .context("making filesystem instance")?
            .serve()
            .await
            .context("serving blobfs")?;

        Ok(Filesystem::Serving(fs))
    }

    pub async fn serve_data<FSC: FSConfig>(
        &self,
        device: &mut dyn Device,
        config: FSC,
    ) -> Result<Filesystem, Error> {
        let block = device.client_end()?;
        let fs = fs_management::filesystem::Filesystem::from_block_device(block, config)?;
        self.serve_data_from(device, fs).await
    }

    // NB: keep these larger functions monomorphized, otherwise they cause significant code size
    // increases.
    async fn serve_data_from(
        &self,
        device: &mut dyn Device,
        mut fs: fs_management::filesystem::Filesystem,
    ) -> Result<Filesystem, Error> {
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
            return self.format_data(device, &mut fs).await;
        }

        if self.config.check_filesystems {
            tracing::info!(?format, "fsck started");
            if let Err(error) = fs.fsck().await {
                self.report_corruption(format, &error);
                if self.config.format_data_on_corruption {
                    tracing::info!("Reformatting filesystem, expect data loss...");
                    return self.format_data(device, &mut fs).await;
                } else {
                    tracing::error!(?format, "format on corruption is disabled, not continuing");
                    return Err(error);
                }
            } else {
                tracing::info!(?format, "fsck completed OK");
            }
        }

        // Wrap the serving in an async block so we can use ?.
        let serve_fut = async {
            match format {
                DiskFormat::Fxfs => {
                    let mut serving_fs = fs.serve_multi_volume().await?;
                    match fxfs::unlock_data_volume(&mut serving_fs, &self.config).await? {
                        UnlockResult::Ok((crypt_service, volume_name, _)) => Ok(Either::Left(
                            Filesystem::ServingMultiVolume(crypt_service, serving_fs, volume_name),
                        )),
                        UnlockResult::Reset => Ok(Either::Right(())),
                    }
                }
                _ => Ok(Either::Left(Filesystem::Serving(fs.serve().await?))),
            }
        };
        match serve_fut.await {
            Ok(Either::Left(fs)) => Ok(fs),
            Ok(Either::Right(())) => {
                tracing::info!("Detected marker file, shredding volume. Expect data loss...");
                self.format_data(device, &mut fs).await
            }
            Err(error) => {
                self.report_corruption(format, &error);
                if self.config.format_data_on_corruption {
                    tracing::info!("Reformatting filesystem, expect data loss...");
                    self.format_data(device, &mut fs).await
                } else {
                    tracing::error!(?format, "format on corruption is disabled, not continuing");
                    Err(error)
                }
            }
        }
    }

    async fn format_data(
        &self,
        device: &mut dyn Device,
        fs: &mut fs_management::filesystem::Filesystem,
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
        let serving_fs = if let DiskFormat::Fxfs = format {
            let mut serving_fs =
                fs.serve_multi_volume().await.context("serving multi volume data partition")?;
            let (crypt_service, volume_name, _) =
                fxfs::init_data_volume(&mut serving_fs, &self.config)
                    .await
                    .context("initializing data volume encryption")?;
            Filesystem::ServingMultiVolume(crypt_service, serving_fs, volume_name)
        } else {
            Filesystem::Serving(fs.serve().await.context("serving single volume data partition")?)
        };

        Ok(serving_fs)
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
                ..fidl_fuchsia_feedback::CrashReport::EMPTY
            };
            if let Err(e) = proxy.file_report(report).await {
                tracing::error!(?e, "Failed to file crash report");
            }
        })
        .detach();
    }
}

async fn set_partition_max_size(device: &mut dyn Device, max_byte_size: u64) -> Result<(), Error> {
    if max_byte_size == 0 {
        return Ok(());
    }

    let index =
        device.topological_path().rfind("/fvm").ok_or(anyhow!("fvm is not in the device path"))?;
    // The 4 is from the 4 characters in "/fvm"
    let fvm_path = &device.topological_path()[..index + 4];

    let fvm_proxy = connect_to_protocol_at_path::<VolumeManagerMarker>(&fvm_path)
        .context("Failed to connect to fvm volume manager")?;
    let (status, info) = fvm_proxy.get_info().await.context("Transport error in get_info call")?;
    zx::Status::ok(status).context("get_info call failed")?;
    let info = info.ok_or(anyhow!("Expected info"))?;
    let slice_size = info.slice_size;
    let max_slice_count = max_byte_size / slice_size;
    let mut instance_guid =
        Guid { value: *device.partition_instance().await.context("Expected partition instance")? };
    let status = fvm_proxy
        .set_partition_limit(&mut instance_guid, max_slice_count)
        .await
        .context("Transport error on set_partition_limit")?;
    zx::Status::ok(status).context("set_partition_limit failed")?;
    Ok(())
}
