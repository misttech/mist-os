// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assembly_container::WalkPaths;
use camino::Utf8PathBuf;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::common::option_path_schema;

/// Platform configuration options for the factory store providers
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema, WalkPaths)]
#[serde(default, deny_unknown_fields)]
pub struct FactoryStoreProvidersConfig {
    #[schemars(schema_with = "option_path_schema")]
    #[walk_paths]
    pub alpha: Option<Utf8PathBuf>,

    #[schemars(schema_with = "option_path_schema")]
    #[walk_paths]
    pub cast_credentials: Option<Utf8PathBuf>,

    #[schemars(schema_with = "option_path_schema")]
    #[walk_paths]
    pub misc: Option<Utf8PathBuf>,

    #[schemars(schema_with = "option_path_schema")]
    #[walk_paths]
    pub play_ready: Option<Utf8PathBuf>,

    #[schemars(schema_with = "option_path_schema")]
    #[walk_paths]
    pub weave: Option<Utf8PathBuf>,

    #[schemars(schema_with = "option_path_schema")]
    #[walk_paths]
    pub widevine: Option<Utf8PathBuf>,
}
