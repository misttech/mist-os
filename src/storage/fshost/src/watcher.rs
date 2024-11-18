// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::{BlockDevice, Device, NandDevice, VolumeProtocolDevice};
use anyhow::{Context as _, Error};
use async_trait::async_trait;
use fuchsia_fs::directory::{WatchEvent, WatchMessage};
use futures::channel::mpsc;
use futures::{stream, SinkExt, StreamExt};
use std::future::ready;
use std::sync::Arc;
use {fidl_fuchsia_io as fio, fuchsia_async as fasync};

/// A provider for a stream of block devices.
#[async_trait]
pub trait WatchSource: Send + Sync + 'static {
    /// Generates a new stream of block devices.
    async fn as_stream(&mut self) -> Result<stream::BoxStream<'static, Box<dyn Device>>, Error>;
}

fn common_filters(watcher: fuchsia_fs::directory::Watcher) -> stream::BoxStream<'static, String> {
    Box::pin(watcher.filter_map(|result| {
        ready(match result {
            Ok(WatchMessage { event: WatchEvent::ADD_FILE | WatchEvent::EXISTING, filename })
                if filename.as_os_str() != "." =>
            {
                Some(filename.to_str().unwrap().to_owned())
            }
            Err(error) => {
                tracing::error!(?error, "fshost block watcher stream error");
                None
            }
            _ => None,
        })
    }))
}

/// An implementation of `WatchSource` based on a path in the local namespace.
#[derive(Clone, Debug)]
pub struct PathSource {
    path: &'static str,
    source_type: PathSourceType,
}

#[derive(Copy, Clone, Debug)]
pub enum PathSourceType {
    Block,
    Nand,
}

impl PathSource {
    pub fn new(path: &'static str, source_type: PathSourceType) -> Self {
        PathSource { path, source_type }
    }
}

#[async_trait]
impl WatchSource for PathSource {
    async fn as_stream(&mut self) -> Result<stream::BoxStream<'static, Box<dyn Device>>, Error> {
        let path = self.path;
        let source_type = self.source_type;
        let dir_proxy = fuchsia_fs::directory::open_in_namespace(path, fio::Flags::empty())
            .with_context(|| format!("Failed to open directory at {path}"))?;
        let watcher = fuchsia_fs::directory::Watcher::new(&dir_proxy)
            .await
            .with_context(|| format!("Failed to watch {path}"))?;
        Ok(Box::pin(common_filters(watcher).filter_map(move |filename| async move {
            let path = format!("{}/{}", path, filename);
            match source_type {
                PathSourceType::Block => {
                    BlockDevice::new(path).await.map(|d| Box::new(d) as Box<dyn Device>)
                }
                PathSourceType::Nand => {
                    NandDevice::new(path).await.map(|d| Box::new(d) as Box<dyn Device>)
                }
            }
            .map_err(|e| {
                tracing::warn!("Failed to create device (maybe it went away?): {:?}", e);
                e
            })
            .ok()
        })))
    }
}

/// An implementation of `WatchSource` based on a DirectoryProxy.  The source is expected to be
/// a directory containing a "volume" node which implements fuchsia.hardware.block.volume.Volume.
#[derive(Clone, Debug)]
pub struct DirSource {
    dir: fio::DirectoryProxy,
}

impl DirSource {
    /// Creates a `DirSource` that connects a `VolumeProtocolDevice` to each entry in `dir`.
    pub fn new(dir: fio::DirectoryProxy) -> Self {
        Self { dir }
    }
}

#[async_trait]
impl WatchSource for DirSource {
    async fn as_stream(&mut self) -> Result<stream::BoxStream<'static, Box<dyn Device>>, Error> {
        let watcher = fuchsia_fs::directory::Watcher::new(&self.dir)
            .await
            .with_context(|| format!("Failed to watch dir"))?;
        let dir = Arc::new(fuchsia_fs::directory::clone(&self.dir)?);
        Ok(Box::pin(common_filters(watcher).filter_map(move |filename| {
            let dir = dir.clone();
            async move {
                let dir_clone = fuchsia_fs::directory::clone(&dir)
                    .map_err(|err| tracing::warn!(?err, "Failed to clone dir"))
                    .ok()?;
                VolumeProtocolDevice::new(dir_clone, filename)
                    .map(|d| Box::new(d) as Box<dyn Device>)
                    .map_err(|err| {
                        tracing::warn!(?err, "Failed to create device (maybe it went away?)");
                        err
                    })
                    .ok()
            }
        })))
    }
}

/// Watcher generates new [`BlockDevice`]s for fshost to process.
pub struct Watcher {
    device_tx: mpsc::UnboundedSender<Box<dyn Device>>,
    // Each source has its own Task, and they all feed into _device_tx.
    tasks: Vec<fasync::Task<()>>,
}

