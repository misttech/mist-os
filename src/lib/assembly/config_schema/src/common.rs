// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assembly_container::{FileType, WalkPaths};
use assembly_file_relative_path::{FileRelativePathBuf, SupportsFileRelativePaths};
use camino::Utf8PathBuf;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub fn path_schema(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
    let mut schema: schemars::schema::SchemaObject = <String>::json_schema(gen).into();
    schema.format = Some("Utf8PathBuf".to_owned());
    schema.into()
}

pub fn vec_path_schema(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
    let mut schema: schemars::schema::SchemaObject = <Vec<String>>::json_schema(gen).into();
    schema.format = Some("Vec<Utf8PathBuf>".to_owned());
    schema.into()
}

pub fn option_path_schema(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
    let mut schema: schemars::schema::SchemaObject = <Option<String>>::json_schema(gen).into();
    schema.format = Some("Option<Utf8PathBuf>".to_owned());
    schema.into()
}

/// These are the package sets that a package can belong to.
///
/// See RFC-0212 "Package Sets" for more information on these:
/// https://fuchsia.dev/fuchsia-src/contribute/governance/rfcs/0212_package_sets
///
/// NOTE: Not all of the sets defined in the RFC are currently supported by this
/// enum.  They are being added as they are needed by assembly.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PackageSet {
    /// The packages in this set are stored in the pkg-cache, and are not
    /// garbage collected.  They are always available, and are pinned by merkle
    /// when the system is assembled.
    ///
    /// They cannot be updated without performing an OTA of the system.
    Base,

    /// The contents of the cache package set are present on the device in
    /// nearly all circumstances but the version may be updated in some
    /// circumstances during local development. This package set is not used
    /// in production.
    Cache,

    /// The packages in this set are placed in one of the other package sets by
    /// assembly based on the assembly context.
    Flexible,

    /// The packages in this set are merged into the "base" package
    /// (system image) to make them available to the software delivery
    /// subsystem while the system is booting up.
    System,

    /// The packages in this set are stored in the BootFS in the zbi.  They are
    /// always available (via `fuchsia-boot:///<name>` pkg urls), and are pinned
    /// by merkle when the ZBI is created.
    ///
    /// They cannot be updated without performing an OTA of the system.
    Bootfs,

    /// The on-demand packages are packages that are known to assembly, but are
    /// not part of the assembled image itself.  These will not be included in
    /// the product images unless developer overrides push them into the base
    /// package set.
    ///
    /// Note: This was previously the "universe" package set, and RFC-0212
    /// refined this as the "on-demand;[anchored|updateable]" package set. No
    /// anchoring (merkle-pinning) is done at this time.
    /// see: https://fuchsia.dev/fuchsia-src/contribute/governance/rfcs/0212_package_sets?hl=en#change-7
    OnDemand,
}

impl std::fmt::Display for PackageSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PackageSet::Base => "base",
            PackageSet::Cache => "cache",
            PackageSet::Flexible => "flexible",
            PackageSet::System => "system",
            PackageSet::Bootfs => "bootfs",
            PackageSet::OnDemand => "on_demand",
        })
    }
}

/// Details about a package that contains drivers.
#[derive(
    Clone, Debug, Deserialize, Serialize, PartialEq, JsonSchema, SupportsFileRelativePaths,
)]
#[serde(deny_unknown_fields)]
pub struct DriverDetails {
    /// The package containing the driver.
    #[schemars(schema_with = "path_schema")]
    #[file_relative_paths]
    pub package: FileRelativePathBuf,

    /// The driver components within the package, e.g. meta/foo.cm.
    #[schemars(schema_with = "vec_path_schema")]
    pub components: Vec<Utf8PathBuf>,
}

impl WalkPaths for DriverDetails {
    fn walk_paths_with_dest<F: assembly_container::WalkPathsFn>(
        &mut self,
        found: &mut F,
        dest: Utf8PathBuf,
    ) -> anyhow::Result<()> {
        found(self.package.as_mut_utf8_pathbuf(), dest.join("package"), FileType::PackageManifest)?;
        Ok(())
    }
}

/// This defines one or more drivers in a package, and which package set they
/// belong to.
#[derive(
    Clone, Debug, Deserialize, Serialize, PartialEq, SupportsFileRelativePaths, JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct PackagedDriverDetails {
    /// The package containing the driver.
    pub package: FileRelativePathBuf,

    /// Which set this package belongs to.
    pub set: PackageSet,

    /// The driver components within the package, e.g. meta/foo.cm.
    #[schemars(schema_with = "vec_path_schema")]
    pub components: Vec<Utf8PathBuf>,
}

/// This defines a package, and which package set it belongs to.
#[derive(
    Clone, Debug, Deserialize, Serialize, PartialEq, SupportsFileRelativePaths, JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct PackageDetails {
    /// A package to add.
    pub package: FileRelativePathBuf,

    /// Which set this package belongs to.
    pub set: PackageSet,
}

/// A typename to clarify intent around what Strings are package names.
pub(crate) type PackageName = String;

/// Options for features that may either be forced on, forced off, or allowed
/// to be either on or off. Features default to disabled.
#[derive(Debug, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
#[derive(Default)]
pub enum FeatureControl {
    #[default]
    Disabled,

    Allowed,

    Required,
}

impl PartialEq<FeatureControl> for &FeatureControl {
    fn eq(&self, other: &FeatureControl) -> bool {
        self.eq(&other)
    }
}
