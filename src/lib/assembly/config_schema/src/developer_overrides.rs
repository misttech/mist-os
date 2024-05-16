// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::{Deserialize, Serialize};

use crate::assembly_config::CompiledPackageDefinition;
use crate::PackageDetails;

/// Developer Overrides struct that is similar to the AssemblyConfig struct,
/// but has extra fields added that allow it to convey extra fields.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DeveloperOverrides {
    /// The label of the target used to define the overrides.
    pub target_name: Option<String>,

    /// Special overrides-only flags to pass to assembly.  These features cannot
    /// be used by products, only by developers that need to override the standard
    /// behavior of assembly.
    ///
    /// Using these will generate warnings.
    #[serde(default)]
    pub developer_only_options: DeveloperOnlyOptions,

    /// Developer overrides for the kernel.
    ///
    /// Using these will generate warnings.
    #[serde(default)]
    pub kernel: KernelOptions,

    /// Developer overrides for the platform configuration.
    ///
    /// This is a 'Value' so that it can be be used to overlay the product's
    /// platform configuration before that's parsed into it's real type.
    #[serde(default)]
    pub platform: serde_json::Value,

    /// Packages to add to the build.
    #[serde(default)]
    pub packages: Vec<PackageDetails>,

    /// Compiled components to add to the build
    #[serde(default)]
    pub packages_to_compile: Vec<CompiledPackageDefinition>,
}

/// Special flags for assembly that can only be used in the context of developer
/// overrides.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DeveloperOnlyOptions {
    /// Force all non-bootfs packages known to assembly to be in the base package
    /// set (cache, universe, etc.).
    ///
    /// This feature exists to enable the use of a product image that has cache
    /// or universe packages in a context where networking is unavailable or
    /// a package server cannot be run.
    #[serde(default)]
    pub all_packages_in_base: bool,
}

/// Kernel options and settings that are only to be used in the context of local
/// developer overrides.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct KernelOptions {
    /// Additional kernel command line args to add to the assembled ZBI.
    #[serde(default)]
    pub command_line_args: Vec<String>,
}
