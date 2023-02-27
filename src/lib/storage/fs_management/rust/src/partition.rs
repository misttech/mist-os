// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::format::{detect_disk_format, DiskFormat},
    anyhow::{anyhow, Context, Error},
    fidl::endpoints::Proxy,
    fidl_fuchsia_hardware_block::BlockProxy,
    fidl_fuchsia_hardware_block_partition::{
        Guid, PartitionAndDeviceMarker, PartitionAndDeviceProxy,
    },
    fidl_fuchsia_hardware_block_volume::VolumeManagerProxy,
    fuchsia_async::TimeoutExt,
    fuchsia_component::client::connect_to_named_protocol_at_dir_root,
    fuchsia_fs::directory::{WatchEvent, Watcher},
    fuchsia_zircon::{self as zx, Duration},
    futures::StreamExt,
};

/// Set of parameters to use for identifying the correct partition to open via
/// [`open_partition`]
///
/// If multiple matchers are specified, the first partition that satisfies any set
/// of matchers will be used. At least one of [`type_guids`], [`instance_guids`], [`labels`],
/// [`detected_formats`], or [`parent_device`] must be specified.
#[derive(Default, Clone)]
pub struct PartitionMatcher {
    /// Set of type GUIDs the partition must match. Ignored if empty.
    pub type_guids: Option<Vec<[u8; 16]>>,
    /// Set of instance GUIDs the partition must match. Ignored if empty.
    pub instance_guids: Option<Vec<[u8; 16]>>,
    pub labels: Option<Vec<String>>,
    pub detected_disk_formats: Option<Vec<DiskFormat>>,
    /// partition must be a child of this device.
    pub parent_device: Option<String>,
    /// The topological path must not start with this prefix.
    pub ignore_prefix: Option<String>,
    /// The topological path must not contain this substring.
    pub ignore_if_path_contains: Option<String>,
}

const BLOCK_DEV_PATH: &str = "/dev/class/block/";

/// Waits for a partition to appear on BLOCK_DEV_PATH that
/// matches the fields in the PartitionMatcher. Returns the
/// path of the partition if found. Errors after timeout duration.
pub async fn find_partition(matcher: PartitionMatcher, timeout: Duration) -> Result<String, Error> {
    async {
        let dir = fuchsia_fs::directory::open_in_namespace(
            BLOCK_DEV_PATH,
            fuchsia_fs::OpenFlags::empty(),
        )?;
        let mut watcher = Watcher::new(&dir).await.context("making watcher")?;
        while let Some(message) = watcher.next().await {
            let message = message.context("watcher channel returned error")?;
            match message.event {
                WatchEvent::ADD_FILE | WatchEvent::EXISTING => {
                    let filename = message.filename.to_str().unwrap();
                    if filename == "." {
                        continue;
                    }
                    let device_proxy = connect_to_named_protocol_at_dir_root::<
                        PartitionAndDeviceMarker,
                    >(&dir, filename)
                    .context("opening partition path")?;
                    let topological_path = device_proxy
                        .get_topological_path()
                        .await
                        .context("Failed to get topological path in open partition")?
                        .map_err(zx::Status::from_raw)?;
                    if partition_matches(device_proxy, &matcher).await {
                        return Ok(topological_path);
                    }
                }
                _ => (),
            }
        }
        Err(anyhow!("Watch stream unexpectedly ended"))
    }
    .on_timeout(timeout, || Err(anyhow!("Expected partition")))
    .await
}

/// Checks if the partition associated with proxy matches the matcher. If an error is encountered,
/// it prints it out as a warning and returns false. An error isn't necessarily an issue - we might
/// be using a matcher that wants a type guid, but the device we are currently checking doesn't
/// implement get_type_guid. The error message may help debugging why no partition was matched
/// though.
pub async fn partition_matches(proxy: PartitionAndDeviceProxy, matcher: &PartitionMatcher) -> bool {
    match partition_matches_res(proxy, matcher).await {
        Ok(matched) => matched,
        Err(e) => {
            tracing::warn!(?e, "partition_matches failed");
            return false;
        }
    }
}

