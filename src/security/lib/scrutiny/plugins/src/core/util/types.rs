// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Structs used in parsing packages

use {
    cm_fidl_analyzer::{match_absolute_pkg_urls, PkgUrlMatch},
    fuchsia_merkle::Hash,
    fuchsia_url::AbsolutePackageUrl,
    std::{collections::HashMap, path::PathBuf},
    tracing::warn,
};

pub type Protocol = String;

/// Package- and component-related data extracted from a package identified by a
/// fully-qualified fuchsia package URL.
#[cfg_attr(test, derive(Clone))]
pub struct PackageDefinition {
    /// The URL from which the definition was extracted.
    pub url: AbsolutePackageUrl,
    /// A mapping from internal package paths to merkle root hashes of content
    /// (that is non-meta) files designated in the package meta.far.
    pub contents: HashMap<PathBuf, Hash>,
    /// A mapping from internal package meta paths to meta file contents.
    pub meta: HashMap<PathBuf, Vec<u8>>,
    /// A mapping from internal package paths to component manifest data.
    pub cms: HashMap<PathBuf, ComponentManifest>,
    /// A mapping from internal package paths to config value files.
    pub cvfs: HashMap<String, Vec<u8>>,
}

impl PackageDefinition {
    pub fn new(url: AbsolutePackageUrl, partial: PartialPackageDefinition) -> Self {
        Self {
            url,
            contents: partial.contents,
            meta: partial.meta,
            cms: partial.cms,
            cvfs: partial.cvfs,
        }
    }

    pub fn matches_url(&self, url: &AbsolutePackageUrl) -> bool {
        let url_match = match_absolute_pkg_urls(&self.url, url);
        if url_match == PkgUrlMatch::WeakMatch {
            warn!(
                PkgDefinition.url = %self.url,
                other_url = %url,
                "Lossy match of absolute package URLs",
            );
        }
        url_match != PkgUrlMatch::NoMatch
    }
}

/// Package- and component-related data extracted from an package.
#[derive(Default)]
#[cfg_attr(test, derive(Clone))]
pub struct PartialPackageDefinition {
    /// A mapping from internal package paths to merkle root hashes of content
    /// (that is non-meta) files designated in the package meta.far.
    pub contents: HashMap<PathBuf, Hash>,
    /// A mapping from internal package meta paths to meta file contents.
    pub meta: HashMap<PathBuf, Vec<u8>>,
    /// A mapping from internal package paths to component manifest data.
    pub cms: HashMap<PathBuf, ComponentManifest>,
    /// A mapping from internal package paths to config value files.
    pub cvfs: HashMap<String, Vec<u8>>,
}

// TODO(https://fxbug.dev/42083956): Use cm_rust type or ComponentDecl type.
#[allow(dead_code)]
#[cfg_attr(test, derive(Clone))]
pub enum ComponentManifest {
    Empty,
    Version2(Vec<u8>),
}

impl From<Vec<u8>> for ComponentManifest {
    fn from(other: Vec<u8>) -> Self {
        ComponentManifest::Version2(other)
    }
}
