// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod assembly_config;
pub mod board_config;
pub mod common;
pub mod image_assembly_config;
pub mod platform_config;
pub mod product_config;

pub use assembly_config::AssemblyConfig;
pub use board_config::BoardInformation;
pub use common::{DriverDetails, FeatureControl, FileEntry};
pub use image_assembly_config::{ImageAssemblyConfig, PartialImageAssemblyConfig};
pub use platform_config::{
    example_config::ExampleConfig,
    icu_config::{ICUConfig, Revision},
    BuildType, FeatureSupportLevel,
};