async fn partition_matches_res(
    proxy: PartitionAndDeviceProxy,
    matcher: &PartitionMatcher,
) -> Result<bool, Error> {
    assert!(
        matcher.type_guids.is_some()
            || matcher.instance_guids.is_some()
            || matcher.detected_disk_formats.is_some()
            || matcher.parent_device.is_some()
            || matcher.labels.is_some()
    );

    if let Some(matcher_type_guids) = &matcher.type_guids {
        let (status, guid_option) =
            proxy.get_type_guid().await.context("transport error on get_type_guid")?;
        zx::Status::ok(status).context("get_type_guid failed")?;
        let guid = guid_option.ok_or(anyhow!("Expected type guid"))?;
        if !matcher_type_guids.into_iter().any(|x| x == &guid.value) {
            return Ok(false);
        }
    }

    if let Some(matcher_instance_guids) = &matcher.instance_guids {
        let (status, guid_option) =
            proxy.get_instance_guid().await.context("transport error on get_instance_guid")?;
        zx::Status::ok(status).context("get_instance_guid failed")?;
        let guid = guid_option.ok_or(anyhow!("Expected instance guid"))?;
        if !matcher_instance_guids.into_iter().any(|x| x == &guid.value) {
            return Ok(false);
        }
    }

    if let Some(matcher_labels) = &matcher.labels {
        let (status, name) = proxy.get_name().await.context("transport error on get_name")?;
        zx::Status::ok(status).context("get_name failed")?;
        let name = name.ok_or(anyhow!("Expected name"))?;
        if name.is_empty() {
            return Ok(false);
        }
        let mut matches_label = false;
        for label in matcher_labels {
            if name == label.to_string() {
                matches_label = true;
                break;
            }
        }
        if !matches_label {
            return Ok(false);
        }
    }

    let topological_path = proxy
        .get_topological_path()
        .await
        .context("get_topological_path failed")?
        .map_err(zx::Status::from_raw)?;

    if let Some(matcher_parent_device) = &matcher.parent_device {
        if !topological_path.starts_with(matcher_parent_device) {
            return Ok(false);
        }
    }

    if let Some(matcher_ignore_prefix) = &matcher.ignore_prefix {
        if topological_path.starts_with(matcher_ignore_prefix) {
            return Ok(false);
        }
    }

    if let Some(matcher_ignore_if_path_contains) = &matcher.ignore_if_path_contains {
        if topological_path.find(matcher_ignore_if_path_contains) != None {
            return Ok(false);
        }
    }

    if let Some(matcher_detected_disk_formats) = &matcher.detected_disk_formats {
        let block_proxy = BlockProxy::new(proxy.into_channel().unwrap());
        let detected_format = detect_disk_format(&block_proxy).await;
        if !matcher_detected_disk_formats.into_iter().any(|x| x == &detected_format) {
            return Ok(false);
        }
    }
    return Ok(true);
}

pub async fn fvm_allocate_partition(
    fvm_proxy: &VolumeManagerProxy,
    type_guid: [u8; 16],
    instance_guid: [u8; 16],
    name: &str,
    flags: u32,
    slice_count: u64,
) -> Result<String, Error> {
    let status = fvm_proxy
        .allocate_partition(
            slice_count,
            &mut Guid { value: type_guid },
            &mut Guid { value: instance_guid },
            name,
            flags,
        )
        .await?;
    zx::Status::ok(status)?;

    let matcher = PartitionMatcher {
        type_guids: Some(vec![type_guid]),
        instance_guids: Some(vec![instance_guid]),
        ..Default::default()
    };

    find_partition(matcher, Duration::from_seconds(10)).await
}

#[cfg(test)]
mod tests {
    use {
        super::{partition_matches, PartitionMatcher},
        crate::format::{constants, DiskFormat},
        fidl::endpoints::create_proxy_and_stream,
        fidl_fuchsia_hardware_block::{BlockInfo, Flag},
        fidl_fuchsia_hardware_block_partition::{
            Guid, PartitionAndDeviceMarker, PartitionAndDeviceRequest,
        },
        fuchsia_zircon as zx,
        futures::{pin_mut, select, FutureExt, StreamExt},
    };

