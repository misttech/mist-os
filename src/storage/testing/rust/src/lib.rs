// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context, Result};
use fidl_fuchsia_device::ControllerProxy;
use fidl_fuchsia_hardware_block_partition::{PartitionMarker, PartitionProxy};
use fs_management::filesystem::BlockConnector;
use fs_management::format::{detect_disk_format, DiskFormat};
use fuchsia_component::client::{connect_to_protocol_at_path, ServiceInstanceStream};
use fuchsia_fs::directory::{WatchEvent, Watcher};
use futures::TryStreamExt;
use std::path::{Path, PathBuf};
use {fidl_fuchsia_io as fio, fidl_fuchsia_storage_partitions as fpartitions};

pub mod fvm;
pub mod zxcrypt;

pub type Guid = [u8; 16];

pub fn into_guid(guid: Guid) -> fidl_fuchsia_hardware_block_partition::Guid {
    fidl_fuchsia_hardware_block_partition::Guid { value: guid }
}

pub fn create_random_guid() -> Guid {
    *uuid::Uuid::new_v4().as_bytes()
}

pub async fn bind_fvm(proxy: &ControllerProxy) -> Result<()> {
    fvm::bind_fvm_driver(proxy).await
}

async fn partition_type_guid_matches(guid: &Guid, partition: &PartitionProxy) -> Result<bool> {
    let (status, type_guid) =
        partition.get_type_guid().await.context("Failed to get type guid (fidl error")?;
    zx::ok(status).context("Failed to get type guid")?;
    let type_guid = if let Some(guid) = type_guid { guid } else { return Ok(false) };
    let matched = type_guid.value == *guid;
    log::info!(matched, type_guid:?, target_guid:?=guid; "matching type guid");
    Ok(matched)
}

async fn partition_instance_guid_matches(guid: &Guid, partition: &PartitionProxy) -> Result<bool> {
    let (status, instance_guid) =
        partition.get_instance_guid().await.context("Failed to get instance guid (fidl error")?;
    zx::ok(status).context("Failed to get instance guid")?;
    let instance_guid = if let Some(guid) = instance_guid { guid } else { return Ok(false) };
    let matched = instance_guid.value == *guid;
    log::info!(matched, instance_guid:?, target_guid:?=guid; "matching instance guid");
    Ok(matched)
}

async fn partition_name_matches(name: &str, partition: &PartitionProxy) -> Result<bool> {
    let (status, partition_name) =
        partition.get_name().await.context("Failed to get partition name (fidl error")?;
    zx::ok(status).context("Failed to get partition name")?;
    let partition_name = if let Some(name) = partition_name { name } else { return Ok(false) };
    let matched = partition_name == name;
    log::info!(matched, partition_name = partition_name.as_str(), target_name = name; "matching name");
    Ok(matched)
}

async fn block_contents_match(format: DiskFormat, block: &PartitionProxy) -> Result<bool> {
    let content_format = detect_disk_format(block).await;
    Ok(format == content_format)
}

