// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use assembly_util::NamedMap;
use camino::Utf8PathBuf;
use fuchsia_pkg::PackageManifest;
use serde::Serialize;

/// Information of a single package in the set.
#[derive(Debug, Serialize)]
pub struct PackageEntry {
    /// Path to the package manifest.
    pub path: Utf8PathBuf,
    /// Parsed package manifest.
    pub manifest: PackageManifest,
}

impl PackageEntry {
    /// Construct a PackageEntry from a path to a package manifest.
    pub fn parse_from(path: impl Into<Utf8PathBuf>) -> Result<Self> {
        let path = path.into();
        let manifest = PackageManifest::try_load_from(&path)
            .with_context(|| format!("parsing {path} as a package manifest"))?;
        Ok(Self { path, manifest })
    }

    /// Retrieve the package name.
    pub fn name(&self) -> &str {
        self.manifest.name().as_ref()
    }
}

/// A named set of packages with their manifests parsed into memory, keyed by package name.
#[derive(Debug, Serialize)]
pub struct PackageSet {
    /// Map of packages keyed by the package name.
    map: NamedMap<PackageEntry>,
}

impl PackageSet {
    /// Construct a PackageSet.
    pub fn new(name: &str) -> Self {
        PackageSet { map: NamedMap::new(name) }
    }

    /// Add the package described by the ProductPackageSetEntry to the
    /// PackageSet
    pub fn add_package(&mut self, entry: PackageEntry) -> Result<()> {
        self.map.try_insert_unique(entry.name().to_owned(), entry)
    }

    /// Parse the given path as a PackageManifest, and add it to the PackageSet.
    pub fn add_package_from_path<P: Into<Utf8PathBuf>>(&mut self, path: P) -> Result<()> {
        {
            let entry = PackageEntry::parse_from(path)?;
            self.add_package(entry)
        }
        .with_context(|| format!("Adding package to set: {}", self.map.name))
    }

    /// Convert the PackageSet into an iterable collection of Paths.
    pub fn into_paths(self) -> impl Iterator<Item = Utf8PathBuf> {
        self.map.entries.into_values().map(|e| e.path)
    }

    /// Convert self into an interator of entries.
    pub fn into_values(self) -> impl Iterator<Item = PackageEntry> {
        self.map.entries.into_values()
    }
}

impl std::ops::Deref for PackageSet {
    type Target = NamedMap<PackageEntry>;

    fn deref(&self) -> &Self::Target {
        &self.map
    }
}

impl std::ops::DerefMut for PackageSet {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.map
    }
}
