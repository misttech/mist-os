// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::crypt::zxcrypt::{UnsealOutcome, ZxcryptDevice};
use crate::device::constants::{
    self, BLOB_VOLUME_LABEL, DATA_PARTITION_LABEL, LEGACY_DATA_PARTITION_LABEL,
    UNENCRYPTED_VOLUME_LABEL, ZXCRYPT_DRIVER_PATH,
};
use crate::device::{BlockDevice, Device, DeviceTag};
use crate::environment::{Environment, FilesystemLauncher, ServeFilesystemStatus};
use crate::{debug_log, fxblob};
use anyhow::{anyhow, Context, Error};
use device_watcher::recursive_wait_and_open;
use fidl::endpoints::{Proxy, RequestStream, ServerEnd};
use fidl_fuchsia_device::{ControllerMarker, ControllerProxy};
use fidl_fuchsia_hardware_block::{BlockMarker, BlockProxy};
use fidl_fuchsia_hardware_block_volume::VolumeManagerMarker;
use fidl_fuchsia_io::{self as fio, DirectoryMarker};
use fidl_fuchsia_process_lifecycle::{LifecycleRequest, LifecycleRequestStream};
use fs_management::format::{detect_disk_format, DiskFormat};
use fs_management::partition::{
    find_partition, fvm_allocate_partition, partition_matches_with_proxy, PartitionMatcher,
};
use fs_management::{filesystem, Blobfs, F2fs, Fxfs, Minfs};
use fuchsia_async::TimeoutExt as _;
use fuchsia_component::client::connect_to_protocol_at_dir_root;
use fuchsia_fs::directory::clone_onto;
use fuchsia_fs::file::write;
use fuchsia_runtime::HandleType;
use futures::channel::mpsc;
use futures::lock::Mutex;
use futures::{StreamExt as _, TryFutureExt as _, TryStreamExt as _};
use std::collections::HashSet;
use std::sync::Arc;
use uuid::Uuid;
use vfs::service;
use zx::sys::{zx_handle_t, zx_status_t};
use zx::{self as zx, AsHandleRef, MonotonicDuration};
use {
    fidl_fuchsia_fshost as fshost, fidl_fuchsia_storage_partitions as fpartitions,
    fuchsia_async as fasync,
};

pub enum FshostShutdownResponder {
    Lifecycle(
        // TODO(https://fxbug.dev/333319162): Implement me.
        #[allow(dead_code)] LifecycleRequestStream,
    ),
}

impl FshostShutdownResponder {
    pub fn close(self) -> Result<(), fidl::Error> {
        match self {
            FshostShutdownResponder::Lifecycle(_) => {}
        }
        Ok(())
    }
}

const FIND_PARTITION_DURATION: MonotonicDuration = MonotonicDuration::from_seconds(20);

fn data_partition_names() -> Vec<String> {
    vec![DATA_PARTITION_LABEL.to_string(), LEGACY_DATA_PARTITION_LABEL.to_string()]
}

