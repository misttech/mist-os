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
    anyhow::{anyhow, Context, Error},
    fidl::endpoints::RequestStream,
    fidl_fuchsia_fshost as fshost,
    fidl_fuchsia_hardware_block::BlockMarker,
    fidl_fuchsia_io::OpenFlags,
    fidl_fuchsia_process_lifecycle::{LifecycleRequest, LifecycleRequestStream},
    fs_management::{
        format::DiskFormat,
        partition::{find_partition, PartitionMatcher},
        F2fs, Fxfs, Minfs,
    },
    fuchsia_async as fasync,
    fuchsia_component::client::connect_to_protocol_at_path,
    fuchsia_fs::{
        directory::{create_directory_recursive, open_file},
        file::write,
    },
    fuchsia_runtime::HandleType,
    fuchsia_zircon::{self as zx, Duration},
    futures::{channel::mpsc, StreamExt, TryStreamExt},
    remote_block_device::{BlockClient, BufferSlice, RemoteBlockClient},
    std::sync::Arc,
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
        DiskFormat::Fxfs => {
            launcher.serve_data(&mut device, Fxfs::dynamic_child(), inside_zxcrypt).await?
        }
        DiskFormat::F2fs => {
            launcher.serve_data(&mut device, F2fs::dynamic_child(), inside_zxcrypt).await?
        }
        DiskFormat::Minfs => {
            launcher.serve_data(&mut device, Minfs::dynamic_child(), inside_zxcrypt).await?
        }
        _ => unreachable!(),
    };

    let data_root = filesystem.root().context("Failed to get data root")?;
    let (directory_path, relative_file_path) =
        filename.rsplit_once("/").ok_or(anyhow!("There is no backslash in the file path"))?;
    let directory_proxy = create_directory_recursive(
        &data_root,
        directory_path,
        OpenFlags::CREATE | OpenFlags::RIGHT_READABLE | OpenFlags::RIGHT_WRITABLE,
    )
    .await
    .context("Failed to create directory")?;

    let file_proxy = open_file(
        &directory_proxy,
        relative_file_path,
        OpenFlags::CREATE | OpenFlags::RIGHT_READABLE | OpenFlags::RIGHT_WRITABLE,
    )
    .await
    .context("Failed to open file")?;

    let mut data = vec![0; content_size];
    payload.read(&mut data, 0)?;
    write(&file_proxy, &data).await?;

    filesystem.shutdown().await?;
    return Ok(());
}

async fn shred_data_volume(config: &fshost_config::Config) -> Result<(), zx::Status> {
    if config.data_filesystem_format != "fxfs" {
        return Err(zx::Status::NOT_SUPPORTED);
    }
    // If we expect Fxfs to be live, just erase the key bag.
    if config.data && !config.fvm_ramdisk {
        std::fs::remove_file(KEY_BAG_FILE)?;

        debug_log("Erased key bag");
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
) -> Arc<service::Service> {
    service::host(move |mut stream: fshost::AdminRequestStream| {
        let config = config.clone();
        let launcher = launcher.clone();
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
                    Ok(fshost::AdminRequest::WipeStorage { responder, .. }) => {
                        tracing::info!("admin wipe storage called");
                        responder
                            .send(&mut Err(zx::Status::NOT_SUPPORTED.into_raw()))
                            .unwrap_or_else(|e| {
                                tracing::error!(
                                    "failed to send WipeStorage response. error: {:?}",
                                    e
                                );
                            });
                    }
                    Ok(fshost::AdminRequest::ShredDataVolume { responder }) => {
                        tracing::info!("admin shred data volume called");
                        let mut res = match shred_data_volume(&config).await {
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
