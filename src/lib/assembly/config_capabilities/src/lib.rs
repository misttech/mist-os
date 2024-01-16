// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![deny(missing_docs)]

//! Library for constructing the configuration capability package.
//!
//! The configuration capability package is a Fuchsia package that holds the
//! platform's configuration. It contains a single CML file that is used in the
//! topology at `/config`. Configuration capabilities are routed from it to
//! the platform components.

mod config_capabilities;

pub use crate::config_capabilities::build_config_capability_package;
pub use crate::config_capabilities::CapabilityNamedMap;
pub use crate::config_capabilities::Config;
pub use crate::config_capabilities::ConfigNestedValueType;
pub use crate::config_capabilities::ConfigValueType;
