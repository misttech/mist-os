// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Definitions and tests of metadata for an FFX-managed tool in the SDK.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::common::{CpuArchitecture, ElementType, File};
use crate::json::JsonObject;

/// A 'tool' that is managed and run by ffx as a subcommand.
///
/// Further definitions of the individual fields can be found in
/// the json schema definition at [`../ffx_tool.json`] and related
/// files.
///
/// This is intended to be largely the same structure as a normal
/// [`crate::HostTool`], but with two additional elements that point
/// to the specific file ffx should run ([`FfxTool::executable`]) and
/// the versioning metadata file ([`FfxTool::executable_metadata`]).
/// These paths should also appear in the [`FfxTool::files`] listing,
/// but that listing can include files other files as well.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct FfxTool {
    pub name: String,
    pub root: File,
    #[serde(rename = "type")]
    pub kind: ElementType,
    pub files: FfxToolFiles,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_files: Option<HashMap<CpuArchitecture, Vec<String>>>,
}

impl JsonObject for FfxTool {
    fn get_schema() -> &'static str {
        include_str!("../ffx_tool.json")
    }
    fn get_referenced_schemata() -> &'static [&'static str] {
        &[crate::json::schema::COMMON, include_str!("../host_tool.json")]
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FfxToolFiles {
    pub executable: File,
    pub executable_metadata: File,
}

#[cfg(test)]
mod tests {
    use super::FfxTool;

    test_validation! {
        name = test_validation,
        kind = FfxTool,
        data = r#"
        {
            "name": "foobar",
            "type": "ffx_tool",
            "root": "ffx_tools/foobar",
            "files": {
                "executable": "ffx_tools/foobar/one",
                "executable_metadata": "ffx_tools/foobar/one.json",
                "support": [
                    "ffx_tools/foobar/one",
                    "ffx_tools/foobar/one.json"
                ]
            },
            "target_files": {
                "x64": [
                    "ffx_tools/foobar/foobar_x64"
                ]
            }
        }
        "#,
        valid = true,
    }

    test_validation! {
        name = test_validation_invalid,
        kind = FfxTool,
        data = r#"
        {
            "name": "foobar",
            "type": "cc_prebuilt_library",
            "root": "tools/foobar",
            "files": {
                "executable": "ffx_tools/foobar/one",
                "executable_metadata": "ffx_tools/foobar/one.json",
                "support": [
                    "tools/foobar/one",
                    "tools/foobar/two"
                ]
            },
            "target_files": {
                "x64": [
                    "tools/foobar/foobar_x64"
                ]
            }
        }
        "#,
        // Type is invalid.
        valid = false,
    }
}
