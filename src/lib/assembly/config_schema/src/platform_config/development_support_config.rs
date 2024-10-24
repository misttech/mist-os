// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assembly_file_relative_path::{FileRelativePathBuf, SupportsFileRelativePaths};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Platform configuration options for enabling development support.
#[derive(
    Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema, SupportsFileRelativePaths,
)]
#[serde(deny_unknown_fields)]
pub struct DevelopmentSupportConfig {
    /// Override the build-type enablement of development support, to include
    /// development support in userdebug which doesn't have full development
    /// access.
    pub enabled: Option<bool>,

    // Whether to use vsock based development connection.
    #[serde(default)]
    pub vsock_development: bool,

    /// Path to a file containing ssh keys that are authorized to connect to the
    /// device.
    #[file_relative_paths]
    #[schemars(schema_with = "crate::option_path_schema")]
    pub authorized_ssh_keys_path: Option<FileRelativePathBuf>,

    /// Path to a file containing CA certs that are trusted roots for signed ssh
    /// keys that are authorized to connect to the device.
    #[file_relative_paths]
    #[schemars(schema_with = "crate::option_path_schema")]
    pub authorized_ssh_ca_certs_path: Option<FileRelativePathBuf>,

    /// Whether to include sl4f.
    #[serde(default)]
    pub include_sl4f: bool,

    /// Include the bin/clock program on the target to get the monotonic time
    /// from the device. TODO(b/309452964): Remove once e2e tests use:
    ///   `ffx target get-time`
    #[serde(default)]
    pub include_bin_clock: bool,

    /// Override netsvc inclusion on the target.
    ///
    /// Follows the same resolution as `enabled` if absent.
    #[serde(default)]
    pub include_netsvc: bool,

    /// Tools to enable along with development support
    #[serde(default)]
    pub tools: ToolsConfig,
}

/// Platform-provided tools for development and debugging.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolsConfig {
    /// Tools for connectivity.
    #[serde(default)]
    pub connectivity: ConnectivityToolsConfig,

    /// Tools for storage.
    #[serde(default)]
    pub storage: StorageToolsConfig,
}

/// Platform-provided tools for development and debugging connectivity.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectivityToolsConfig {
    /// Include tools for basic networking, such as:
    ///   - 'nc'
    ///   - 'iperf3'
    ///   - 'tcpdump'
    #[serde(default)]
    pub enable_networking: bool,

    /// Include tools for wlan
    #[serde(default)]
    pub enable_wlan: bool,

    /// Include tools for Thread
    #[serde(default)]
    pub enable_thread: bool,
}

/// Platform-provided tools for the development and debugging of storage.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StorageToolsConfig {
    /// Include tools used for disk partitioning, such as:
    ///   - 'mount'
    ///   - 'install-disk-image'
    #[serde(default)]
    pub enable_partitioning_tools: bool,
}
