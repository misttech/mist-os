// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::common::FeatureControl;

/// Platform configuration options for the identity area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlatformIdentityConfig {
    /// Whether the pinweaver protocol should be enabled in `password_authenticator` on supported
    /// boards. Pinweaver will always be disabled if the board does not support the protocol.
    #[serde(default)]
    pub password_pinweaver: FeatureControl,
}
