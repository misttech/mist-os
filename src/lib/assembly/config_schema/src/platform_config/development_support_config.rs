// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assembly_container::WalkPaths;
use camino::Utf8PathBuf;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Platform configuration options for enabling development support.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema, WalkPaths)]
#[serde(default, deny_unknown_fields)]
pub struct DevelopmentSupportConfig {
    /// Override the build-type enablement of development support, to include
    /// development support in userdebug which doesn't have full development
    /// access.
    /// If nothing is provided, a reasonable default is used based on the build type.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,

    // Whether to use vsock based development connection.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub vsock_development: bool,

    /// Path to a file containing ssh keys that are authorized to connect to the
    /// device.
    #[walk_paths]
    #[schemars(schema_with = "crate::option_path_schema")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorized_ssh_keys_path: Option<Utf8PathBuf>,

    /// Path to a file containing CA certs that are trusted roots for signed ssh
    /// keys that are authorized to connect to the device.
    #[walk_paths]
    #[schemars(schema_with = "crate::option_path_schema")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorized_ssh_ca_certs_path: Option<Utf8PathBuf>,

    /// Whether to include sl4f.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub include_sl4f: bool,

    /// Include the bin/clock program on the target to get the monotonic time
    /// from the device. TODO(b/309452964): Remove once e2e tests use:
    ///   `ffx target get-time`
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub include_bin_clock: bool,

    /// Override netsvc inclusion on the target.
    ///
    /// Follows the same resolution as `enabled` if absent.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub include_netsvc: bool,

    /// Enable the netboot feature of the netsvc
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub enable_netsvc_netboot: bool,

    /// Tools to enable along with development support
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub tools: ToolsConfig,

    /// Whether to include the bootstrap testing framework which will allow running tests in a
    /// bringup-like environment using the run-test-suite command line tool.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub include_bootstrap_testing_framework: bool,

    /// Enable userboot.next for running a boot-time test.
    ///
    /// Only valid on eng builds.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub enable_userboot_next_component_manager: bool,
}

/// Platform-provided tools for development and debugging.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ToolsConfig {
    /// Tools for audio.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub audio: AudioToolsConfig,

    /// Tools for connectivity.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub connectivity: ConnectivityToolsConfig,

    /// Tools for storage.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub storage: StorageToolsConfig,
}

/// Platform-provided tools for development and debugging of audio.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct AudioToolsConfig {
    /// Include tools for audio driver development, such as:
    ///   - 'audio-codec-ctl'
    ///   - 'audio-driver-ctl'
    ///   - 'virtual_audio_util'
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub driver_tools: bool,

    /// Include tools for debugging the audio_core service, such as:
    ///   - 'audio_listener'
    ///   - 'signal_generator'
    ///   - 'wav_recorder'
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub full_stack_tools: bool,
}

/// Platform-provided tools for development and debugging connectivity.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ConnectivityToolsConfig {
    /// Include tools for basic networking, such as:
    ///   - 'nc'
    ///   - 'iperf3'
    ///   - 'tcpdump'
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub enable_networking: bool,

    /// Include tools for wlan
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub enable_wlan: bool,

    /// Include tools for Thread
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub enable_thread: bool,
}

/// Platform-provided tools for the development and debugging of storage.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct StorageToolsConfig {
    /// Include tools used for disk partitioning, such as:
    ///   - 'mount'
    ///   - 'install-disk-image'
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub enable_partitioning_tools: bool,
}
