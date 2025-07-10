// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::Device;
use anyhow::Error;
use fidl::endpoints::{DiscoverableProtocolMarker as _, ServerEnd};
use fidl_fuchsia_hardware_block_volume::VolumeMarker;
use fidl_fuchsia_io as fio;
use fs_management::filesystem::BlockConnector;
use std::sync::Arc;
use vfs::directory::helper::DirectlyMutable;
use vfs::directory::simple::Simple;
use vfs::service::endpoint;
use vfs::ExecutionScope;

pub trait SinglePublisher: Send + Sync {
    fn publish(self: Box<Self>, device: &dyn Device) -> Result<(), Error>;
}

/// A staged entry is one where the entry is placed in the block directory but it hasn't been
/// matched yet. Once it's matched, the matcher can use this to finish publishing the device. Until
/// it is completely published, open requests will be queued in the standard dangling-server-end
/// pipelining way.
pub struct StagedPublisher {
    scope: ExecutionScope,
    server_end: ServerEnd<fio::DirectoryMarker>,
}

impl StagedPublisher {
    fn new(scope: ExecutionScope) -> (fio::DirectoryProxy, Self) {
        let (proxy, server_end) = fidl::endpoints::create_proxy();
        (proxy, Self { scope, server_end })
    }
}

impl SinglePublisher for StagedPublisher {
    fn publish(self: Box<Self>, device: &dyn Device) -> Result<(), Error> {
        let volume = device.block_connector()?;
        let entry = vfs::pseudo_directory! {
            VolumeMarker::PROTOCOL_NAME => endpoint(move |_scope, channel| {
                volume.connect_channel_to_volume(channel.into_zx_channel().into())
                    .unwrap_or_else(|error| {
                        log::error!(error:%; "failed to open volume");
                    });
            }),
        };
        vfs::directory::serve_on(entry, fio::PERM_READABLE, self.scope, self.server_end);

        Ok(())
    }
}

pub struct DevicePublisher {
    scope: ExecutionScope,
    debug_block_dir: Arc<Simple>,
    block_dir: Arc<Simple>,
}

impl DevicePublisher {
    pub fn new(scope: ExecutionScope) -> Self {
        Self {
            scope,
            debug_block_dir: vfs::pseudo_directory! {},
            block_dir: vfs::pseudo_directory! {},
        }
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

    pub fn publish_to_debug_block_dir(&self, device: &dyn Device, name: &str) -> Result<(), Error> {
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

    pub fn stage(&self, name: &str) -> Result<StagedPublisher, Error> {
        let (proxy, staged_publisher) = StagedPublisher::new(self.scope.clone());
        self.block_dir.add_entry(name, vfs::remote::remote_dir(proxy))?;
        Ok(staged_publisher)
    }

    pub fn publish(&mut self, volume: Box<dyn BlockConnector>, name: &str) -> Result<(), Error> {
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
