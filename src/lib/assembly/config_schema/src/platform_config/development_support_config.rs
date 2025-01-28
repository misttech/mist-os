// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assembly_container::WalkPaths;
use assembly_file_relative_path::{FileRelativePathBuf, SupportsFileRelativePaths};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Platform configuration options for enabling development support.
#[derive(
    Debug,
    Default,
    Deserialize,
    Serialize,
    PartialEq,
    JsonSchema,
    SupportsFileRelativePaths,
    WalkPaths,
)]
#[serde(default, deny_unknown_fields)]
pub struct DevelopmentSupportConfig {
    /// Override the build-type enablement of development support, to include
    /// development support in userdebug which doesn't have full development
    /// access.
    /// If nothing is provided, a reasonable default is used based on the build type.
    pub enabled: Option<bool>,

    // Whether to use vsock based development connection.
    pub vsock_development: bool,

    /// Path to a file containing ssh keys that are authorized to connect to the
    /// device.
    #[file_relative_paths]
    #[walk_paths]
    #[schemars(schema_with = "crate::option_path_schema")]
    pub authorized_ssh_keys_path: Option<FileRelativePathBuf>,

    /// Path to a file containing CA certs that are trusted roots for signed ssh
    /// keys that are authorized to connect to the device.
    #[file_relative_paths]
    #[walk_paths]
    #[schemars(schema_with = "crate::option_path_schema")]
    pub authorized_ssh_ca_certs_path: Option<FileRelativePathBuf>,

    /// Whether to include sl4f.
    pub include_sl4f: bool,

    /// Include the bin/clock program on the target to get the monotonic time
    /// from the device. TODO(b/309452964): Remove once e2e tests use:
    ///   `ffx target get-time`
    pub include_bin_clock: bool,

    /// Override netsvc inclusion on the target.
    ///
    /// Follows the same resolution as `enabled` if absent.
    pub include_netsvc: bool,

    /// Tools to enable along with development support
    pub tools: ToolsConfig,

    /// Whether to include the bootstrap testing framework which will allow running tests in a
    /// bringup-like environment using the run-test-suite command line tool.
    pub include_bootstrap_testing_framework: bool,
}

/// Platform-provided tools for development and debugging.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ToolsConfig {
    /// Tools for connectivity.
    pub connectivity: ConnectivityToolsConfig,

    /// Tools for storage.
    pub storage: StorageToolsConfig,
}

/// Platform-provided tools for development and debugging connectivity.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ConnectivityToolsConfig {
    /// Include tools for basic networking, such as:
    ///   - 'nc'
    ///   - 'iperf3'
    ///   - 'tcpdump'
    pub enable_networking: bool,

    /// Include tools for wlan
    pub enable_wlan: bool,

    /// Include tools for Thread
    pub enable_thread: bool,
}

/// Platform-provided tools for the development and debugging of storage.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct StorageToolsConfig {
    /// Include tools used for disk partitioning, such as:
    ///   - 'mount'
    ///   - 'install-disk-image'
    pub enable_partitioning_tools: bool,
}
