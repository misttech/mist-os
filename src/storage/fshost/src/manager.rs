// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::Device;
use crate::environment::Environment;
use crate::{matcher, service};
use anyhow::{format_err, Error};
use fidl::endpoints::DiscoverableProtocolMarker as _;
use fidl_fuchsia_hardware_block_volume::VolumeMarker;
use fs_management::filesystem::BlockConnector;
use fs_management::format::DiskFormat;
use futures::channel::mpsc;
use futures::lock::Mutex;
use futures::StreamExt;
use std::collections::HashSet;
use std::sync::Arc;
use vfs::directory::helper::DirectlyMutable;
use vfs::directory::simple::Simple;
use vfs::service::endpoint;

pub struct DevicePublisher {
    debug_block_dir: Arc<Simple>,
    block_dir: Arc<Simple>,
}

impl DevicePublisher {
    pub fn new() -> Self {
        Self { debug_block_dir: vfs::pseudo_directory! {}, block_dir: vfs::pseudo_directory! {} }
    }

    /// Publishes *all* block devices.  Only suitable for routing outside of fshost on eng
    /// configurations.
    pub fn debug_block_dir(&self) -> Arc<Simple> {
        self.debug_block_dir.clone()
    }

    /// Publishes block devices which are not already managed by fshost (i.e. block devices from the
    /// low-level storage drivers).
    pub fn block_dir(&self) -> Arc<Simple> {
        self.block_dir.clone()
    }

    fn publish_to_debug_block_dir(&self, device: &dyn Device, name: &str) -> Result<(), Error> {
        let volume = device.block_connector()?;
        self.debug_block_dir.add_entry(
            name,
            vfs::pseudo_directory! {
                VolumeMarker::PROTOCOL_NAME => endpoint(move |_scope, channel| {
                    volume.connect_channel_to_volume(channel.into_zx_channel().into())
                        .unwrap_or_else(|error| {
                            log::error!(error:%; "failed to open volume");
                        });
                }),
                "source" => vfs::file::read_only(device.source()),
            },
        )?;
        Ok(())
    }

    fn publish(&self, volume: Box<dyn BlockConnector>, name: &str) -> Result<(), Error> {
        self.block_dir.add_entry(
            name,
            vfs::pseudo_directory! {
                VolumeMarker::PROTOCOL_NAME => endpoint(move |_scope, channel| {
                    volume.connect_channel_to_volume(channel.into_zx_channel().into())
                        .unwrap_or_else(|error| {
                            log::error!(error:%; "failed to open volume");
                        });
                }),
            },
        )?;
        Ok(())
    }
}

pub struct Manager {
    matcher: matcher::Matchers,
    environment: Arc<Mutex<dyn Environment>>,
    /// Holds a set of topological paths that have already been processed and
    /// should be ignored when matching. When matched, the ignored paths are removed from the set.
    /// (i.e. The device is ignored only once.)
    matcher_lock: Arc<Mutex<HashSet<String>>>,
    device_publisher: DevicePublisher,
}

impl Manager {
    pub fn new(
        config: &fshost_config::Config,
        environment: Arc<Mutex<dyn Environment>>,
        matcher_lock: Arc<Mutex<HashSet<String>>>,
        device_publisher: DevicePublisher,
    ) -> Self {
        Manager {
            matcher: matcher::Matchers::new(config),
            environment,
            matcher_lock,
            device_publisher,
        }
    }

    /// The main loop of fshost. Watch for new devices, match them against filesystems we expect,
    /// and then launch them appropriately.
    pub async fn device_handler(
        &mut self,
        device_stream: impl futures::Stream<Item = Box<dyn Device>>,
        mut shutdown_rx: mpsc::Receiver<service::FshostShutdownResponder>,
    ) -> Result<service::FshostShutdownResponder, Error> {
        let mut device_stream = Box::pin(device_stream).fuse();
        let mut ignored_paths = HashSet::new();
        let mut block_index = 0;
        loop {
            let name = format!("{:03}", block_index);
            block_index += 1;
            // Wait for the next device to come in, or the shutdown signal to arrive.
            let mut device = futures::select! {
                responder = shutdown_rx.next() => {
                    let responder = responder
                        .ok_or_else(|| format_err!("shutdown signal stream ended unexpectedly"))?;
                    return Ok(responder);
                },
                maybe_device = device_stream.next() => {
                    if let Some(device) = maybe_device {
                        device
                    } else {
                        anyhow::bail!("block watcher returned none unexpectedly");
                    }
                },
            };

            for path in (*self.matcher_lock.lock().await).drain() {
                ignored_paths.insert(path);
            }
            let topological_path = device.topological_path().to_string();
            if ignored_paths.remove(&topological_path) {
                log::info!(
                    topological_path = topological_path.as_str();
                    "Skipping explicitly ignored device."
                );
                continue;
            }

            let content_format = device.content_format().await.unwrap_or(DiskFormat::Unknown);
            let label = device.partition_label().await.ok().map(|s| s.to_string());
            let type_guid =
                device.partition_type().await.ok().map(|guid| uuid::Uuid::from_bytes(guid.clone()));
            let is_managed = device.is_managed();
            let connector = device.block_connector().ok();
            log::info!(
                source:% = device.source(),
                path:% = device.path(),
                content_format:?,
                label:?,
                type_guid:?,
                is_ramdisk = device.is_fshost_ramdisk();
                "Matching device"
            );
            // Publish devices we find as needed.
            // If we get an error from this just log a warning and continue.
            if let Err(error) =
                self.device_publisher.publish_to_debug_block_dir(device.as_ref(), &name)
            {
                log::warn!(error:?; "Failed to publish block device");
            }

            let device_path = device.path().to_string();
            match self.matcher.match_device(device, &mut *self.environment.lock().await).await {
                Ok(was_matched) => {
                    if !was_matched {
                        log::info!(path:% = device_path; "ignored");
                    }
                    if let Some(connector) = connector {
                        if !is_managed && !was_matched {
                            log::info!("Publishing {device_path} to /block");
                            if let Err(error) = self.device_publisher.publish(connector, &name) {
                                log::warn!(error:?; "Failed to publish block device");
                            }
                        }
                    }
                }
                Err(error) => {
                    log::error!(
                        path:% = device_path,
                        error:?;
                        "Failed to match device",
                    );
                }
            };
        }
    }

    pub async fn shutdown(self) -> Result<(), Error> {
        self.environment.lock().await.shutdown().await
    }
}