    const VALID_TYPE_GUID: [u8; 16] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f,
    ];

    const VALID_INSTANCE_GUID: [u8; 16] = [
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e,
        0x1f,
    ];

    const INVALID_GUID_1: [u8; 16] = [
        0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2d, 0x2e,
        0x2f,
    ];

    const INVALID_GUID_2: [u8; 16] = [
        0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x3b, 0x3c, 0x3d, 0x3e,
        0x3f,
    ];

    const VALID_LABEL: &str = "test";
    const INVALID_LABEL_1: &str = "TheWrongLabel";
    const INVALID_LABEL_2: &str = "StillTheWrongLabel";
    const PARENT_DEVICE_PATH: &str = "/fake/block/device/1";
    const NOT_PARENT_DEVICE_PATH: &str = "/fake/block/device/2";
    const DEFAULT_PATH: &str = "/fake/block/device/1/partition/001";

    async fn check_partition_matches(matcher: &PartitionMatcher) -> bool {
        let (proxy, mut stream) = create_proxy_and_stream::<PartitionAndDeviceMarker>().unwrap();

        let mock_device = async {
            while let Some(request) = stream.next().await {
                match request {
                    Ok(PartitionAndDeviceRequest::GetTypeGuid { responder }) => {
                        responder
                            .send(zx::sys::ZX_OK, Some(&mut Guid { value: VALID_TYPE_GUID }))
                            .unwrap();
                    }
                    Ok(PartitionAndDeviceRequest::GetInstanceGuid { responder }) => {
                        responder
                            .send(zx::sys::ZX_OK, Some(&mut Guid { value: VALID_INSTANCE_GUID }))
                            .unwrap();
                    }
                    Ok(PartitionAndDeviceRequest::GetName { responder }) => {
                        responder.send(zx::sys::ZX_OK, Some(VALID_LABEL)).unwrap();
                    }
                    Ok(PartitionAndDeviceRequest::GetTopologicalPath { responder }) => {
                        responder.send(&mut Ok(DEFAULT_PATH.to_string())).unwrap();
                    }
                    Ok(PartitionAndDeviceRequest::GetInfo { responder }) => {
                        responder
                            .send(&mut Ok(BlockInfo {
                                block_count: 1000,
                                block_size: 512,
                                max_transfer_size: 1024 * 1024,
                                flags: Flag::empty(),
                            }))
                            .unwrap();
                    }
                    Ok(PartitionAndDeviceRequest::ReadBlocks {
                        responder,
                        vmo_offset,
                        vmo,
                        length,
                        dev_offset,
                    }) => {
                        assert_eq!(dev_offset, 0);
                        assert_eq!(length, 4096);
                        vmo.write(&constants::FVM_MAGIC, vmo_offset).unwrap();
                        responder.send(&mut Ok(())).unwrap();
                    }
                    _ => {
                        println!("Unexpected request: {:?}", request);
                        unreachable!()
                    }
                }
            }
        }
        .fuse();

        pin_mut!(mock_device);

        select! {
            _ = mock_device => unreachable!(),
            matches = partition_matches(proxy, &matcher).fuse() => matches,
        }
    }

    #[fuchsia::test]
    async fn test_type_guid_match() {
        let matcher = PartitionMatcher {
            type_guids: Some(vec![VALID_TYPE_GUID, INVALID_GUID_1]),
            ..Default::default()
        };
        assert_eq!(check_partition_matches(&matcher).await, true);
    }

    #[fuchsia::test]
    async fn test_instance_guid_match() {
        let matcher = PartitionMatcher {
            instance_guids: Some(vec![VALID_INSTANCE_GUID, INVALID_GUID_1]),
            ..Default::default()
        };
        assert_eq!(check_partition_matches(&matcher).await, true);
    }

    #[fuchsia::test]
    async fn test_type_and_instance_guid_match() {
        let matcher = PartitionMatcher {
            type_guids: Some(vec![VALID_TYPE_GUID, INVALID_GUID_1]),
            instance_guids: Some(vec![VALID_INSTANCE_GUID, INVALID_GUID_2]),
            ..Default::default()
        };
        assert_eq!(check_partition_matches(&matcher).await, true);
    }

    #[fuchsia::test]
    async fn test_parent_match() {
        let matcher = PartitionMatcher {
            parent_device: Some(PARENT_DEVICE_PATH.to_string()),
            ..Default::default()
        };
        assert_eq!(check_partition_matches(&matcher).await, true);

        let matcher2 = PartitionMatcher {
            parent_device: Some(NOT_PARENT_DEVICE_PATH.to_string()),
            ..Default::default()
        };
        assert_eq!(check_partition_matches(&matcher2).await, false);
    }

    #[fuchsia::test]
    async fn test_single_label_match() {
        let the_labels = vec![VALID_LABEL.to_string()];
        let matcher = PartitionMatcher { labels: Some(the_labels), ..Default::default() };
        assert_eq!(check_partition_matches(&matcher).await, true);
    }

    #[fuchsia::test]
    async fn test_multi_label_match() {
        let mut the_labels = vec![VALID_LABEL.to_string()];
        the_labels.push(INVALID_LABEL_1.to_string());
        the_labels.push(INVALID_LABEL_2.to_string());
        let matcher = PartitionMatcher { labels: Some(the_labels), ..Default::default() };
        assert_eq!(check_partition_matches(&matcher).await, true);
    }

    #[fuchsia::test]
    async fn test_ignore_prefix_mismatch() {
        let matcher = PartitionMatcher {
            type_guids: Some(vec![VALID_TYPE_GUID]),
            ignore_prefix: Some("/fake/block/device".to_string()),
            ..Default::default()
        };
        assert_eq!(check_partition_matches(&matcher).await, false);
    }

    #[fuchsia::test]
    async fn test_ignore_prefix_match() {
        let matcher = PartitionMatcher {
            type_guids: Some(vec![VALID_TYPE_GUID]),
            ignore_prefix: Some("/real/block/device".to_string()),
            ..Default::default()
        };
        assert_eq!(check_partition_matches(&matcher).await, true);
    }

    #[fuchsia::test]
    async fn test_ignore_if_path_contains_mismatch() {
        let matcher = PartitionMatcher {
            type_guids: Some(vec![VALID_TYPE_GUID]),
            ignore_if_path_contains: Some("/device/1".to_string()),
            ..Default::default()
        };
        assert_eq!(check_partition_matches(&matcher).await, false);
    }

    #[fuchsia::test]
    async fn test_ignore_if_path_contains_match() {
        let matcher = PartitionMatcher {
            type_guids: Some(vec![VALID_TYPE_GUID]),
            ignore_if_path_contains: Some("/device/0".to_string()),
            ..Default::default()
        };
        assert_eq!(check_partition_matches(&matcher).await, true);
    }

    #[fuchsia::test]
    async fn test_type_and_label_match() {
        let the_labels = vec![VALID_LABEL.to_string()];
        let matcher = PartitionMatcher {
            type_guids: Some(vec![VALID_TYPE_GUID]),
            labels: Some(the_labels),
            ..Default::default()
        };
        assert_eq!(check_partition_matches(&matcher).await, true);
    }

    #[fuchsia::test]
    async fn test_type_guid_mismatch() {
        let matcher = PartitionMatcher {
            type_guids: Some(vec![INVALID_GUID_1, INVALID_GUID_2]),
            ..Default::default()
        };
        assert_eq!(check_partition_matches(&matcher).await, false);
    }

    #[fuchsia::test]
    async fn test_instance_guid_mismatch() {
        let matcher = PartitionMatcher {
            instance_guids: Some(vec![INVALID_GUID_1, INVALID_GUID_2]),
            ..Default::default()
        };
        assert_eq!(check_partition_matches(&matcher).await, false);
    }

    #[fuchsia::test]
    async fn test_label_mismatch() {
        let mut the_labels = vec![INVALID_LABEL_1.to_string()];
        the_labels.push(INVALID_LABEL_2.to_string());
        let matcher = PartitionMatcher { labels: Some(the_labels), ..Default::default() };
        assert_eq!(check_partition_matches(&matcher).await, false);
    }

    #[fuchsia::test]
    async fn test_detected_disk_format_match() {
        let matcher = PartitionMatcher {
            detected_disk_formats: Some(vec![DiskFormat::Fvm, DiskFormat::Minfs]),
            ..Default::default()
        };
        assert_eq!(check_partition_matches(&matcher).await, true);
    }

    #[fuchsia::test]
    async fn test_detected_disk_format_mismatch() {
        let matcher = PartitionMatcher {
            detected_disk_formats: Some(vec![DiskFormat::Fxfs, DiskFormat::Minfs]),
            ..Default::default()
        };
        assert_eq!(check_partition_matches(&matcher).await, false);
    }
}
