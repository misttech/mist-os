// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Release information for a single assembly input.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseInfo {
    /// Name of this input artifact.
    pub name: String,

    /// Origin repository of this input artifact.
    pub repository: String,

    /// Version of this input artifact.
    pub version: String,
}

/// Release information for boards and their associated BIB sets.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BoardReleaseInfo {
    /// Name of this input artifact.
    pub info: ReleaseInfo,

    /// Name of this input artifact.
    pub bib_sets: Vec<ReleaseInfo>,
}

/// Release information for products and their associated product input bundles.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProductReleaseInfo {
    /// Name of this input artifact.
    pub info: ReleaseInfo,

    /// Name of this input artifact.
    pub pibs: Vec<ReleaseInfo>,
}

/// Release information for an assembly image.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SystemReleaseInfo {
    /// Platform release information.
    pub platform: Option<ReleaseInfo>,

    /// Product release information.
    pub product: Option<ProductReleaseInfo>,

    /// Board release information.
    pub board: Option<BoardReleaseInfo>,
}

/// Release information for all assembly artifacts that contributed to a product bundle.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProductBundleReleaseInfo {
    /// Product Bundle name.
    pub name: String,

    /// Product Bundle version.
    pub version: String,

    /// Release information for slot A.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_a: Option<SystemReleaseInfo>,

    /// Release information for slot B.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_b: Option<SystemReleaseInfo>,

    /// Release information for slot R.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_r: Option<SystemReleaseInfo>,
}