impl Watcher {
    /// Create a new Watcher and BlockDevice stream. The watcher will start watching `sources`
    /// initially, populating the stream with any entries which are already there, then sending new
    /// items on the stream as they are added to the directory.
    pub async fn new(
        sources: Vec<Box<dyn WatchSource>>,
    ) -> Result<(Self, impl futures::Stream<Item = Box<dyn Device>>), Error> {
        let (device_tx, device_rx) = mpsc::unbounded();

        let mut this = Watcher { device_tx, tasks: vec![] };
        for source in sources.into_iter() {
            this.add_source(source).await?;
        }

        Ok((this, device_rx))
    }

    pub async fn add_source(&mut self, mut source: Box<dyn WatchSource>) -> Result<(), Error> {
        self.tasks.push(fasync::Task::spawn(Self::process_one_stream(
            source.as_stream().await?,
            self.device_tx.clone(),
        )));
        Ok(())
    }

    async fn process_one_stream(
        mut device_stream: stream::BoxStream<'static, Box<dyn Device>>,
        mut device_tx: mpsc::UnboundedSender<Box<dyn Device>>,
    ) {
        while let Some(device) = device_stream.next().await {
            device_tx.send(device).await.expect("failed to send device");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DirSource, PathSource, PathSourceType, Watcher};
    use fidl_fuchsia_device::{ControllerRequest, ControllerRequestStream};
    use fidl_fuchsia_hardware_block_volume::VolumeRequestStream;
    use fidl_fuchsia_io as fio;
    use futures::StreamExt;
    use std::sync::Arc;
    use vfs::directory::entry_container::Directory;
    use vfs::directory::helper::DirectlyMutable;
    use vfs::execution_scope::ExecutionScope;
    use vfs::path::Path;
    use vfs::service;

    pub fn volume_service() -> Arc<service::Service> {
        service::host(move |mut stream: VolumeRequestStream| async move {
            if let Some(request) = stream.next().await {
                // The service never actually gets used in the tests.
                panic!("Unexpected request {request:?}");
            }
        })
    }

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

        let partitions_dir = vfs::pseudo_directory! {
            "000" => vfs::pseudo_directory! {
                "volume" => volume_service(),
            },
            "001" => vfs::pseudo_directory! {
                "volume" => volume_service(),
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

        let (client, server) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
        partitions_dir.clone().open(
            scope.clone(),
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY,
            Path::dot(),
            fidl::endpoints::ServerEnd::new(server.into_channel()),
        );

        let (_watcher, mut device_stream) = Watcher::new(vec![
            Box::new(PathSource::new("/test-dev/class/block", PathSourceType::Block)),
            Box::new(PathSource::new("/test-dev/class/nand", PathSourceType::Nand)),
            Box::new(DirSource::new(client)),
        ])
        .await
        .expect("failed to make watcher");

        let expected_devices = std::collections::HashSet::from([
            "block-000".to_string(),
            "block-001".to_string(),
            "nand-000".to_string(),
            "nand-001".to_string(),
            "000/volume".to_string(),
            "001/volume".to_string(),
        ]);
        let mut devices = std::collections::HashSet::new();
        for _ in 0..expected_devices.len() {
            devices.insert(device_stream.next().await.unwrap().topological_path().to_string());
        }
        assert_eq!(devices, expected_devices);

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

    #[fuchsia::test]
    async fn add_stream() {
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

        let (mut watcher, mut device_stream) = Watcher::new(vec![Box::new(PathSource::new(
            "/test-dev/class/block",
            PathSourceType::Block,
        ))])
        .await
        .expect("failed to make watcher");

        let mut devices = std::collections::HashSet::from(["block-000", "block-001"]);

        // There are two devices that were added before we started watching.
        assert!(devices.remove(device_stream.next().await.unwrap().topological_path()));
        assert!(devices.remove(device_stream.next().await.unwrap().topological_path()));
        assert!(devices.is_empty());

        // Existing entries in the new source are yielded immediately
        watcher
            .add_source(Box::new(PathSource::new("/test-dev/class/nand", PathSourceType::Nand)))
            .await
            .expect("failed to add_source");

        let mut devices = std::collections::HashSet::from(["nand-000", "nand-001"]);
        assert!(devices.remove(device_stream.next().await.unwrap().topological_path()));
        assert!(devices.remove(device_stream.next().await.unwrap().topological_path()));
        assert!(devices.is_empty());

        // And now the directories are both watched as expected
        nand.add_entry(
            "002",
            vfs::pseudo_directory! {
                "device_controller" => fshost_controller("nand-002"),
            },
        )
        .expect("failed to add dir entry 002");

        assert_eq!(device_stream.next().await.unwrap().topological_path(), "nand-002");

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
