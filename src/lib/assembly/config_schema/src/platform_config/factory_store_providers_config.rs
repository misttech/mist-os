// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assembly_container::WalkPaths;
use assembly_file_relative_path::{FileRelativePathBuf, SupportsFileRelativePaths};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Platform configuration options for the factory store providers
#[derive(
    Debug,
    Default,
    Deserialize,
    Serialize,
    PartialEq,
    JsonSchema,
    SupportsFileRelativePaths,
    WalkPaths,
)]
#[serde(default, deny_unknown_fields)]
pub struct FactoryStoreProvidersConfig {
    #[file_relative_paths]
    #[walk_paths]
    pub alpha: Option<FileRelativePathBuf>,

    #[file_relative_paths]
    #[walk_paths]
    pub cast_credentials: Option<FileRelativePathBuf>,

    #[file_relative_paths]
    #[walk_paths]
    pub misc: Option<FileRelativePathBuf>,

    #[file_relative_paths]
    #[walk_paths]
    pub play_ready: Option<FileRelativePathBuf>,

    #[file_relative_paths]
    #[walk_paths]
    pub weave: Option<FileRelativePathBuf>,

    #[file_relative_paths]
    #[walk_paths]
    pub widevine: Option<FileRelativePathBuf>,
}
