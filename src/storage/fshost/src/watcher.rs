// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::{BlockDevice, Device, NandDevice, StorageHostDevice};
use anyhow::{Context as _, Error};
use fuchsia_async as fasync;
use fuchsia_fs::directory::WatchEvent;
use futures::channel::mpsc;
use futures::lock::Mutex;
use futures::{stream, SinkExt, StreamExt};
use std::sync::Arc;

const DEV_CLASS_BLOCK: &'static str = "/dev/class/block";
const DEV_CLASS_NAND: &'static str = "/dev/class/nand";
const STORAGE_HOST_PARTITIONS_PATH: &'static str = "/partitions";

/// Generates a stream of block devices based off the path events we get from setting up a
/// directory watcher on this path. This will set up a new directory watcher every time stream is
/// called.
#[derive(Clone, Debug)]
struct PathSource {
    path: &'static str,
    source_type: PathSourceType,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum PathSourceType {
    Block,
    Nand,
    StorageHost,
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum StreamSettings {
    IgnoreExisting,
    YieldExisting,
}

impl PathSource {
    fn new(path: &'static str, source_type: PathSourceType) -> Self {
        PathSource { path, source_type }
    }

    /// Sets up a new directory watcher against the configured path and returns the stream of block
    /// devices found at that path. If [`settings`] is [`StreamSettings::IgnoreExisting`] then we
    /// skip [`WatchEvent::EXISTING`] events, and only add to the stream when new directory entries
    /// are added after this call.
    async fn into_stream(
        self,
        settings: StreamSettings,
    ) -> Result<stream::BoxStream<'static, Box<dyn Device>>, Error> {
        let path = self.path;
        let source_type = self.source_type;
        let dir_proxy = fuchsia_fs::directory::open_in_namespace_deprecated(
            path,
            fuchsia_fs::OpenFlags::empty(),
        )
        .with_context(|| format!("Failed to open directory at {path}"))?;
        let watcher = fuchsia_fs::directory::Watcher::new(&dir_proxy)
            .await
            .with_context(|| format!("Failed to watch {path}"))?;
        Ok(Box::pin(
            watcher
                .filter_map(|result| {
                    futures::future::ready({
                        match result {
                            Ok(message) => Some(message),
                            Err(error) => {
                                tracing::error!(?error, "fshost block watcher stream error");
                                None
                            }
                        }
                    })
                })
                .filter(|message| futures::future::ready(message.filename.as_os_str() != "."))
                .filter_map(move |fuchsia_fs::directory::WatchMessage { event, filename }| {
                    futures::future::ready({
                        let file_path = format!("{}/{}", path, filename.to_str().unwrap());
                        match event {
                            WatchEvent::ADD_FILE => Some(file_path),
                            WatchEvent::EXISTING => {
                                if settings == StreamSettings::IgnoreExisting {
                                    None
                                } else {
                                    Some(file_path)
                                }
                            }
                            _ => None,
                        }
                    })
                })
                .filter_map(move |path| async move {
                    match source_type {
                        PathSourceType::Block => {
                            BlockDevice::new(path).await.map(|d| Box::new(d) as Box<dyn Device>)
                        }
                        PathSourceType::Nand => {
                            NandDevice::new(path).await.map(|d| Box::new(d) as Box<dyn Device>)
                        }
                        PathSourceType::StorageHost => {
                            StorageHostDevice::new(path).map(|d| Box::new(d) as Box<dyn Device>)
                        }
                    }
                    .map_err(|e| {
                        tracing::warn!("Failed to create device (maybe it went away?): {:?}", e);
                        e
                    })
                    .ok()
                }),
        ))
    }
}

/// Watcher generates new [`BlockDevice`]s for fshost to process.
pub struct Watcher {
    _device_tx: Arc<Mutex<mpsc::UnboundedSender<Box<dyn Device>>>>,
    // Each source has its own Task, and they all feed into _device_tx.
    _tasks: Vec<fasync::Task<()>>,
}

impl Watcher {
    /// Create a new Watcher and BlockDevice stream. The watcher will start watching
    /// /dev/class/block immediately, initially populating the stream with any entries which are
    /// already there, then sending new items on the stream as they are added to the directory.
    pub async fn new(
        config: &fshost_config::Config,
    ) -> Result<(Self, impl futures::Stream<Item = Box<dyn Device>>), Error> {
        let sources = if config.storage_host {
            vec![
                PathSource::new(DEV_CLASS_BLOCK, PathSourceType::Block),
                PathSource::new(DEV_CLASS_NAND, PathSourceType::Nand),
                PathSource::new(STORAGE_HOST_PARTITIONS_PATH, PathSourceType::StorageHost),
            ]
        } else {
            vec![
                PathSource::new(DEV_CLASS_BLOCK, PathSourceType::Block),
                PathSource::new(DEV_CLASS_NAND, PathSourceType::Nand),
            ]
        };
        Self::new_with_sources(sources).await
    }