async fn find_data_partition(ramdisk_prefix: Option<String>) -> Result<ControllerProxy, Error> {
    let fvm_matcher = PartitionMatcher {
        detected_disk_formats: Some(vec![DiskFormat::Fvm]),
        ignore_prefix: ramdisk_prefix,
        ..Default::default()
    };

    let fvm_controller =
        find_partition(fvm_matcher, FIND_PARTITION_DURATION).await.context("Failed to find FVM")?;
    let fvm_path = fvm_controller
        .get_topological_path()
        .await
        .context("fvm get_topo_path transport error")?
        .map_err(zx::Status::from_raw)
        .context("fvm get_topo_path returned error")?;

    let fvm_dir = fuchsia_fs::directory::open_in_namespace(&fvm_path, fio::Flags::empty())?;
    let fvm_volume_manager_proxy = recursive_wait_and_open::<VolumeManagerMarker>(&fvm_dir, "/fvm")
        .await
        .context("failed to connect to the VolumeManager")?;

    // **NOTE**: We must call VolumeManager::GetInfo() to ensure all partitions are visible when
    // we enumerate them below. See https://fxbug.dev/42077585 for more information.
    zx::ok(fvm_volume_manager_proxy.get_info().await.context("transport error on get_info")?.0)
        .context("get_info failed")?;

    let fvm_dir =
        fuchsia_fs::directory::open_in_namespace(&format!("{fvm_path}/fvm"), fio::PERM_READABLE)?;

    let data_matcher = PartitionMatcher {
        type_guids: Some(vec![constants::DATA_TYPE_GUID]),
        labels: Some(data_partition_names()),
        parent_device: Some(fvm_path),
        ignore_if_path_contains: Some("zxcrypt/unsealed".to_string()),
        ..Default::default()
    };

    // We can't use find_partition because it looks in /dev/class/block and we can't be sure that
    // it will show up there yet (the block driver is bound after fvm has published its
    // volumes). Instead, we enumerate the topological path directory whose entries should be
    // present thanks to calling get_info above.
    for entry in fuchsia_fs::directory::readdir(&fvm_dir).await? {
        // This will wait for the block entry to show up.
        let proxy = recursive_wait_and_open::<ControllerMarker>(
            &fvm_dir,
            &format!("{}/block/device_controller", entry.name),
        )
        .await
        .context("opening partition path")?;
        match partition_matches_with_proxy(&proxy, &data_matcher).await {
            Ok(true) => {
                return Ok(proxy);
            }
            Ok(false) => {}
            Err(error) => {
                tracing::info!(?error, "Failure in partition match. Transient device?");
            }
        }
    }
    Err(anyhow!("Data partition not found"))
}

async fn wipe_storage(
    environment: &Arc<Mutex<dyn Environment>>,
    config: &fshost_config::Config,
    ramdisk_prefix: Option<String>,
    launcher: &FilesystemLauncher,
    matcher_lock: &Arc<Mutex<HashSet<String>>>,
    blobfs_root: Option<ServerEnd<DirectoryMarker>>,
    blob_creator: Option<ServerEnd<fidl_fuchsia_fxfs::BlobCreatorMarker>>,
) -> Result<(), Error> {
    if config.fxfs_blob {
        // For fxblob, we skip several of the arguments that the fvm one needs. For config and
        // launcher, there currently aren't any options that modify how fxblob works, so we don't
        // need access (that will probably change eventually, at which point those will need to be
        // threaded through). For ignored_paths, fxblob launching doesn't generate any new block
        // devices that we need to mark as accounted for for the block watcher.
        wipe_storage_fxblob(environment, blobfs_root, blob_creator).await
    } else {
        wipe_storage_fvm(config, ramdisk_prefix, launcher, matcher_lock, blobfs_root).await
    }
}

async fn wipe_storage_fxblob(
    environment: &Arc<Mutex<dyn Environment>>,
    blobfs_root: Option<ServerEnd<DirectoryMarker>>,
    blob_creator: Option<ServerEnd<fidl_fuchsia_fxfs::BlobCreatorMarker>>,
) -> Result<(), Error> {
    tracing::info!("Searching for fxfs block device");

    let registered_devices = environment.lock().await.registered_devices().clone();
    let block_connector = registered_devices
        .get_block_connector(DeviceTag::FxblobOnRecovery)
        .map_err(|error| {
            tracing::error!(?error, "shred_data_volume: unable to get block connector");
            zx::Status::NOT_FOUND
        })
        .on_timeout(FIND_PARTITION_DURATION, || {
            tracing::error!("Failed to find fxfs within timeout");
            Err(zx::Status::NOT_FOUND)
        })
        .await?;

    tracing::info!("Reformatting Fxfs.");

    let mut fxfs =
        filesystem::Filesystem::from_boxed_config(block_connector, Box::new(Fxfs::default()));
    fxfs.format().await.context("Failed to format fxfs")?;

    let blobfs_root = match blobfs_root {
        Some(handle) => handle,
        None => {
            tracing::info!("Not provisioning fxblob: missing blobfs root handle");
            return Ok(());
        }
    };

    let blob_creator = match blob_creator {
        Some(handle) => handle,
        None => {
            tracing::info!("Not provisioning fxblob: missing blob creator handle");
            return Ok(());
        }
    };

    let mut serving_fxfs = fxfs.serve_multi_volume().await.context("serving fxfs")?;
    let blob_volume = serving_fxfs
        .create_volume(
            BLOB_VOLUME_LABEL,
            fidl_fuchsia_fs_startup::CreateOptions::default(),
            fidl_fuchsia_fs_startup::MountOptions { as_blob: Some(true), ..Default::default() },
        )
        .await
        .context("making blob volume")?;
    clone_onto(blob_volume.root(), blobfs_root)?;
    blob_volume.exposed_dir().open(
        fio::OpenFlags::empty(),
        fio::ModeType::empty(),
        "svc/fuchsia.fxfs.BlobCreator",
        blob_creator.into_channel().into(),
    )?;
    // Prevent fs_management from shutting down the filesystem when it's dropped.
    let _ = serving_fxfs.take_exposed_dir();
    Ok(())
}

