// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use ffx_command::Result;
use fho::{FhoEnvironment, TryFromEnvWith};
use fidl_fuchsia_io as fio;
use std::time::Duration;

const DEFAULT_PROXY_TIMEOUT: Duration = Duration::from_secs(15);

/// Connector for FfxTool fields that creates a DirectoryProxy
/// for a directory capability exposed by a component.
pub struct WithExposedDir {
    moniker: String,
    capability_name: String,
}

#[async_trait(?Send)]
impl TryFromEnvWith for WithExposedDir {
    type Output = fio::DirectoryProxy;

    async fn try_from_env_with(self, env: &FhoEnvironment) -> Result<Self::Output> {
        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
        let rcs = env.injector.remote_factory().await?;
        rcs::open_with_timeout_at(
            DEFAULT_PROXY_TIMEOUT,
            &self.moniker,
            rcs::OpenDirType::ExposedDir,
            &self.capability_name,
            &rcs,
            server_end.into(),
        )
        .await?;
        Ok(proxy)
    }
}

/// Connects to the directory capability exposed by the component at `moniker`.
pub fn exposed_dir(
    moniker: impl Into<String>,
    capability_name: impl Into<String>,
) -> WithExposedDir {
    WithExposedDir { moniker: moniker.into(), capability_name: capability_name.into() }
}