    async fn new_with_sources(
        sources: Vec<PathSource>,
    ) -> Result<(Self, impl futures::Stream<Item = Box<dyn Device>>), Error> {
        let (device_tx, device_rx) = mpsc::unbounded();
        let device_tx = Arc::new(Mutex::new(device_tx));

        let mut tasks = vec![];
        for source in sources.into_iter() {
            tasks.push(fasync::Task::spawn(Self::process_one_stream(
                source.into_stream(StreamSettings::YieldExisting).await?,
                device_tx.clone(),
            )));
        }

        Ok((Watcher { _device_tx: device_tx, _tasks: tasks }, device_rx))
    }

    async fn process_one_stream(
        mut device_stream: stream::BoxStream<'static, Box<dyn Device>>,
        device_tx: Arc<Mutex<mpsc::UnboundedSender<Box<dyn Device>>>>,
    ) {
        while let Some(device) = device_stream.next().await {
            device_tx.lock().await.send(device).await.expect("failed to send device");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PathSource, PathSourceType, Watcher};
    use fidl_fuchsia_device::{ControllerRequest, ControllerRequestStream};
    use fidl_fuchsia_io as fio;
    use futures::StreamExt;
    use std::sync::Arc;
    use vfs::directory::entry_container::Directory;
    use vfs::directory::helper::DirectlyMutable;
    use vfs::execution_scope::ExecutionScope;
    use vfs::path::Path;
    use vfs::service;

    pub fn fshost_controller(path: &'static str) -> Arc<service::Service> {
        service::host(move |mut stream: ControllerRequestStream| async move {
            while let Some(request) = stream.next().await {
                match request {
                    Ok(ControllerRequest::GetTopologicalPath { responder, .. }) => {
                        responder.send(Ok(path)).unwrap_or_else(|e| {
                            tracing::error!(
                                "failed to send GetTopologicalPath response. error: {:?}",
                                e
                            );
                        });
                    }
                    Ok(ControllerRequest::ConnectToDeviceFidl { .. }) => {}
                    Ok(controller_request) => {
                        panic!("unexpected request: {:?}", controller_request);
                    }
                    Err(error) => {
                        panic!("controller server failed: {}", error);
                    }
                }
            }
        })
    }

    #[fuchsia::test]
    async fn watcher_populates_device_stream() {
        // Start with a couple of devices
        let block = vfs::pseudo_directory! {
            "000" => vfs::pseudo_directory! {
                "device_controller" => fshost_controller("block-000"),
            },
            "001" => vfs::pseudo_directory! {
                "device_controller" => fshost_controller("block-001"),
            },
        };

        let nand = vfs::pseudo_directory! {
            "000" => vfs::pseudo_directory! {
                "device_controller" => fshost_controller("nand-000"),
            },
            "001" => vfs::pseudo_directory! {
                "device_controller" => fshost_controller("nand-001"),
            },
        };

        let class_block_and_nand = vfs::pseudo_directory! {
            "class" => vfs::pseudo_directory! {
                "block" => block.clone(),
                "nand" => nand.clone(),
            },
        };

        let (client, server) = fidl::endpoints::create_endpoints();
        let scope = ExecutionScope::new();
        class_block_and_nand.open(
            scope.clone(),
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY,
            Path::dot(),
            fidl::endpoints::ServerEnd::new(server.into_channel()),
        );

        {
            let ns = fdio::Namespace::installed().expect("failed to get installed namespace");
            ns.bind("/test-dev", client).expect("failed to bind dev in namespace");
        }

        let (_watcher, mut device_stream) = Watcher::new_with_sources(vec![
            PathSource { path: "/test-dev/class/block", source_type: PathSourceType::Block },
            PathSource { path: "/test-dev/class/nand", source_type: PathSourceType::Nand },
        ])
        .await
        .expect("failed to make watcher");

        let mut devices =
            std::collections::HashSet::from(["block-000", "block-001", "nand-000", "nand-001"]);

        // There are four devices that were added before we started watching.
        assert!(devices.remove(device_stream.next().await.unwrap().topological_path()));
        assert!(devices.remove(device_stream.next().await.unwrap().topological_path()));
        assert!(devices.remove(device_stream.next().await.unwrap().topological_path()));
        assert!(devices.remove(device_stream.next().await.unwrap().topological_path()));
        assert!(devices.is_empty());

        // Removing an entry for a device already taken off the stream doesn't do anything.
        assert!(block
            .remove_entry("001", false)
            .expect("failed to remove dir entry 001")
            .is_some());

        // Adding an entry generates a new block device.
        block
            .add_entry(
                "002",
                vfs::pseudo_directory! {
                    "device_controller" => fshost_controller("block-002"),
                },
            )
            .expect("failed to add dir entry 002");

        assert_eq!(device_stream.next().await.unwrap().topological_path(), "block-002");
    }
}