extern "C" {
    // This function initializes FVM on a fuchsia.hardware.block.Block device
    // with a given slice size.
    fn fvm_init(device: zx_handle_t, slice_size: usize) -> zx_status_t;
}

fn initialize_fvm(fvm_slice_size: u64, device: &BlockProxy) -> Result<(), Error> {
    let device_raw = device.as_channel().raw_handle();
    let status = unsafe { fvm_init(device_raw, fvm_slice_size as usize) };
    zx::Status::ok(status).context("fvm_init failed")?;
    Ok(())
}

async fn wipe_storage_fvm(
    config: &fshost_config::Config,
    ramdisk_prefix: Option<String>,
    launcher: &FilesystemLauncher,
    matcher_lock: &Arc<Mutex<HashSet<String>>>,
    blobfs_root: Option<ServerEnd<DirectoryMarker>>,
) -> Result<(), Error> {
    tracing::info!("Searching for block device with FVM");
    let mut ignored_paths = matcher_lock.lock().await;

    let fvm_matcher = PartitionMatcher {
        type_guids: Some(vec![constants::FVM_TYPE_GUID, constants::FVM_LEGACY_TYPE_GUID]),
        ignore_prefix: ramdisk_prefix,
        ..Default::default()
    };

    let fvm_controller =
        find_partition(fvm_matcher, FIND_PARTITION_DURATION).await.context("Failed to find FVM")?;
    let fvm_path = fvm_controller
        .get_topological_path()
        .await
        .context("fvm get_topo_path transport error")?
        .map_err(zx::Status::from_raw)
        .context("fvm get_topo_path returned error")?;

    tracing::info!(device_path = ?fvm_path, "Wiping storage");
    tracing::info!("Unbinding child drivers (FVM/zxcrypt).");

    fvm_controller.unbind_children().await?.map_err(zx::Status::from_raw)?;

    tracing::info!(slice_size = config.fvm_slice_size, "Initializing FVM");
    let (block_proxy, server_end) = fidl::endpoints::create_proxy::<BlockMarker>();
    fvm_controller
        .connect_to_device_fidl(server_end.into_channel())
        .context("connecting to block protocol")?;
    initialize_fvm(config.fvm_slice_size, &block_proxy)?;

    tracing::info!("Binding and waiting for FVM driver.");
    fvm_controller.bind(constants::FVM_DRIVER_PATH).await?.map_err(zx::Status::from_raw)?;

    let fvm_dir = fuchsia_fs::directory::open_in_namespace(&fvm_path, fio::Flags::empty())
        .context("Failed to open the fvm directory")?;

    let fvm_volume_manager_proxy =
        device_watcher::recursive_wait_and_open::<VolumeManagerMarker>(&fvm_dir, "fvm")
            .await
            .context("waiting for FVM driver")?;

    let blobfs_root = match blobfs_root {
        Some(handle) => handle,
        None => {
            tracing::info!("Not provisioning blobfs");
            return Ok(());
        }
    };

    tracing::info!("Allocating new partitions");
    // Volumes will be dynamically resized.
    const INITIAL_SLICE_COUNT: u64 = 1;

    // Generate FVM layouts and new GUIDs for the blob/data volumes.
    let blobfs_controller = fvm_allocate_partition(
        &fvm_volume_manager_proxy,
        constants::BLOBFS_TYPE_GUID,
        *Uuid::new_v4().as_bytes(),
        constants::BLOBFS_PARTITION_LABEL,
        0,
        INITIAL_SLICE_COUNT,
    )
    .await
    .context("Failed to allocate blobfs fvm partition")?;

    let device_path =
        blobfs_controller.get_topological_path().await?.map_err(zx::Status::from_raw)?;
    ignored_paths.insert(device_path.clone());

    fvm_allocate_partition(
        &fvm_volume_manager_proxy,
        constants::DATA_TYPE_GUID,
        *Uuid::new_v4().as_bytes(),
        constants::DATA_PARTITION_LABEL,
        0,
        INITIAL_SLICE_COUNT,
    )
    .await
    .context("Failed to allocate fvm data partition")?;

    tracing::info!("Formatting Blobfs.");
    let mut blobfs_config = Blobfs {
        deprecated_padded_blobfs_format: config.blobfs_use_deprecated_padded_format,
        ..launcher.get_blobfs_config()
    };
    if config.blobfs_initial_inodes > 0 {
        blobfs_config.num_inodes = config.blobfs_initial_inodes;
    }

    let mut blobfs = filesystem::Filesystem::new(blobfs_controller, blobfs_config);
    blobfs.format().await.context("Failed to format blobfs")?;
    let started_blobfs = blobfs.serve().await.context("serving blobfs")?;
    clone_onto(started_blobfs.root(), blobfs_root)?;
    // Prevent fs_management from shutting down the filesystem when it's dropped.
    let _ = started_blobfs.take_exposed_dir();
    Ok(())
}

