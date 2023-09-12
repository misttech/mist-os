// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assembly_images_config::ProductFilesystemConfig;
use camino::Utf8PathBuf;
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
    pub component_id_index: ComponentIdIndexConfig,

    #[serde(default)]
    pub filesystems: ProductFilesystemConfig,
}

/// Platform configuration options for the component id index which describes
/// consistent storage IDs to use for component monikers. If the monikers
/// change, the IDs can stay consistent, ensuring that the storage does not
/// need to be migrated to a new location.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct ComponentIdIndexConfig {
    /// An optional index to use for product-provided components.
    #[serde(default)]
    pub product_index: Option<Utf8PathBuf>,

    /// Avoid including the default platform-provided component ids.
    #[serde(default)]
    pub exclude_default_platform_ids: bool,
}
