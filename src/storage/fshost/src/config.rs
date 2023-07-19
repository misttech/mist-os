// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::boot_args::BootArgs;

#[cfg(test)]
pub fn default_config() -> fshost_config::Config {
    fshost_config::Config {
        blobfs: true,
        blobfs_allow_delivery_blobs: false,
        blobfs_initial_inodes: 0,
        blobfs_max_bytes: 0,
        blobfs_use_deprecated_padded_format: false,
        bootpart: true,
        data: true,
        data_filesystem_format: String::new(),
        data_max_bytes: 0,
        disable_block_watcher: false,
        factory: false,
        format_data_on_corruption: true,
        check_filesystems: true,
        fvm: true,
        ramdisk_image: false,
        fvm_slice_size: 1024 * 1024, // Default to 1 MiB slice size for tests.
        fxfs_blob: false,
        gpt: true,
        gpt_all: false,
        mbr: false,
        nand: false,
        netboot: false,
        no_zxcrypt: false,
        use_disk_migration: false,
    }
}

pub fn apply_boot_args_to_config(config: &mut fshost_config::Config, boot_args: &BootArgs) {
    if boot_args.netboot() {
        config.netboot = true;
    }

    if boot_args.check_filesystems() {
        config.check_filesystems = true;
    }
}
