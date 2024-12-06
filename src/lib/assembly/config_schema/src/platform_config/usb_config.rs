// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Platform configuration options for usb.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct UsbConfig {
    /// Set this if the platform has a USB peripheral device that needs to be configured.
    pub peripheral: UsbPeripheralConfig,
}

/// Configure how the USB peripheral subsystem should work.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct UsbPeripheralConfig {
    /// Optional list of functions that will be published by the USB peripheral driver.
    /// See |UsbPeripheralFunction| for the list of supported functions.
    /// If this is `None`, |UsbPeripheralFunction::Cdc| shall be set as the default.
    pub functions: Option<Vec<UsbPeripheralFunction>>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum UsbPeripheralFunction {
    Adb,
    Cdc,
    Fastboot,
    Overnet,
    Rndis,
    Test,
    Ums,
}

impl std::fmt::Display for UsbPeripheralFunction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UsbPeripheralFunction::Adb => write!(f, "adb"),
            UsbPeripheralFunction::Cdc => write!(f, "cdc"),
            UsbPeripheralFunction::Fastboot => write!(f, "fastboot"),
            UsbPeripheralFunction::Overnet => write!(f, "overnet"),
            UsbPeripheralFunction::Rndis => write!(f, "rndis"),
            UsbPeripheralFunction::Test => write!(f, "test"),
            UsbPeripheralFunction::Ums => write!(f, "ums"),
        }
    }
}
