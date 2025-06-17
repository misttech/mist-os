// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assembly_container::WalkPaths;
use assembly_images_config::ProductFilesystemConfig;
use camino::Utf8PathBuf;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Platform configuration options for storage support.
#[derive(Debug, Deserialize, Serialize, PartialEq, JsonSchema, WalkPaths)]
#[serde(default, deny_unknown_fields)]
pub struct StorageConfig {
    #[walk_paths]
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub component_id_index: ComponentIdIndexConfig,

    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub factory_data: FactoryDataConfig,

    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub filesystems: ProductFilesystemConfig,

    /// Prevents fshost from binding to detected block devices.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub disable_automount: bool,

    /// Enables storage-host.  See RFC (https://fxrev.dev/1077832) for details.
    #[serde(default = "default_storage_host")]
    #[serde(skip_serializing_if = "is_default_storage_host")]
    pub storage_host_enabled: bool,

    /// Enable the automatic garbage collection of mutable storage.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub mutable_storage_garbage_collection: bool,

    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub starnix_volume: StarnixVolumeConfig,
}

fn is_default_storage_host(value: &bool) -> bool {
    *value == default_storage_host()
}

fn default_storage_host() -> bool {
    true
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            component_id_index: Default::default(),
            factory_data: Default::default(),
            filesystems: Default::default(),
            disable_automount: Default::default(),
            storage_host_enabled: true,
            mutable_storage_garbage_collection: Default::default(),
            starnix_volume: Default::default(),
        }
    }
}

/// Platform configuration options for the component id index
///
/// Platform configuration options for the component id index which describes
/// consistent storage IDs to use for component monikers. If the monikers
/// change, the IDs can stay consistent, ensuring that the storage does not
/// need to be migrated to a new location.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema, WalkPaths)]
#[serde(default)]
pub struct ComponentIdIndexConfig {
    /// An optional index to use for product-provided components.
    #[walk_paths]
    #[schemars(schema_with = "crate::option_path_schema")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product_index: Option<Utf8PathBuf>,
}

/// Platform configuration options for the factory data store.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default)]
pub struct FactoryDataConfig {
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub enabled: bool,
}

/// Platform configuration options for the main Starnix volume.
///
/// If set, this field specifies the name of the volume which the main Starnix component will store
/// its mutable data in.  If unset, Starnix will rely on a storage capability instead.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default)]
pub struct StarnixVolumeConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}
