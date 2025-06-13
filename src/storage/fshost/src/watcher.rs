// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::{BlockDevice, Device, NandDevice, Parent, VolumeProtocolDevice};
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
                // TODO(https://fxbug.dev/422230360): This is probably worth using error for, but
                // sometimes in tests the gpt2 watcher stream gets closed while we are still trying
                // to read watch messages, which was causing flakes. Upgrade again once the flake
                // is addressed.
                log::warn!(error:?; "fshost block watcher stream error");
                None
            }
            _ => None,
        })
    }))
}

pub type GetParentCallback = Arc<dyn Fn(&str) -> Parent + Send + Sync>;

/// An implementation of `WatchSource` based on a path in the local namespace.
pub struct PathSource {
    path: &'static str,
    source_type: PathSourceType,
    // TODO(https://fxbug.dev/394970436): once storage-host is enabled on all products, we can
    // remove this side-channel check. For non-storage-host configurations, once we find the block
    // device that represents the SystemPartitionTable, we need to configure its children, based on
    // the path prefix, as having the SystemPartitionTable parent instead of the Dev parent. Post
    // storage-host, they are separate sources, and thus static.
    get_parent: Option<GetParentCallback>,
}

#[derive(Copy, Clone, Debug)]
pub enum PathSourceType {
    Block,
    Nand,
}

impl PathSource {
    pub fn new(
        path: &'static str,
        source_type: PathSourceType,
        get_parent: Option<GetParentCallback>,
    ) -> Self {
        PathSource { path, source_type, get_parent }
    }
}

#[async_trait]
impl WatchSource for PathSource {
    async fn as_stream(&mut self) -> Result<stream::BoxStream<'static, Box<dyn Device>>, Error> {
        let path = self.path;
        let source_type = self.source_type;
        let get_parent = self.get_parent.clone();
        let dir_proxy = fuchsia_fs::directory::open_in_namespace(path, fio::Flags::empty())
            .with_context(|| format!("Failed to open directory at {path}"))?;
        let watcher = fuchsia_fs::directory::Watcher::new(&dir_proxy)
            .await
            .with_context(|| format!("Failed to watch {path}"))?;
        Ok(Box::pin(common_filters(watcher).filter_map(move |filename| {
            let get_parent = get_parent.clone();
            async move {
                let path = format!("{}/{}", path, filename);
                match source_type {
                    PathSourceType::Block => {
                        let mut device = BlockDevice::new(path)
                            .await
                            .inspect_err(|e| {
                                log::warn!(
                                    "Failed to create device (maybe it went away?): {:?}",
                                    e
                                );
                            })
                            .ok()?;
                        if let Some(get_parent) = get_parent {
                            device.set_parent(get_parent(device.topological_path()))
                        }
                        Some(Box::new(device) as Box<dyn Device>)
                    }
                    PathSourceType::Nand => NandDevice::new(path)
                        .await
                        .map(|d| Box::new(d) as Box<dyn Device>)
                        .inspect_err(|e| {
                            log::warn!("Failed to create device (maybe it went away?): {:?}", e);
                        })
                        .ok(),
                }
            }
        })))
    }
}

/// An implementation of `WatchSource` based on a DirectoryProxy.  The source is expected to be
/// a directory containing a "volume" node which implements fuchsia.hardware.block.volume.Volume.
#[derive(Clone, Debug)]
pub struct DirSource {
    dir: fio::DirectoryProxy,
    // The name of the source of these devices, for example the moniker of a component providing
    // them. Used for logging and debugging.
    source: String,
    // The parent to set for these devices.
    parent: Parent,
}

impl DirSource {
    /// Creates a `DirSource` that connects a `VolumeProtocolDevice` to each entry in `dir`.
    pub fn new(dir: fio::DirectoryProxy, source: impl ToString, parent: Parent) -> Self {
        Self { dir, source: source.to_string(), parent }
    }
}

#[async_trait]
impl WatchSource for DirSource {
    async fn as_stream(&mut self) -> Result<stream::BoxStream<'static, Box<dyn Device>>, Error> {
        let watcher = fuchsia_fs::directory::Watcher::new(&self.dir)
            .await
            .with_context(|| format!("Failed to watch dir"))?;
        let dir = Arc::new(fuchsia_fs::directory::clone(&self.dir)?);
        let source = self.source.clone();
        let parent = self.parent;
        Ok(Box::pin(common_filters(watcher).filter_map(move |filename| {
            let dir = dir.clone();
            let source = source.clone();
            async move {
                let dir_clone = fuchsia_fs::directory::clone(&dir)
                    .map_err(|err| log::warn!(err:?; "Failed to clone dir"))
                    .ok()?;
                VolumeProtocolDevice::new(dir_clone, filename, source, parent)
                    .map(|d| Box::new(d) as Box<dyn Device>)
                    .map_err(|err| {
                        log::warn!(err:?; "Failed to create device (maybe it went away?)");
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
            if let Err(error) = device_tx.send(device).await {
                log::warn!(error:?; "Failed to send device");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DirSource, PathSource, PathSourceType, Watcher};
    use crate::device::Parent;
    use fidl::endpoints::Proxy as _;
    use fidl_fuchsia_device::{ControllerRequest, ControllerRequestStream};
    use fidl_fuchsia_hardware_block_volume::VolumeRequestStream;
    use futures::StreamExt;
    use std::sync::Arc;
    use vfs::directory::helper::DirectlyMutable;
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
                            log::error!(
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

        let client =
            vfs::directory::serve_read_only(class_block_and_nand).into_client_end().unwrap();

        {
            let ns = fdio::Namespace::installed().expect("failed to get installed namespace");
            ns.bind("/test-dev", client).expect("failed to bind dev in namespace");
        }

        let client = vfs::directory::serve_read_only(partitions_dir);
        let (_watcher, mut device_stream) = Watcher::new(vec![
            Box::new(PathSource::new("/test-dev/class/block", PathSourceType::Block, None)),
            Box::new(PathSource::new("/test-dev/class/nand", PathSourceType::Nand, None)),
            Box::new(DirSource::new(client, "test-dir-source", Parent::Dev)),
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

        let client =
            vfs::directory::serve_read_only(class_block_and_nand).into_client_end().unwrap();

        {
            let ns = fdio::Namespace::installed().expect("failed to get installed namespace");
            ns.bind("/test-dev", client).expect("failed to bind dev in namespace");
        }

        let (mut watcher, mut device_stream) = Watcher::new(vec![Box::new(PathSource::new(
            "/test-dev/class/block",
            PathSourceType::Block,
            None,
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
            .add_source(Box::new(PathSource::new(
                "/test-dev/class/nand",
                PathSourceType::Nand,
                None,
            )))
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