async fn write_data_file(
    environment: &Arc<Mutex<dyn Environment>>,
    config: &fshost_config::Config,
    ramdisk_prefix: Option<String>,
    launcher: &FilesystemLauncher,
    filename: &str,
    payload: zx::Vmo,
) -> Result<(), Error> {
    if !config.ramdisk_image && !config.netboot {
        return Err(anyhow!(
            "Can't WriteDataFile from a non-recovery build;
            ramdisk_image must be set."
        ));
    }

    let content_size = if let Ok(content_size) = payload.get_content_size() {
        content_size
    } else if let Ok(content_size) = payload.get_size() {
        content_size
    } else {
        return Err(anyhow!("Failed to get content size"));
    };

    let content_size =
        usize::try_from(content_size).context("Failed to convert u64 content_size to usize")?;

    let (mut filesystem, mut data) = if config.fxfs_blob {
        fxblob::mount_or_format_data(environment, launcher)
            .await
            .map(|(fs, data)| ((Some(fs), data)))?
    } else {
        let partition_controller = find_data_partition(ramdisk_prefix).await?;

        let format = match config.data_filesystem_format.as_ref() {
            "fxfs" => DiskFormat::Fxfs,
            "f2fs" => DiskFormat::F2fs,
            "minfs" => DiskFormat::Minfs,
            _ => panic!("unsupported data filesystem format type"),
        };

        let partition_path = partition_controller
            .get_topological_path()
            .await
            .context("get_topo_path transport error")?
            .map_err(zx::Status::from_raw)
            .context("get_topo_path returned error")?;
        tracing::info!(%partition_path, "Found data partition");
        let mut device = Box::new(
            BlockDevice::from_proxy(partition_controller, &partition_path)
                .await
                .context("failed to make new device")?,
        );
        let mut device: &mut dyn Device = device.as_mut();
        let mut zxcrypt_device;
        if format != DiskFormat::Fxfs && !config.no_zxcrypt {
            launcher.attach_driver(device, ZXCRYPT_DRIVER_PATH).await?;
            tracing::info!("Ensuring device is formatted with zxcrypt");
            zxcrypt_device = Box::new(
                match ZxcryptDevice::unseal(device).await.context("Failed to unseal zxcrypt")? {
                    UnsealOutcome::Unsealed(device) => device,
                    UnsealOutcome::FormatRequired => ZxcryptDevice::format(device).await?,
                },
            );
            device = zxcrypt_device.as_mut();
        }

        let filesystem = match format {
            DiskFormat::Fxfs => {
                launcher.serve_data(device, Fxfs::dynamic_child()).await.context("serving fxfs")?
            }
            DiskFormat::F2fs => {
                launcher.serve_data(device, F2fs::dynamic_child()).await.context("serving f2fs")?
            }
            DiskFormat::Minfs => launcher
                .serve_data(device, Minfs::dynamic_child())
                .await
                .context("serving minfs")?,
            _ => unreachable!(),
        };
        let filesystem = match filesystem {
            ServeFilesystemStatus::Serving(fs) => fs,
            ServeFilesystemStatus::FormatRequired => {
                tracing::info!(
                    "Format required {:?} for device {:?}",
                    format,
                    device.topological_path()
                );
                match format {
                    DiskFormat::Fxfs => launcher
                        .format_data(device, Fxfs::dynamic_child())
                        .await
                        .context("serving fxfs")?,
                    DiskFormat::F2fs => launcher
                        .format_data(device, F2fs::dynamic_child())
                        .await
                        .context("serving f2fs")?,
                    DiskFormat::Minfs => launcher
                        .format_data(device, Minfs::dynamic_child())
                        .await
                        .context("serving minfs")?,
                    _ => unreachable!(),
                }
            }
        };

        (None, filesystem)
    };
    let data_root = data.root(filesystem.as_mut()).context("Failed to get data root")?;
    let (directory_proxy, file_path) = match filename.rsplit_once("/") {
        Some((directory_path, relative_file_path)) => {
            let directory_proxy = fuchsia_fs::directory::create_directory_recursive(
                &data_root,
                directory_path,
                fio::Flags::FLAG_MAYBE_CREATE | fio::PERM_READABLE | fio::PERM_WRITABLE,
            )
            .await
            .context("Failed to create directory")?;
            (directory_proxy, relative_file_path)
        }
        None => (data_root, filename),
    };

    let file_proxy = fuchsia_fs::directory::open_file(
        &directory_proxy,
        file_path,
        fio::Flags::FLAG_MAYBE_CREATE | fio::PERM_READABLE | fio::PERM_WRITABLE,
    )
    .await
    .context("Failed to open file")?;

    let mut contents = vec![0; content_size];
    payload.read(&mut contents, 0).context("reading payload vmo")?;
    write(&file_proxy, &contents).await.context("writing file contents")?;

    data.shutdown(filesystem.as_mut()).await.context("shutting down data")?;
    if let Some(fs) = filesystem {
        fs.shutdown().await.context("shutting down filesystem")?;
    }
    return Ok(());
}

