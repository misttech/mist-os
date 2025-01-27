// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::init_daemon_behavior;
use async_trait::async_trait;
use ffx_command_error::{user_error, Error, FfxContext as _, Result};
use fho::{FhoEnvironment, TryFromEnv, TryFromEnvWith};
use fidl::endpoints::{DiscoverableProtocolMarker, Proxy, ServerEnd};
use fidl_fuchsia_developer_ffx as ffx_fidl;
use std::marker::PhantomData;
use std::ops::Deref;

#[derive(Clone, Debug)]
pub struct DaemonProxyHolder(ffx_fidl::DaemonProxy);

impl Deref for DaemonProxyHolder {
    type Target = ffx_fidl::DaemonProxy;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<ffx_fidl::DaemonProxy> for DaemonProxyHolder {
    fn from(value: ffx_fidl::DaemonProxy) -> Self {
        DaemonProxyHolder(value)
    }
}

#[async_trait(?Send)]
impl TryFromEnv for DaemonProxyHolder {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        if env.behavior().await.is_none() {
            let b = init_daemon_behavior(env.environment_context()).await?;
            env.set_behavior(b.clone()).await;
        }
        // Might need to revisit whether it's necessary to cast every daemon_factory() invocation
        // into a user error. This line originally casted every error into "Failed to create daemon
        // proxy", which obfuscates the original error.
        env.injector::<Self>()
            .await?
            .daemon_factory()
            .await
            .map(Into::into)
            .map_err(|e| user_error!("{}", e))
    }
}

#[derive(Debug, Clone)]
pub struct DaemonProtocol<P: Clone>(P);

#[derive(Debug, Clone, Default)]
pub struct WithDaemonProtocol<P>(PhantomData<fn() -> P>);

impl<P: Clone> std::ops::Deref for DaemonProtocol<P> {
    type Target = P;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[async_trait(?Send)]
impl<P> TryFromEnv for DaemonProtocol<P>
where
    P: Proxy + Clone + 'static,
    P::Protocol: DiscoverableProtocolMarker,
{
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        load_daemon_protocol(env).await.map(DaemonProtocol)
    }
}

#[async_trait(?Send)]
impl<P> TryFromEnvWith for WithDaemonProtocol<P>
where
    P: Proxy + Clone + 'static,
    P::Protocol: DiscoverableProtocolMarker,
{
    type Output = P;
    async fn try_from_env_with(self, env: &FhoEnvironment) -> Result<P> {
        load_daemon_protocol(env).await
    }
}

/// A decorator for daemon proxies.
///
/// Example:
///
/// ```rust
/// #[derive(FfxTool)]
/// struct Tool {
///     #[with(fho::daemon_protocol())]
///     foo_proxy: FooProxy,
/// }
/// ```
pub fn daemon_protocol<P>() -> WithDaemonProtocol<P> {
    WithDaemonProtocol(Default::default())
}

async fn load_daemon_protocol<P>(env: &FhoEnvironment) -> Result<P>
where
    P: Proxy + Clone + 'static,
    P::Protocol: DiscoverableProtocolMarker,
{
    let svc_name = <P::Protocol as DiscoverableProtocolMarker>::PROTOCOL_NAME;
    let daemon = DaemonProxyHolder::try_from_env(env).await?;
    let (proxy, server_end) = create_proxy().bug_context("creating proxy")?;

    daemon
        .connect_to_protocol(svc_name, server_end.into_channel())
        .await
        .bug_context("Connecting to protocol")?
        .map_err(|err| Error::User(target_errors::map_daemon_error(svc_name, err)))?;

    Ok(proxy)
}

fn create_proxy<P>() -> Result<(P, ServerEnd<P::Protocol>)>
where
    P: Proxy + 'static,
    P::Protocol: DiscoverableProtocolMarker,
{
    Ok(fidl::endpoints::create_proxy::<P::Protocol>())
}
