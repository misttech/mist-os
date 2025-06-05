// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::Device;
use anyhow::Error;
use fidl::endpoints::DiscoverableProtocolMarker as _;
use fidl_fuchsia_hardware_block_volume::VolumeMarker;
use fs_management::filesystem::BlockConnector;
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

    pub fn publish(&self, volume: Box<dyn BlockConnector>, name: &str) -> Result<(), Error> {
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