/// A constraint for the block device being waited for in `wait_for_block_device`.
#[derive(Debug)]
pub enum BlockDeviceMatcher<'a> {
    /// Only matches block devices that have this type Guid.
    TypeGuid(&'a Guid),

    /// Only matches block devices that have this instance Guid.
    InstanceGuid(&'a Guid),

    /// Only matches block devices that have this name.
    Name(&'a str),

    /// Only matches block devices whose contents match the given format.
    ContentsMatch(DiskFormat),
}

impl BlockDeviceMatcher<'_> {
    async fn matches(&self, partition: &PartitionProxy) -> Result<bool> {
        match self {
            Self::TypeGuid(guid) => partition_type_guid_matches(guid, partition).await,
            Self::InstanceGuid(guid) => partition_instance_guid_matches(guid, partition).await,
            Self::Name(name) => partition_name_matches(name, partition).await,
            Self::ContentsMatch(format) => block_contents_match(*format, partition).await,
        }
    }
}

async fn matches_all(partition: &PartitionProxy, matchers: &[BlockDeviceMatcher<'_>]) -> bool {
    for matcher in matchers {
        if !matcher.matches(partition).await.unwrap_or(false) {
            return false;
        }
    }
    true
}

/// Waits for a block device to appear in `/dev/class/block` that meets all of the requirements of
/// `matchers`. Returns the path to the matched block device.
/// TODO(https://fxbug.dev/339491886): Remove when all clients are ported to
/// `wait_for_block_device`.
pub async fn wait_for_block_device_devfs(matchers: &[BlockDeviceMatcher<'_>]) -> Result<PathBuf> {
    const DEV_CLASS_BLOCK: &str = "/dev/class/block";
    assert!(!matchers.is_empty());
    let block_dev_dir =
        fuchsia_fs::directory::open_in_namespace(DEV_CLASS_BLOCK, fio::PERM_READABLE)?;
    let mut watcher = Watcher::new(&block_dev_dir).await?;
    while let Some(msg) = watcher.try_next().await? {
        if msg.event != WatchEvent::ADD_FILE && msg.event != WatchEvent::EXISTING {
            continue;
        }
        if msg.filename.to_str() == Some(".") {
            continue;
        }
        let path = Path::new(DEV_CLASS_BLOCK).join(msg.filename);
        let partition = connect_to_protocol_at_path::<PartitionMarker>(path.to_str().unwrap())?;
        if matches_all(&partition, matchers).await {
            return Ok(path);
        }
    }
    Err(anyhow!("Failed to wait for block device"))
}

/// Waits for the first partition service instance that meets all of the requirements of `matchers`.
/// Returns the path to the matched block device.
/// TODO(https://fxbug.dev/339491886): Remove when all clients are ported to
/// `wait_for_block_device_devfs.
pub async fn wait_for_block_device(
    matchers: &[BlockDeviceMatcher<'_>],
    mut stream: ServiceInstanceStream<fpartitions::PartitionServiceMarker>,
) -> Result<fpartitions::PartitionServiceProxy> {
    while let Some(proxy) = stream.try_next().await? {
        let partition = proxy.connect_partition()?.into_proxy();
        if matches_all(&partition, matchers).await {
            return Ok(proxy);
        }
    }
    unreachable!()
}

/// Looks for a block device already in `/dev/class/block` that meets all of the requirements of
/// `matchers`. Returns the path to the matched block device.
/// TODO(https://fxbug.dev/339491886): Remove when all clients are ported to `find_block_device`.
pub async fn find_block_device_devfs(matchers: &[BlockDeviceMatcher<'_>]) -> Result<PathBuf> {
    const DEV_CLASS_BLOCK: &str = "/dev/class/block";
    assert!(!matchers.is_empty());
    let block_dev_dir =
        fuchsia_fs::directory::open_in_namespace(DEV_CLASS_BLOCK, fio::PERM_READABLE)?;
    let entries = fuchsia_fs::directory::readdir(&block_dev_dir)
        .await
        .context("Failed to readdir /dev/class/block")?;
    for entry in entries {
        let path = Path::new(DEV_CLASS_BLOCK).join(entry.name);
        let partition = connect_to_protocol_at_path::<PartitionMarker>(path.to_str().unwrap())?;
        if matches_all(&partition, matchers).await {
            return Ok(path);
        }
    }
    Err(anyhow!("Failed to find matching block device"))
}

/// Returns the first partition in `partitions` matching all of `matchers.`  Ok(None) indicates no
/// partitions matched.
pub async fn find_block_device<C, Iter>(
    matchers: &[BlockDeviceMatcher<'_>],
    partitions: Iter,
) -> Result<Option<C>>
where
    C: BlockConnector,
    Iter: Iterator<Item = C>,
{
    for connector in partitions {
        let partition = connector.connect_partition()?.into_proxy();
        if matches_all(&partition, matchers).await {
            return Ok(Some(connector));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl_fuchsia_hardware_block_volume::ALLOCATE_PARTITION_FLAG_INACTIVE;
    use ramdevice_client::RamdiskClient;
    const BLOCK_SIZE: u64 = 512;
    const BLOCK_COUNT: u64 = 64 * 1024 * 1024 / BLOCK_SIZE;
    const FVM_SLICE_SIZE: usize = 1024 * 1024;
    const INSTANCE_GUID: Guid = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f,
    ];
    const TYPE_GUID: Guid = [
        0x00, 0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80, 0x90, 0xa0, 0xb0, 0xc0, 0xd0, 0xe0,
        0xf0,
    ];
    const VOLUME_NAME: &str = "volume-name";

    #[fuchsia::test]
    async fn wait_for_block_device_devfs_with_all_match_criteria() {
        let ramdisk = RamdiskClient::create(BLOCK_SIZE, BLOCK_COUNT).await.unwrap();
        let fvm = fvm::set_up_fvm(
            ramdisk.as_controller().expect("invalid controller"),
            ramdisk.as_dir().expect("invalid directory proxy"),
            FVM_SLICE_SIZE,
        )
        .await
        .expect("Failed to format ramdisk with FVM");
        fvm::create_fvm_volume(
            &fvm,
            VOLUME_NAME,
            &TYPE_GUID,
            &INSTANCE_GUID,
            None,
            ALLOCATE_PARTITION_FLAG_INACTIVE,
        )
        .await
        .expect("Failed to create fvm volume");

        wait_for_block_device_devfs(&[
            BlockDeviceMatcher::TypeGuid(&TYPE_GUID),
            BlockDeviceMatcher::InstanceGuid(&INSTANCE_GUID),
            BlockDeviceMatcher::Name(VOLUME_NAME),
        ])
        .await
        .expect("Failed to find block device");

        find_block_device_devfs(&[
            BlockDeviceMatcher::TypeGuid(&TYPE_GUID),
            BlockDeviceMatcher::InstanceGuid(&INSTANCE_GUID),
            BlockDeviceMatcher::Name(VOLUME_NAME),
        ])
        .await
        .expect("Failed to find block device");

        find_block_device_devfs(&[
            BlockDeviceMatcher::TypeGuid(&TYPE_GUID),
            BlockDeviceMatcher::InstanceGuid(&INSTANCE_GUID),
            BlockDeviceMatcher::Name("something else"),
        ])
        .await
        .expect_err("Unexpected match for block device");
    }

    #[fuchsia::test]
    async fn wait_for_block_device_with_all_match_criteria() {
        let ramdisk = RamdiskClient::create(BLOCK_SIZE, BLOCK_COUNT).await.unwrap();
        let fvm = fvm::set_up_fvm(
            ramdisk.as_controller().expect("invalid controller"),
            ramdisk.as_dir().expect("invalid directory proxy"),
            FVM_SLICE_SIZE,
        )
        .await
        .expect("Failed to format ramdisk with FVM");
        fvm::create_fvm_volume(
            &fvm,
            VOLUME_NAME,
            &TYPE_GUID,
            &INSTANCE_GUID,
            None,
            ALLOCATE_PARTITION_FLAG_INACTIVE,
        )
        .await
        .expect("Failed to create fvm volume");

        wait_for_block_device_devfs(&[
            BlockDeviceMatcher::TypeGuid(&TYPE_GUID),
            BlockDeviceMatcher::InstanceGuid(&INSTANCE_GUID),
            BlockDeviceMatcher::Name(VOLUME_NAME),
        ])
        .await
        .expect("Failed to find block device");

        find_block_device_devfs(&[
            BlockDeviceMatcher::TypeGuid(&TYPE_GUID),
            BlockDeviceMatcher::InstanceGuid(&INSTANCE_GUID),
            BlockDeviceMatcher::Name(VOLUME_NAME),
        ])
        .await
        .expect("Failed to find block device");

        find_block_device_devfs(&[
            BlockDeviceMatcher::TypeGuid(&TYPE_GUID),
            BlockDeviceMatcher::InstanceGuid(&INSTANCE_GUID),
            BlockDeviceMatcher::Name("something else"),
        ])
        .await
        .expect_err("Unexpected match for block device");
    }
}
