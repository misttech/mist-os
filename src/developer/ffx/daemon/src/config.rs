// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use errors::ffx_error;
use ffx_config::EnvironmentContext;
use std::path::PathBuf;
use std::time::Duration;

/// An extension trait for [`ffx_config::EnvironmentContext`] to get daemon and
/// proxy settings.
#[async_trait::async_trait]
pub trait DaemonConfig {
    const OVERNET_SOCKET_KEY: &'static str = "overnet.socket";
    const PROXY_TIMEOUT_KEY: &'static str = "proxy.timeout_secs";

    /// Returns the path to the configured daemon socket, or the default for this environment
    /// if none is configured.
    async fn get_ascendd_path(&self) -> Result<PathBuf>;

    /// Returns the proxy timeout as defined in the config under "proxy.timeout_secs"
    ///
    /// Will return an error if this value is not found in the config.
    async fn get_proxy_timeout(&self) -> Result<Duration>;
}

#[async_trait::async_trait]
impl DaemonConfig for EnvironmentContext {
    async fn get_ascendd_path(&self) -> Result<PathBuf> {
        match self.get(Self::OVERNET_SOCKET_KEY).await {
            Ok(path) => Ok(path),
            _ => self.get_default_ascendd_path(),
        }
    }

    async fn get_proxy_timeout(&self) -> Result<Duration> {
        self.get(Self::PROXY_TIMEOUT_KEY)
            .await
            .with_context(|| ffx_error!("Unable to load proxy timeout"))
            .map(Duration::from_secs_f64)
    }
}
