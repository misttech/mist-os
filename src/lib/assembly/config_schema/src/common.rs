// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

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
#[serde(deny_unknown_fields)]
#[derive(Default)]
pub enum FeatureControl {
    #[serde(rename = "disabled")]
    #[default]
    Disabled,

    #[serde(rename = "allowed")]
    Allowed,

    #[serde(rename = "required")]
    Required,
}

impl PartialEq<FeatureControl> for &FeatureControl {
    fn eq(&self, other: &FeatureControl) -> bool {
        self.eq(&other)
    }
}
