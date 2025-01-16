// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assembly_file_relative_path::{FileRelativePathBuf, SupportsFileRelativePaths};
use assembly_images_config::ProductFilesystemConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Platform configuration options for storage support.
#[derive(
    Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema, SupportsFileRelativePaths,
)]
#[serde(default, deny_unknown_fields)]
pub struct StorageConfig {
    #[file_relative_paths]
    pub component_id_index: ComponentIdIndexConfig,

    pub factory_data: FactoryDataConfig,

    pub filesystems: ProductFilesystemConfig,

    /// Prevents fshost from binding to detected block devices.
    pub disable_automount: bool,

    /// Enables storage-host.  See RFC (https://fxrev.dev/1077832) for details.
    pub storage_host_enabled: bool,

    /// Enable the automatic garbage collection of mutable storage.
    pub mutable_storage_garbage_collection: bool,
}

/// Platform configuration options for the component id index
///
/// Platform configuration options for the component id index which describes
/// consistent storage IDs to use for component monikers. If the monikers
/// change, the IDs can stay consistent, ensuring that the storage does not
/// need to be migrated to a new location.
#[derive(
    Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema, SupportsFileRelativePaths,
)]
#[serde(default)]
pub struct ComponentIdIndexConfig {
    /// An optional index to use for product-provided components.
    #[file_relative_paths]
    #[schemars(schema_with = "crate::option_path_schema")]
    pub product_index: Option<FileRelativePathBuf>,
}

/// Platform configuration options for the factory data store.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default)]
pub struct FactoryDataConfig {
    pub enabled: bool,
}
