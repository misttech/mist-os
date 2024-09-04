// Copyright 2024 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Configuration for how to run a trusted application in Fuchsia.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[allow(dead_code)]
#[serde(rename_all = "camelCase")]
pub struct TAConfig {
    /// The component url to run as the trusted application.
    pub url: String,
    /// Only create one instance of the trusted app and route all connections to it.
    single_instance: bool,
    /// Whether `single_instance` trusted apps support multiple separate sessions.
    // TODO: Support multiSession functionality.
    multi_session: bool,
    /// The trusted app should continue running even in low power states and suspension.
    // TODO: Support instanceKeepAlive functionality.
    instance_keep_alive: bool,
    /// Additional capabilities to pass to the component at `url`.
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
