// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod config;
use crate::config::volumes_spec;
use block_client::RemoteBlockClient;
use config::new_builder;
use fidl::endpoints::ServiceMarker;
use fshost_test_fixture::disk_builder::{
    Disk, DiskBuilder, DEFAULT_DISK_SIZE, DEFAULT_TEST_TYPE_GUID, FVM_PART_INSTANCE_GUID,
    TEST_DISK_BLOCK_SIZE,
};
use fuchsia_component::client::connect_to_named_protocol_at_dir_root;
use std::sync::Arc;
use vmo_backed_block_server::{VmoBackedServer, VmoBackedServerTestingExt as _};
use zx::HandleBased;
use {
    fidl_fuchsia_hardware_block_volume as fvolume, fidl_fuchsia_io as fio,
    fidl_fuchsia_storage_partitions as fpartitions,
};

#[fuchsia::test]
async fn test_provision_fxfs() {
    let mut builder = new_builder();
    builder.fshost().set_config_value("provision_fxfs", true);
    let mut fixture = builder.build().await;

    let partition_labels = vec!["a", "super", "userdata"];
    let vmo = zx::Vmo::create(DEFAULT_DISK_SIZE).unwrap();
    let server = Arc::new(VmoBackedServer::from_vmo(
        TEST_DISK_BLOCK_SIZE,
        vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap(),
    ));
    let client =
        Arc::new(RemoteBlockClient::new(server.connect::<fvolume::VolumeProxy>()).await.unwrap());
    let mut partitions = Vec::new();
    let mut start_block = 64;
    for label in partition_labels {
        partitions.push(gpt::PartitionInfo {
            label: label.to_string(),
            type_guid: gpt::Guid::from_bytes(DEFAULT_TEST_TYPE_GUID),
            instance_guid: gpt::Guid::from_bytes(FVM_PART_INSTANCE_GUID),
            start_block,
            num_blocks: 1,
            flags: 0,
        });
        start_block += 1;
    }
    let _ = gpt::Gpt::format(client, partitions).await.expect("gpt format failed");

    // Add as system GPT. The device matcher for the system GPT expects the type GUID to be None or
    // zero.
    fixture.add_main_disk(Disk::Prebuilt(vmo, None)).await;

    // TODO(https://fxbug.dev/411312604): after updating fshost of the device's new state, expect to
    // only see two partitions, "a" and "fxfs", if migration was successful.

    fixture.tear_down().await;
}

#[fuchsia::test]
async fn test_provision_fxfs_with_fxfs_partition() {
    let mut builder = new_builder();
    builder.fshost().set_config_value("provision_fxfs", true);
    let mut fixture = builder.build().await;

    let mut disk = DiskBuilder::new();
    disk.with_gpt().format_volumes(volumes_spec()).with_extra_gpt_partition("fxfs");
    fixture.add_main_disk(Disk::Builder(disk)).await;

    let partitions =
        fixture.dir(fpartitions::PartitionServiceMarker::SERVICE_NAME, fio::PERM_READABLE);
    let entries =
        fuchsia_fs::directory::readdir(&partitions).await.expect("Failed to read partitions");
    assert_eq!(entries.len(), 2);

    let mut found_partition_labels = Vec::new();
    for entry in entries {
        let endpoint_name = format!("{}/volume", entry.name);
        let volume = connect_to_named_protocol_at_dir_root::<fvolume::VolumeMarker>(
            &partitions,
            &endpoint_name,
        )
        .expect("failed to connect to named protocol at dir root");
        let (raw_status, label) = volume.get_name().await.expect("failed to call get_name");
        zx::Status::ok(raw_status).expect("get_name status failed");
        found_partition_labels.push(label.expect("partition label expected to be some value"));
    }
    assert!(found_partition_labels.iter().any(|label| label == "fvm"));
    assert!(found_partition_labels.iter().any(|label| label == "fxfs"));

    fixture.tear_down().await;
}