async fn shred_data_volume(
    environment: &Arc<Mutex<dyn Environment>>,
    config: &fshost_config::Config,
    ramdisk_prefix: Option<String>,
    launcher: &FilesystemLauncher,
) -> Result<(), zx::Status> {
    if config.data_filesystem_format != "fxfs" {
        return Err(zx::Status::NOT_SUPPORTED);
    }
    // If we expect Fxfs to be live, ask `environment` to shred the data volume.
    if (config.data || config.fxfs_blob) && !config.ramdisk_image {
        tracing::info!("Filesystem is running; shredding online.");
        environment.lock().await.shred_data().await.map_err(|err| {
            debug_log(&format!("Failed to shred data: {:?}", err));
            zx::Status::INTERNAL
        })?;
    } else {
        // Otherwise we need to find the Fxfs partition and shred it.
        tracing::info!("Filesystem is not running; shredding offline.");

        let filesystem = if config.fxfs_blob {
            // We find the device via our own matcher.
            let registered_devices = environment.lock().await.registered_devices().clone();
            let block_connector = registered_devices
                .get_block_connector(DeviceTag::FxblobOnRecovery)
                .map_err(|error| {
                    tracing::error!(?error, "shred_data_volume: unable to get block connector");
                    zx::Status::NOT_FOUND
                })
                .on_timeout(FIND_PARTITION_DURATION, || {
                    tracing::error!("Failed to find fxfs within timeout");
                    Err(zx::Status::NOT_FOUND)
                })
                .await?;

            // Check to see if the device needs formatting.
            let format = detect_disk_format(
                &block_connector
                    .connect_block()
                    .map_err(|error| {
                        tracing::error!(?error, "connect_block failed");
                        zx::Status::INTERNAL
                    })?
                    .into_proxy(),
            )
            .await;
            if format != DiskFormat::Fxfs {
                ServeFilesystemStatus::FormatRequired
            } else {
                launcher
                    .serve_data_from(fs_management::filesystem::Filesystem::from_boxed_config(
                        block_connector,
                        Box::new(Fxfs::dynamic_child()),
                    ))
                    .await
                    .map_err(|error| {
                        tracing::error!(?error, "serving fxfs");
                        zx::Status::INTERNAL
                    })?
            }
        } else {
            let partition_controller = find_data_partition(ramdisk_prefix).await.map_err(|e| {
                tracing::error!("shred_data_volume: unable to find partition: {e:?}");
                zx::Status::NOT_FOUND
            })?;
            let partition_path = partition_controller
                .get_topological_path()
                .await
                .map_err(|e| {
                    tracing::error!("Failed to get topo path (fidl error): {e:?}");
                    zx::Status::INTERNAL
                })?
                .map_err(|e| {
                    let status = zx::Status::from_raw(e);
                    tracing::error!("Failed to get topo path: {}", status.to_string());
                    status
                })?;
            let mut device = Box::new(
                BlockDevice::from_proxy(partition_controller, &partition_path).await.map_err(
                    |e| {
                        tracing::error!("failed to make new device: {e:?}");
                        zx::Status::NOT_FOUND
                    },
                )?,
            );
            launcher.serve_data(device.as_mut(), Fxfs::dynamic_child()).await.map_err(|e| {
                tracing::error!("serving fxfs: {e:?}");
                zx::Status::INTERNAL
            })?
        };

        let mut filesystem = match filesystem {
            // If we already need to format for some reason, we don't need to worry about shredding
            // the data volume.
            ServeFilesystemStatus::FormatRequired => return Ok(()),
            ServeFilesystemStatus::Serving(fs) => fs,
        };
        let unencrypted = filesystem.volume(UNENCRYPTED_VOLUME_LABEL).ok_or_else(|| {
            tracing::error!("Failed to find unencrypted volume");
            zx::Status::NOT_FOUND
        })?;
        let dir =
            fuchsia_fs::directory::open_directory(unencrypted.root(), "keys", fio::PERM_WRITABLE)
                .await
                .map_err(|e| {
                    tracing::error!("Failed to open keys dir: {e:?}");
                    zx::Status::INTERNAL
                })?;
        dir.unlink("fxfs-data", &fio::UnlinkOptions::default())
            .await
            .map_err(|e| {
                tracing::error!("Failed to remove keybag (fidl error): {e:?}");
                zx::Status::INTERNAL
            })?
            .map_err(|e| {
                let status = zx::Status::from_raw(e);
                tracing::error!("Failed to remove keybag: {}", status.to_string());
                status
            })?;
        debug_log("Deleted fxfs-data keybag");
    }
    tracing::info!("Shredded the data volume.  Data will be lost!!");
    Ok(())
}

