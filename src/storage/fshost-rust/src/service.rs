// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        crypt::fxfs,
        debug_log,
        device::{constants, BlockDevice},
        environment::FilesystemLauncher,
        service::fxfs::KEY_BAG_FILE,
        watcher,
    },
    anyhow::{anyhow, ensure, Context, Error},
    fidl::endpoints::{Proxy, RequestStream, ServerEnd},
    fidl_fuchsia_device::ControllerMarker,
    fidl_fuchsia_fshost as fshost,
    fidl_fuchsia_hardware_block::{BlockMarker, BlockProxy},
    fidl_fuchsia_hardware_block_volume::VolumeManagerProxy,
    fidl_fuchsia_io::{self as fio, DirectoryMarker, OpenFlags},
    fidl_fuchsia_process_lifecycle::{LifecycleRequest, LifecycleRequestStream},
    fs_management::{
        filesystem,
        format::DiskFormat,
        partition::{find_partition, fvm_allocate_partition, PartitionMatcher},
        Blobfs, F2fs, Fxfs, Minfs,
    },
    fuchsia_async as fasync,
    fuchsia_component::client::connect_to_protocol_at_path,
    fuchsia_fs::{
        directory::{clone_onto_no_describe, create_directory_recursive, open_file},
        file::write,
    },
    fuchsia_runtime::HandleType,
    fuchsia_zircon::{self as zx, sys::zx_handle_t, zx_status_t, AsHandleRef, Duration},
    futures::{channel::mpsc, StreamExt, TryStreamExt},
    remote_block_device::{BlockClient, BufferSlice, RemoteBlockClient},
    std::sync::Arc,
    tracing::info,
    uuid::Uuid,
    vfs::service,
};

pub enum FshostShutdownResponder {
    Lifecycle(LifecycleRequestStream),
}

impl FshostShutdownResponder {
    pub fn close(self) -> Result<(), fidl::Error> {
        match self {
            FshostShutdownResponder::Lifecycle(_) => {}
        }
        Ok(())
    }
}

const FIND_PARTITION_DURATION: Duration = Duration::from_seconds(10);
const DATA_PARTITION_LABEL: &str = "data";
const LEGACY_DATA_PARTITION_LABEL: &str = "minfs";
pub const SHRED_DATA_VOLUME_MARKER_FILE: &str = "shred_data_volume";

fn data_partition_names() -> Vec<String> {
    vec![DATA_PARTITION_LABEL.to_string(), LEGACY_DATA_PARTITION_LABEL.to_string()]
}

async fn find_data_partition(config: &fshost_config::Config) -> Result<String, Error> {
    let fvm_matcher = PartitionMatcher {
        detected_disk_formats: Some(vec![DiskFormat::Fvm]),
        ignore_prefix: Some(config.ramdisk_prefix.clone()),
        ..Default::default()
    };

    let fvm_path =
        find_partition(fvm_matcher, FIND_PARTITION_DURATION).await.context("Failed to find FVM")?;

    let data_matcher = PartitionMatcher {
        type_guids: Some(vec![constants::DATA_TYPE_GUID]),
        labels: Some(data_partition_names()),
        parent_device: Some(fvm_path),
        ignore_if_path_contains: Some("zxcrypt/unsealed".to_string()),
        ..Default::default()
    };

    Ok(find_partition(data_matcher, FIND_PARTITION_DURATION)
        .await
        .context("Failed to open partition")?)
}

#[link(name = "fvm")]
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

