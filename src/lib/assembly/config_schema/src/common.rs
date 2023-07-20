// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

/// These are the package sets that a package can belong to.
///
/// See RFC-0212 "Package Sets" for more information on these:
/// https://fuchsia.dev/fuchsia-src/contribute/governance/rfcs/0212_package_sets
///
/// NOTE: Not all of the sets defined in the RFC are currently supported by this
/// enum.  They are being added as they are needed by assembly.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PackageSet {
    /// The packages in this set are stored in the pkg-cache, and are not
    /// garbage collected.  They are always available, and are pinned by merkle
    /// when the system is assembled.
    ///
    /// They cannot be updated without performing an OTA of the system.
    Base,

    /// The packages in this set are stored in the BootFS in the zbi.  They are
    /// always available (via `fuchsia-boot:///<name>` pkg urls), and are pinned
    /// by merkle when the ZBI is created.
    ///
    /// They cannot be updated without performing an OTA of the system.
    BootFS,
}

/// Details about a package that contains drivers.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DriverDetails {
    /// The package containing the driver.
    pub package: Utf8PathBuf,

    /// The driver components within the package, e.g. meta/foo.cm.
    pub components: Vec<Utf8PathBuf>,
}

/// A mapping between a file source and destination.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct FileEntry {
    /// The path of the source file.
    pub source: Utf8PathBuf,

    /// The destination path to put the file.
    pub destination: String,
}

impl std::fmt::Display for FileEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "(src={}, dest={})", self.source, self.destination)
    }
}

/// A typename to clarify intent around what Strings are package names.
pub(crate) type PackageName = String;

/// Options for features that may either be forced on, forced off, or allowed
/// to be either on or off. Features default to disabled.
#[derive(Debug, Deserialize, Serialize, PartialEq)]
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