async fn init_system_partition_table(
    partitions: Vec<fpartitions::PartitionInfo>,
    environment: &Arc<Mutex<dyn Environment>>,
    config: &fshost_config::Config,
) -> Result<(), zx::Status> {
    if !config.netboot {
        tracing::error!("init_system_partition_table only supported in netboot mode.");
        return Err(zx::Status::NOT_SUPPORTED);
    }
    if !config.storage_host {
        tracing::error!("init_system_partition_table only supported in storage-host mode.");
        return Err(zx::Status::NOT_SUPPORTED);
    }
    if !config.gpt {
        tracing::error!("init_system_partition_table called on a non-gpt system.");
        return Err(zx::Status::NOT_SUPPORTED);
    }

    let registered_devices = environment.lock().await.registered_devices().clone();
    const TIMEOUT: MonotonicDuration = MonotonicDuration::from_seconds(10);
    let _ = registered_devices
        .get_block_connector(DeviceTag::SystemPartitionTable)
        .map_err(|error| {
            tracing::error!(?error, "init_system_partition_table: unable to get block connector");
            zx::Status::NOT_FOUND
        })
        .on_timeout(TIMEOUT, || {
            tracing::error!("init_system_partition_table: Failed to find gpt within timeout");
            Err(zx::Status::NOT_FOUND)
        })
        .await?;

    tracing::info!("init_system_partition_table: Reformatting GPT...");
    let exposed_dir = environment.lock().await.partition_manager_exposed_dir().map_err(|err| {
        tracing::error!(
            ?err,
            "init_system_partition_table: Failed to connect to partition manager"
        );
        Err(zx::Status::BAD_STATE)
    })?;
    let client =
        connect_to_protocol_at_dir_root::<fpartitions::PartitionsAdminMarker>(&exposed_dir)
            .unwrap();
    client
        .reset_partition_table(&partitions[..])
        .await
        .map_err(|err| {
            tracing::error!(?err, "init_system_partition_table: FIDL error");
            Err(zx::Status::PEER_CLOSED)
        })?
        .map_err(zx::Status::from_raw)
}

