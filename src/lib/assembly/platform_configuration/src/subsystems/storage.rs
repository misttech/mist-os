// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use assembly_config_schema::platform_config::storage_config::StorageConfig;

pub(crate) struct StorageSubsystemConfig;
impl DefineSubsystemConfiguration<StorageConfig> for StorageSubsystemConfig {
    fn define_configuration(
        _context: &ConfigurationContext<'_>,
        storage_config: &StorageConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        if storage_config.live_usb_enabled {
            builder.platform_bundle("live_usb");
        } else {
            builder.platform_bundle("empty_live_usb");
        }

        if storage_config.configure_fshost {
            builder.platform_bundle("fshost_common");
            builder.platform_bundle("fshost_storage");
            builder.platform_bundle("fshost_fxfs");

            let mut fshost_config_builder = builder.bootfs().component("meta/fshost.cm")?;
            fshost_config_builder
                // LINT.IfChange
                .field("blobfs", true)?
                .field("blobfs_allow_delivery_blobs", false)?
                .field("blobfs_max_bytes", 0)?
                .field("bootpart", true)?
                .field("check_filesystems", true)?
                .field("data", true)?
                .field("data_max_bytes", 0)?
                .field("disable_block_watcher", false)?
                .field("factory", false)?
                .field("fvm", true)?
                .field("ramdisk_image", false)?
                .field("gpt", true)?
                .field("gpt_all", false)?
                .field("mbr", false)?
                .field("netboot", false)?
                .field("no_zxcrypt", false)?
                .field("format_data_on_corruption", true)?
                .field("blobfs_initial_inodes", 0)?
                .field("blobfs_use_deprecated_padded_format", false)?
                .field("allow_legacy_data_partition_names", false)?
                .field("use_disk_migration", false)?
                .field("nand", false)?
                .field("fxfs_blob", false)?
                // LINT.ThenChange(/src/storage/fshost/generated_fshost_config.gni)
                // LINT.IfChange
                .field("fvm_slice_size", 8388608)?;
            // LINT.ThenChange(/build/images/fvm.gni)

            fshost_config_builder.field("data_filesystem_format", "fxfs")?;
        }

        Ok(())
    }
}
