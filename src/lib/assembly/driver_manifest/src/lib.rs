// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![deny(missing_docs)]

//! Library for constructing driver manifests.

mod driver_manifest;

pub use driver_manifest::DriverManifest;
pub use driver_manifest::DriverManifestBuilder;
pub use driver_manifest::DriverPackageType;

/// The path to the base driver manifest in /boot.
pub const BASE_DRIVER_MANIFEST_PATH: &str = "config/driver_index/base_driver_manifest";
/// The path to the boot driver manifest in /boot.
pub const BOOT_DRIVER_MANIFEST_PATH: &str = "config/driver_index/boot_driver_manifest";