/// Make a new vfs service node that implements fuchsia.fshost.Admin
pub fn fshost_admin(
    environment: Arc<Mutex<dyn Environment>>,
    config: Arc<fshost_config::Config>,
    ramdisk_prefix: Option<String>,
    launcher: Arc<FilesystemLauncher>,
    matcher_lock: Arc<Mutex<HashSet<String>>>,
) -> Arc<service::Service> {
    service::host(move |mut stream: fshost::AdminRequestStream| {
        let env = environment.clone();
        let config = config.clone();
        let ramdisk_prefix = ramdisk_prefix.clone();
        let launcher = launcher.clone();
        let matcher_lock = matcher_lock.clone();
        async move {
            while let Some(request) = stream.next().await {
                match request {
                    Ok(fshost::AdminRequest::Mount { responder, .. }) => {
                        tracing::info!("admin mount called");
                        responder.send(Err(zx::Status::NOT_SUPPORTED.into_raw())).unwrap_or_else(
                            |e| {
                                tracing::error!("failed to send Mount response. error: {:?}", e);
                            },
                        );
                    }
                    Ok(fshost::AdminRequest::Unmount { responder, .. }) => {
                        tracing::info!("admin unmount called");
                        responder.send(Err(zx::Status::NOT_SUPPORTED.into_raw())).unwrap_or_else(
                            |e| {
                                tracing::error!("failed to send Unmount response. error: {:?}", e);
                            },
                        );
                    }
                    Ok(fshost::AdminRequest::GetDevicePath { responder, .. }) => {
                        tracing::info!("admin get device path called");
                        responder.send(Err(zx::Status::NOT_SUPPORTED.into_raw())).unwrap_or_else(
                            |e| {
                                tracing::error!(
                                    "failed to send GetDevicePath response. error: {:?}",
                                    e
                                );
                            },
                        );
                    }
                    Ok(fshost::AdminRequest::WriteDataFile { responder, payload, filename }) => {
                        tracing::info!(?filename, "admin write data file called");
                        let res = match write_data_file(
                            &env,
                            &config,
                            ramdisk_prefix.clone(),
                            &launcher,
                            &filename,
                            payload,
                        )
                        .await
                        {
                            Ok(()) => Ok(()),
                            Err(e) => {
                                tracing::error!("admin service: write_data_file failed: {:?}", e);
                                Err(zx::Status::INTERNAL.into_raw())
                            }
                        };
                        responder.send(res).unwrap_or_else(|e| {
                            tracing::error!(
                                "failed to send WriteDataFile response. error: {:?}",
                                e
                            );
                        });
                    }
                    Ok(fshost::AdminRequest::WipeStorage {
                        responder,
                        blobfs_root,
                        blob_creator,
                    }) => {
                        tracing::info!("admin wipe storage called");
                        let res = if !config.ramdisk_image {
                            tracing::error!(
                                "Can't WipeStorage from a non-recovery build; \
                                ramdisk_image must be set."
                            );
                            Err(zx::Status::NOT_SUPPORTED.into_raw())
                        } else {
                            match wipe_storage(
                                &env,
                                &config,
                                ramdisk_prefix.clone(),
                                &launcher,
                                &matcher_lock,
                                blobfs_root,
                                blob_creator,
                            )
                            .await
                            {
                                Ok(()) => Ok(()),
                                Err(e) => {
                                    tracing::error!(?e, "admin service: wipe_storage failed");
                                    Err(zx::Status::INTERNAL.into_raw())
                                }
                            }
                        };
                        responder.send(res).unwrap_or_else(|e| {
                            tracing::error!(?e, "failed to send WipeStorage response");
                        });
                    }
                    Ok(fshost::AdminRequest::ShredDataVolume { responder }) => {
                        tracing::info!("admin shred data volume called");
                        let res = match shred_data_volume(
                            &env,
                            &config,
                            ramdisk_prefix.clone(),
                            &launcher,
                        )
                        .await
                        {
                            Ok(()) => Ok(()),
                            Err(e) => {
                                debug_log(&format!(
                                    "admin service: shred_data_volume failed: {:?}",
                                    e
                                ));
                                Err(e.into_raw())
                            }
                        };
                        responder.send(res).unwrap_or_else(|e| {
                            tracing::error!(
                                "failed to send ShredDataVolume response. error: {:?}",
                                e
                            );
                        });
                    }
                    Ok(fshost::AdminRequest::StorageHostEnabled { responder }) => {
                        responder.send(config.storage_host).unwrap_or_else(|e| {
                            tracing::error!(
                                "failed to send StorageHostEnabled response. error: {:?}",
                                e
                            );
                        });
                    }
                    Err(e) => {
                        tracing::error!("admin server failed: {:?}", e);
                        return;
                    }
                }
            }
        }
    })
}

