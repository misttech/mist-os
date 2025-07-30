// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use assembly_container::{assembly_container, AssemblyContainer, WalkPaths};
use assembly_release_info::ReleaseInfo;
use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};

/// A container for the platform artifacts.
/// This is different from "platform_config.rs", which holds the platform settings within a product
/// config.
///
/// This is created from the script `//build/assembly/scripts/generate_platform_artifacts.py`.
#[derive(Debug, Deserialize, Serialize, WalkPaths)]
#[serde(deny_unknown_fields)]
#[assembly_container(platform_artifacts.json)]
pub struct PlatformArtifacts {
    /// Root directory where all platform input bundles reside.
    #[serde(skip)]
    pub platform_input_bundle_dir: Utf8PathBuf,

    /// Release information for the platform artifacts.
    pub release_info: ReleaseInfo,
}

impl PlatformArtifacts {
    pub fn from_dir_with_path(dir: impl AsRef<Utf8Path>) -> Result<Self> {
        let dir = dir.as_ref();
        let mut artifacts = Self::from_dir(dir)?;
        artifacts.platform_input_bundle_dir = dir.into();
        Ok(artifacts)
    }

    pub fn get_resources(&self) -> Utf8PathBuf {
        self.platform_input_bundle_dir.join("resources")
    }

    pub fn get_bundle(&self, bundle_name: impl AsRef<str>) -> Utf8PathBuf {
        self.platform_input_bundle_dir.join(bundle_name.as_ref()).join("assembly_config.json")
    }
}
