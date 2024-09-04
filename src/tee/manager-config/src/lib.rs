// Copyright 2024 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Debug, Clone)]
#[allow(dead_code)]
#[serde(rename_all = "camelCase")]
pub struct TAConfig {
    pub url: String,
    single_instance: bool,
    // TODO: Support multiSession functionality.
    multi_session: bool,
    // TODO: Support instanceKeepAlive functionality.
    instance_keep_alive: bool,
    capabilities: Vec<()>,
}

impl TAConfig {
    pub fn new(url: String) -> Self {
        Self {
            url,
            single_instance: false,
            multi_session: false,
            instance_keep_alive: false,
            capabilities: vec![],
        }
    }

    pub fn parse_config(path: &PathBuf) -> Result<Self> {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| anyhow!("Could not read config file at {path:?}: {e}"))?;
        let parsed = serde_json::from_str(&contents)
            .map_err(|e| anyhow!("Could not deserialize {path:?} from json: {e}"))?;
        Ok(parsed)
    }
}
