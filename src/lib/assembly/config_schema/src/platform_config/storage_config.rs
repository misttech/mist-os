// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assembly_images_config::ProductFilesystemConfig;
use serde::{Deserialize, Serialize};

/// Platform configuration options for storage support.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StorageConfig {
    #[serde(default)]
    pub live_usb_enabled: bool,

    #[serde(default)]
    pub configure_fshost: bool,

    #[serde(default)]
    pub gpt_all: bool,

    #[serde(default)]
    pub filesystems: ProductFilesystemConfig,
}