async fn wipe_storage(
    config: &fshost_config::Config,
    launcher: &FilesystemLauncher,
    mut pauser: watcher::Watcher,
    blobfs_root: ServerEnd<DirectoryMarker>,
) -> Result<(), Error> {
    ensure!(!config.ramdisk_prefix.is_empty());
    tracing::info!("Searching for block device with FVM");

    let fvm_matcher = PartitionMatcher {
        type_guids: Some(vec![constants::FVM_TYPE_GUID, constants::FVM_LEGACY_TYPE_GUID]),
        ignore_prefix: Some(config.ramdisk_prefix.clone()),
        ..Default::default()
    };

    let fvm_path =
        find_partition(fvm_matcher, FIND_PARTITION_DURATION).await.context("Failed to find FVM")?;

    // We need to pause the block watcher to make sure fshost doesn't try to mount or format any of
    // the newly provisioned volumes in the FVM.
    let paused = pauser.is_paused().await;
    if !paused {
        tracing::info!("Pausing block watcher");
        pauser.pause().await.context("Failed to pause the block watcher")?;
    } else {
        tracing::info!("Block watcher already paused in WipeStorage");
    }

    let fvm_proxy = connect_to_protocol_at_path::<ControllerMarker>(&fvm_path)
        .context("Failed to create a fvm Controller proxy")?;

    tracing::info!(device_path = ?fvm_path, "Wiping storage");
    tracing::info!("Unbinding child drivers (FVM/zxcrypt).");

    fvm_proxy.unbind_children().await?.map_err(zx::Status::from_raw)?;

    tracing::info!(slice_size = config.fvm_slice_size, "Initializing FVM");
    let device = connect_to_protocol_at_path::<BlockMarker>(&fvm_path)
        .context("Failed to create fvm Block proxy")?;
    initialize_fvm(config.fvm_slice_size, &device)?;

    tracing::info!("Binding and waiting for FVM driver.");
    fvm_proxy.bind(constants::FVM_DRIVER_PATH).await?.map_err(zx::Status::from_raw)?;

    let fvm_dir = fuchsia_fs::directory::open_in_namespace(&fvm_path, OpenFlags::RIGHT_READABLE)
        .context("Failed to open the fvm directory")?;

    let fvm_device = device_watcher::recursive_wait_and_open_node(&fvm_dir, "fvm")
        .await
        .context("waiting for FVM driver")?;

    tracing::info!("Allocating new partitions");
    // Volumes will be dynamically resized.
    const INITIAL_SLICE_COUNT: u64 = 1;

    let fvm_volume_manager_proxy = VolumeManagerProxy::new(fvm_device.into_channel().unwrap());

    // Generate FVM layouts and new GUIDs for the blob/data volumes.
    let blobfs_path = fvm_allocate_partition(
        &fvm_volume_manager_proxy,
        constants::BLOBFS_TYPE_GUID,
        *Uuid::new_v4().as_bytes(),
        constants::BLOBFS_PARTITION_LABEL,
        0,
        INITIAL_SLICE_COUNT,
    )
    .await
    .context("Failed to allocate blobfs fvm partition")?;

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
        ..Default::default()
    };
    if config.blobfs_initial_inodes > 0 {
        blobfs_config.num_inodes = config.blobfs_initial_inodes;
    }

    let mut blobfs = filesystem::Filesystem::from_path(&blobfs_path, blobfs_config)?;
    blobfs.format().await.context("Failed to format blobfs")?;
    let mut blobfs_device =
        BlockDevice::new(blobfs_path).await.context("Failed to make new device")?;
    let mut started_blobfs =
        launcher.serve_blobfs(&mut blobfs_device).await.context("Failed to mount blobfs")?;
    clone_onto_no_describe(
        &started_blobfs.root().context("Failed to get blobfs root")?,
        None,
        blobfs_root,
    )?;
    // We use forget() here to ensure that our Blobfs instance is not dropped, which would cause the
    // filesystem to shutdown.
    std::mem::forget(started_blobfs);
    Ok(())
}

async fn write_data_file(
    config: &fshost_config::Config,
    launcher: &FilesystemLauncher,
    filename: &str,
    payload: zx::Vmo,
) -> Result<(), Error> {
    if !config.fvm_ramdisk && !config.netboot {
        return Err(anyhow!(
            "Can't WriteDataFile from a non-recovery build;
            fvm_ramdisk must be set."
        ));
    }
    // When `fvm_ramdisk` is set, zxcrypt will be bound (and formatted if needed) automatically
    // during matching.  Otherwise, do it ourselves.
    let bind_zxcrypt = !config.fvm_ramdisk;

    let content_size = if let Ok(content_size) = payload.get_content_size() {
        content_size
    } else if let Ok(content_size) = payload.get_size() {
        content_size
    } else {
        return Err(anyhow!("Failed to get content size"));
    };

    let content_size =
        usize::try_from(content_size).context("Failed to convert u64 content_size to usize")?;

    assert_eq!(config.ramdisk_prefix.is_empty(), false);

    let mut partition_path = find_data_partition(config).await?;

    let format = match config.data_filesystem_format.as_ref() {
        "fxfs" => DiskFormat::Fxfs,
        "f2fs" => DiskFormat::F2fs,
        "minfs" => DiskFormat::Minfs,
        _ => panic!("unsupported data filesystem format type"),
    };

    let mut inside_zxcrypt = false;

    if format != DiskFormat::Fxfs && !config.no_zxcrypt {
        if bind_zxcrypt {
            info!("Ensuring device is formatted with zxcrypt");
            let mut device =
                BlockDevice::new(partition_path.clone()).await.context("failed to make device")?;
            launcher.bind_zxcrypt(&mut device).await.context("Failed to bind zxcrypt")?;
        }
        let mut zxcrypt_path = partition_path;
        zxcrypt_path.push_str("/zxcrypt/unsealed");
        let zxcrypt_matcher = PartitionMatcher {
            type_guids: Some(vec![constants::DATA_TYPE_GUID]),
            labels: Some(data_partition_names()),
            parent_device: Some(zxcrypt_path),
            ..Default::default()
        };
        partition_path = find_partition(zxcrypt_matcher, FIND_PARTITION_DURATION)
            .await
            .context("Failed to open zxcrypt partition")?;
        inside_zxcrypt = true;
    }

    let mut device = BlockDevice::new(partition_path).await.context("failed to make new device")?;
    let mut filesystem = match format {
        DiskFormat::Fxfs => launcher
            .serve_data(&mut device, Fxfs::dynamic_child(), inside_zxcrypt, None)
            .await
            .context("serving fxfs")?,
        DiskFormat::F2fs => launcher
            .serve_data(&mut device, F2fs::dynamic_child(), inside_zxcrypt, None)
            .await
            .context("serving f2fs")?,
        DiskFormat::Minfs => launcher
            .serve_data(&mut device, Minfs::dynamic_child(), inside_zxcrypt, None)
            .await
            .context("serving minfs")?,
        _ => unreachable!(),
    };

    let data_root = filesystem.root().context("Failed to get data root")?;
    let (directory_proxy, file_path) = match filename.rsplit_once("/") {
        Some((directory_path, relative_file_path)) => {
            let directory_proxy = create_directory_recursive(
                &data_root,
                directory_path,
                OpenFlags::CREATE | OpenFlags::RIGHT_READABLE | OpenFlags::RIGHT_WRITABLE,
            )
            .await
            .context("Failed to create directory")?;
            (directory_proxy, relative_file_path)
        }
        None => (data_root, filename),
    };

    let file_proxy = open_file(
        &directory_proxy,
        file_path,
        OpenFlags::CREATE | OpenFlags::RIGHT_READABLE | OpenFlags::RIGHT_WRITABLE,
    )
    .await
    .context("Failed to open file")?;

    let mut data = vec![0; content_size];
    payload.read(&mut data, 0).context("reading payload vmo")?;
    write(&file_proxy, &data).await.context("writing file contents")?;

    filesystem.shutdown().await.context("shutting down data filesystem")?;
    return Ok(());
}

