// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::{Deserialize, Serialize};

/// Specifies the configuration for the Bluetooth Snoop component (`bt-snoop`).
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Snoop {
    /// Don't include `bt-snoop`.
    #[default]
    None,
    /// Include `bt-snoop` with lazy startup.
    Lazy,
    /// Include `bt-snoop` with an eager startup during boot.
    Eager,
}

/// Platform configuration options for Bluetooth.
// TODO(b/292109810): Add more configuration options for core & profiles.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BluetoothConfig {
    #[serde(default)]
    pub snoop: Snoop,
}
