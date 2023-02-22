// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This module holds the common data types for emulator engines. These are implementation-agnostic
//! data types, not the engine-specific command types that each engine will define for itself. These
//! types will be directly deserializable from the PBM, and converted into engine-specific types at
//! runtime.

use sdk_metadata::display_impl;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EngineConsoleType {
    /// An emulator console for issuing commands to the emulation hypervisor.
    Command,

    /// An emulator console designed for machine-to-machine communication through a structured
    /// language such as JSON.
    Machine,

    /// An emulator console for communicating with the virtual serial port.
    Serial,

    /// A default value indicating none of the above.
    None,
}

impl Default for EngineConsoleType {
    fn default() -> Self {
        EngineConsoleType::None
    }
}

display_impl!(EngineConsoleType);

/// Indicates which details the "show" command should return.
#[derive(Clone, Debug, PartialEq)]
pub enum ShowDetail {
    All,
    Cmd,
    Config,
    Device,
    Net,
    Raw,
}

impl Default for ShowDetail {
    fn default() -> Self {
        ShowDetail::All
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_show_detail() {
        // Verify it returns a default.
        let _default = ShowDetail::default();
    }
}
