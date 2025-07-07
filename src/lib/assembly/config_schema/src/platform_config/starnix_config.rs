// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use derivative::Derivative;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Platform configuration options for the starnix area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct PlatformStarnixConfig {
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub enabled: bool,

    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub enable_android_support: bool,

    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub socket_mark: SocketMarkTreatment,

    // TODO(https://fxbug.dev/387998791): Remove this flag after making
    // RtnetlinkTreatmentOfIbf0Interface::NoProvideFake the default behavior.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub rtnetlink_ifb0: RtnetlinkTreatmentOfIfb0Interface,

    // Whether the network manager feature should be enabled.
    // Should be enabled alongside `SocketMarkTreatment` and
    // the `include_socket_proxy` assembly argument.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub network_manager: NetworkManagerTreatment,
}

/// How starnix treats socket marks.
#[derive(Debug, Deserialize, Serialize, PartialEq, JsonSchema, Derivative)]
#[derivative(Default)]
#[serde(rename_all = "snake_case")]
pub enum SocketMarkTreatment {
    /// Marks are tracked internally in starnix.
    #[derivative(Default)]
    StarnixOnly,
    /// Marks are propagated to the networking stack.
    SharedWithNetstack,
}

/// How the rtnetlink worker in starnix treats the ifb0 interface.
#[derive(Debug, Deserialize, Serialize, PartialEq, JsonSchema, Derivative)]
#[derivative(Default)]
#[serde(rename_all = "snake_case")]
pub enum RtnetlinkTreatmentOfIfb0Interface {
    /// Provide a fake implementation of that interface such that common netlink commands work
    /// against it (e.g. installing and listing addresses).
    #[derivative(Default)]
    ProvideFake,
    /// Do not provide any fake interface implementation.
    NoProvideFake,
}

/// Whether the network manager feature should be included in the Starnix container.
#[derive(Debug, Deserialize, Serialize, PartialEq, JsonSchema, Derivative)]
#[derivative(Default)]
#[serde(rename_all = "snake_case")]
pub enum NetworkManagerTreatment {
    /// Do not include the network manager feature.
    #[derivative(Default)]
    Disabled,
    /// Include the network manager feature.
    Enabled,
}