pub fn fshost_recovery(
    environment: Arc<Mutex<dyn Environment>>,
    config: Arc<fshost_config::Config>,
) -> Arc<service::Service> {
    service::host(move |mut stream: fshost::RecoveryRequestStream| {
        let env = environment.clone();
        let config = config.clone();
        async move {
            while let Some(request) = stream.next().await {
                match request {
                    Ok(fshost::RecoveryRequest::InitSystemPartitionTable {
                        partitions,
                        responder,
                    }) => {
                        tracing::info!("recovery init gpt called");
                        let res = match init_system_partition_table(partitions, &env, &config).await
                        {
                            Ok(()) => Ok(()),
                            Err(e) => {
                                debug_log(&format!(
                                    "recovery service: init_system_partition_table failed: {:?}",
                                    e
                                ));
                                Err(e.into_raw())
                            }
                        };
                        responder.send(res).unwrap_or_else(|e| {
                            tracing::error!(
                                "failed to send InitSystemPartitionTable response. error: {:?}",
                                e
                            );
                        });
                    }
                    Err(e) => {
                        tracing::error!("admin server failed: {:?}", e);
                        return;
                    }
                }
            }
        }
    })
}

pub fn handle_lifecycle_requests(
    mut shutdown: mpsc::Sender<FshostShutdownResponder>,
) -> Result<(), Error> {
    if let Some(handle) = fuchsia_runtime::take_startup_handle(HandleType::Lifecycle.into()) {
        let mut stream =
            LifecycleRequestStream::from_channel(fasync::Channel::from_channel(handle.into()));
        fasync::Task::spawn(async move {
            if let Ok(Some(LifecycleRequest::Stop { .. })) = stream.try_next().await {
                shutdown.start_send(FshostShutdownResponder::Lifecycle(stream)).unwrap_or_else(
                    |e| tracing::error!("failed to send shutdown message. error: {:?}", e),
                );
            }
        })
        .detach();
    }
    Ok(())
}