async fn shred_data_volume(
    config: &fshost_config::Config,
    data_root: &fio::DirectoryProxy,
) -> Result<(), zx::Status> {
    if config.data_filesystem_format != "fxfs" {
        return Err(zx::Status::NOT_SUPPORTED);
    }
    // If we expect Fxfs to be live, just erase the key bag.
    if config.data && !config.fvm_ramdisk {
        if config.use_native_fxfs_crypto {
            std::fs::remove_file(KEY_BAG_FILE)?;
            debug_log("Erased key bag");
        } else {
            // If we're using legacy crypto (which we will until we can switch to hardware backed
            // keys), all we can do is store a file so that the volume gets wiped on next boot.
            fuchsia_fs::directory::open_file(
                data_root,
                SHRED_DATA_VOLUME_MARKER_FILE,
                fio::OpenFlags::CREATE,
            )
            .await
            .map_err(|error| {
                tracing::warn!(?error, "Unable to create shred_data_volume marker file");
                zx::Status::INTERNAL
            })?;
            debug_log("Wrote shred_data_volume marker file");
        }
    } else {
        // Otherwise we need to find the Fxfs partition and shred it.
        let partition_path =
            find_data_partition(config).await.map_err(|_| zx::Status::NOT_FOUND)?;

        let block_client = RemoteBlockClient::new(
            connect_to_protocol_at_path::<BlockMarker>(&partition_path)
                .map_err(|_| zx::Status::INTERNAL)?,
        )
        .await
        .map_err(|_| zx::Status::INTERNAL)?;

        // Overwrite both super blocks.  Deliberately use a non-zero value so that we can tell this
        // has happened if we're debugging.
        let buf = vec![0x93; std::cmp::min(block_client.block_size() as usize, 4096)];
        futures::try_join!(
            block_client.write_at(BufferSlice::Memory(&buf), 0),
            block_client.write_at(BufferSlice::Memory(&buf), 524_288)
        )
        .map_err(|_| zx::Status::IO)?;

        debug_log("Wiped Fxfs super blocks");
    }
    Ok(())
}

