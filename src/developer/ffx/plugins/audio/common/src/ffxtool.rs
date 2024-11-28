// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use ffx_command::Result;
use fho::{FhoEnvironment, TryFromEnv, TryFromEnvWith};
use fidl::endpoints::{DiscoverableProtocolMarker, Proxy};
use fidl_fuchsia_developer_remotecontrol::RemoteControlProxy;
use fidl_fuchsia_io as fio;
use std::marker::PhantomData;
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
        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
        // It would be better to use connect_to_rcs that retries, but it's private.
        let rcs = RemoteControlProxy::try_from_env(env).await?;
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

/// The implementation of the decorator returned by [`optional_moniker`].
pub struct OptionalWithToolbox<P> {
    backup: Option<String>,
    _p: PhantomData<fn() -> P>,
}

#[async_trait(?Send)]
impl<P> TryFromEnvWith for OptionalWithToolbox<P>
where
    P: Proxy + 'static,
    P::Protocol: DiscoverableProtocolMarker,
{
    type Output = Option<P>;

    async fn try_from_env_with(self, env: &FhoEnvironment) -> Result<Self::Output> {
        // It would be better to use connect_to_rcs that retries, but it's private.
        let rcs = RemoteControlProxy::try_from_env(env).await?;
        let output = match rcs::toolbox::connect_with_timeout::<P::Protocol>(
            &rcs,
            self.backup.as_ref(),
            DEFAULT_PROXY_TIMEOUT,
        )
        .await
        {
            Ok(proxy) => Some(proxy),
            Err(err) => {
                tracing::debug!(%err, "Protocol {} is unavailable", P::Protocol::PROTOCOL_NAME);
                None
            }
        };
        Ok(output)
    }
}

/// Connects to an optional protocol that may be exposed by the toolbox
/// or the component with the given moniker.
///
/// Essentially, this is the optional version of `fho::moniker`.
///
/// If the component with the moniker does not exist or fails to connect,
/// the field is set to None.
pub fn optional_moniker<P: Proxy>(or_moniker: impl Into<String>) -> OptionalWithToolbox<P> {
    OptionalWithToolbox { backup: Some(or_moniker.into()), _p: PhantomData::default() }
}
