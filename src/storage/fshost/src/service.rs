// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        crypt::zxcrypt::{UnsealOutcome, ZxcryptDevice},
        debug_log,
        device::{constants, BlockDevice, Device},
        environment::{Environment, FilesystemLauncher, ServeFilesystemStatus},
        fxblob,
        service::constants::{
            DATA_PARTITION_LABEL, LEGACY_DATA_PARTITION_LABEL, ZXCRYPT_DRIVER_PATH,
        },
        watcher,
    },
    anyhow::{anyhow, Context, Error},
    fidl::endpoints::{Proxy, RequestStream, ServerEnd},
    fidl_fuchsia_device::ControllerProxy,
    fidl_fuchsia_fshost as fshost,
    fidl_fuchsia_hardware_block::{BlockMarker, BlockProxy},
    fidl_fuchsia_hardware_block_volume::VolumeManagerMarker,
    fidl_fuchsia_io::{DirectoryMarker, OpenFlags},
    fidl_fuchsia_process_lifecycle::{LifecycleRequest, LifecycleRequestStream},
    fs_management::{
        filesystem,
        format::DiskFormat,
        partition::{find_partition, fvm_allocate_partition, PartitionMatcher},
        Blobfs, F2fs, Fxfs, Minfs,
    },
    fuchsia_async as fasync,
    fuchsia_fs::{
        directory::{clone_onto_no_describe, create_directory_recursive, open_file},
        file::write,
    },
    fuchsia_runtime::HandleType,
    fuchsia_zircon::{self as zx, sys::zx_handle_t, zx_status_t, AsHandleRef, Duration},
    futures::{channel::mpsc, lock::Mutex, StreamExt, TryStreamExt},
    remote_block_device::{BlockClient, BufferSlice, RemoteBlockClient},
    std::{collections::HashSet, sync::Arc},
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

    let data_matcher = PartitionMatcher {
        type_guids: Some(vec![constants::DATA_TYPE_GUID]),
        labels: Some(data_partition_names()),
        parent_device: Some(fvm_path),
        ignore_if_path_contains: Some("zxcrypt/unsealed".to_string()),
        ..Default::default()
    };

    find_partition(data_matcher, FIND_PARTITION_DURATION)
        .await
        .context("Failed to find data partition")
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

// TODO(https://fxbug.dev/122942) Add support for fxblob.
async fn wipe_storage(
    config: &fshost_config::Config,
    ramdisk_prefix: Option<String>,
    launcher: &FilesystemLauncher,
    ignored_paths: &mut HashSet<String>,
    blobfs_root: ServerEnd<DirectoryMarker>,
) -> Result<(), Error> {
    tracing::info!("Searching for block device with FVM");

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
    let (block_proxy, server_end) = fidl::endpoints::create_proxy::<BlockMarker>()?;
    fvm_controller
        .connect_to_device_fidl(server_end.into_channel())
        .context("connecting to block protocol")?;
    initialize_fvm(config.fvm_slice_size, &block_proxy)?;

    tracing::info!("Binding and waiting for FVM driver.");
    fvm_controller.bind(constants::FVM_DRIVER_PATH).await?.map_err(zx::Status::from_raw)?;

    let fvm_dir = fuchsia_fs::directory::open_in_namespace(&fvm_path, OpenFlags::empty())
        .context("Failed to open the fvm directory")?;

    let fvm_volume_manager_proxy =
        device_watcher::recursive_wait_and_open::<VolumeManagerMarker>(&fvm_dir, "fvm")
            .await
            .context("waiting for FVM driver")?;

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
        allow_delivery_blobs: config.blobfs_allow_delivery_blobs,
        ..launcher.get_blobfs_config()
    };
    if config.blobfs_initial_inodes > 0 {
        blobfs_config.num_inodes = config.blobfs_initial_inodes;
    }

    let mut blobfs = filesystem::Filesystem::new(blobfs_controller, blobfs_config);
    blobfs.format().await.context("Failed to format blobfs")?;
    let started_blobfs = blobfs.serve().await.context("serving blobfs")?;
    clone_onto_no_describe(started_blobfs.root(), None, blobfs_root)?;
    // We use forget() here to ensure that our Blobfs instance is not dropped, which would cause the
    // filesystem to shutdown.
    std::mem::forget(started_blobfs);
    Ok(())
}

async fn write_data_file(
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
        fxblob::mount_or_format_data(ramdisk_prefix, launcher)
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
) -> Result<(), zx::Status> {
    if config.data_filesystem_format != "fxfs" {
        return Err(zx::Status::NOT_SUPPORTED);
    }
    // If we expect Fxfs to be live, ask `environment` to shred the data volume.
    if config.data && !config.ramdisk_image {
        environment.lock().await.shred_data().await.map_err(|err| {
            debug_log(&format!("Failed to shred data: {:?}", err));
            zx::Status::INTERNAL
        })?;
    } else {
        // Otherwise we need to find the Fxfs partition and shred it.
        // TODO(https://fxbug.dev/122940) Add support for fxblob.
        let partition_controller =
            find_data_partition(ramdisk_prefix).await.map_err(|_| zx::Status::NOT_FOUND)?;
        let (block_proxy, block_server_end) =
            fidl::endpoints::create_proxy::<BlockMarker>().map_err(|_| zx::Status::INTERNAL)?;
        partition_controller
            .connect_to_device_fidl(block_server_end.into_channel())
            .map_err(|_| zx::Status::INTERNAL)?;

        let block_client =
            RemoteBlockClient::new(block_proxy).await.map_err(|_| zx::Status::INTERNAL)?;

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
                    Ok(fshost::AdminRequest::WipeStorage { responder, blobfs_root }) => {
                        tracing::info!("admin wipe storage called");
                        let mut ignored_paths = matcher_lock.lock().await;
                        let res = if !config.ramdisk_image {
                            tracing::error!(
                                "Can't WipeStorage from a non-recovery build; \
                                ramdisk_image must be set."
                            );
                            Err(zx::Status::NOT_SUPPORTED.into_raw())
                        } else {
                            match wipe_storage(
                                &config,
                                ramdisk_prefix.clone(),
                                &launcher,
                                &mut *ignored_paths,
                                blobfs_root,
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
                        let res =
                            match shred_data_volume(&env, &config, ramdisk_prefix.clone()).await {
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