/// Make a new vfs service node that implements fuchsia.fshost.Admin
pub fn fshost_admin(
    config: Arc<fshost_config::Config>,
    launcher: Arc<FilesystemLauncher>,
    data_root: fio::DirectoryProxy,
    pauser: watcher::Watcher,
) -> Arc<service::Service> {
    service::host(move |mut stream: fshost::AdminRequestStream| {
        let config = config.clone();
        let launcher = launcher.clone();
        let data_root = fuchsia_fs::directory::clone_no_describe(&data_root, None).unwrap();
        let pauser = pauser.clone();
        async move {
            while let Some(request) = stream.next().await {
                match request {
                    Ok(fshost::AdminRequest::Mount { responder, .. }) => {
                        tracing::info!("admin mount called");
                        responder
                            .send(&mut Err(zx::Status::NOT_SUPPORTED.into_raw()))
                            .unwrap_or_else(|e| {
                                tracing::error!("failed to send Mount response. error: {:?}", e);
                            });
                    }
                    Ok(fshost::AdminRequest::Unmount { responder, .. }) => {
                        tracing::info!("admin unmount called");
                        responder
                            .send(&mut Err(zx::Status::NOT_SUPPORTED.into_raw()))
                            .unwrap_or_else(|e| {
                                tracing::error!("failed to send Unmount response. error: {:?}", e);
                            });
                    }
                    Ok(fshost::AdminRequest::GetDevicePath { responder, .. }) => {
                        tracing::info!("admin get device path called");
                        responder
                            .send(&mut Err(zx::Status::NOT_SUPPORTED.into_raw()))
                            .unwrap_or_else(|e| {
                                tracing::error!(
                                    "failed to send GetDevicePath response. error: {:?}",
                                    e
                                );
                            });
                    }
                    Ok(fshost::AdminRequest::WriteDataFile { responder, payload, filename }) => {
                        tracing::info!(?filename, "admin write data file called");
                        let mut res = match write_data_file(&config, &launcher, &filename, payload)
                            .await
                        {
                            Ok(()) => Ok(()),
                            Err(e) => {
                                tracing::error!("admin service: write_data_file failed: {:?}", e);
                                Err(zx::Status::INTERNAL.into_raw())
                            }
                        };
                        responder.send(&mut res).unwrap_or_else(|e| {
                            tracing::error!(
                                "failed to send WriteDataFile response. error: {:?}",
                                e
                            );
                        });
                    }
                    Ok(fshost::AdminRequest::WipeStorage { responder, blobfs_root }) => {
                        tracing::info!("admin wipe storage called");
                        let pauser = pauser.clone();
                        let mut res = if !config.fvm_ramdisk {
                            tracing::error!(
                                "Can't WipeStorage from a non-recovery build; \
                                fvm_ramdisk must be set."
                            );
                            Err(zx::Status::NOT_SUPPORTED.into_raw())
                        } else {
                            match wipe_storage(&config, &launcher, pauser, blobfs_root).await {
                                Ok(()) => Ok(()),
                                Err(e) => {
                                    tracing::error!(?e, "admin service: wipe_storage failed");
                                    Err(zx::Status::INTERNAL.into_raw())
                                }
                            }
                        };
                        responder.send(&mut res).unwrap_or_else(|e| {
                            tracing::error!(?e, "failed to send WipeStorage response");
                        });
                    }
                    Ok(fshost::AdminRequest::ShredDataVolume { responder }) => {
                        tracing::info!("admin shred data volume called");
                        let mut res = match shred_data_volume(&config, &data_root).await {
                            Ok(()) => Ok(()),
                            Err(e) => {
                                debug_log(&format!(
                                    "admin service: shred_data_volume failed: {:?}",
                                    e
                                ));
                                Err(zx::Status::INTERNAL.into_raw())
                            }
                        };
                        responder.send(&mut res).unwrap_or_else(|e| {
                            tracing::error!(
                                "failed to send ShredDataVolume response. error: {:?}",
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

/// Create a new service node which implements the fuchsia.fshost.BlockWatcher protocol.
pub fn fshost_block_watcher(pauser: watcher::Watcher) -> Arc<service::Service> {
    service::host(move |mut stream: fshost::BlockWatcherRequestStream| {
        let mut pauser = pauser.clone();
        async move {
            while let Some(request) = stream.next().await {
                match request {
                    Ok(fshost::BlockWatcherRequest::Pause { responder }) => {
                        let res = match pauser.pause().await {
                            Ok(()) => zx::Status::OK.into_raw(),
                            Err(e) => {
                                tracing::error!("block watcher service: failed to pause: {:?}", e);
                                zx::Status::BAD_STATE.into_raw()
                            }
                        };
                        responder.send(res).unwrap_or_else(|e| {
                            tracing::error!("failed to send Pause response. error: {:?}", e);
                        });
                    }
                    Ok(fshost::BlockWatcherRequest::Resume { responder }) => {
                        let res = match pauser.resume().await {
                            Ok(()) => zx::Status::OK.into_raw(),
                            Err(e) => {
                                tracing::error!("block watcher service: failed to resume: {:?}", e);
                                zx::Status::BAD_STATE.into_raw()
                            }
                        };
                        responder.send(res).unwrap_or_else(|e| {
                            tracing::error!("failed to send Resume response. error: {:?}", e);
                        });
                    }
                    Err(e) => {
                        tracing::error!("block watcher server failed: {:?}", e);
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
            LifecycleRequestStream::from_channel(fasync::Channel::from_channel(handle.into())?);
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
