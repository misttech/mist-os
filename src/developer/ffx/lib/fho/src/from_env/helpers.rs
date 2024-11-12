// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::FhoEnvironment;
use crate::from_env::TryFromEnv;
use fdomain_client::fidl::{
    DiscoverableProtocolMarker as FDiscoverableProtocolMarker, Proxy as FProxy,
};
use fdomain_fuchsia_developer_remotecontrol::RemoteControlProxy as FRemoteControlProxy;
use ffx_command::{Error, FfxContext, Result};
use fidl::endpoints::{DiscoverableProtocolMarker, Proxy, ServerEnd};
use fidl_fuchsia_developer_ffx::DaemonProxy;
use fidl_fuchsia_developer_remotecontrol::RemoteControlProxy;
use std::time::Duration;

pub fn create_proxy<P>() -> Result<(P, ServerEnd<P::Protocol>)>
where
    P: Proxy + 'static,
    P::Protocol: DiscoverableProtocolMarker,
{
    let protocol_name = P::Protocol::PROTOCOL_NAME; // for error messages.
    fidl::endpoints::create_proxy::<P::Protocol>()
        .with_bug_context(|| format!("Failed creating proxy for protocol '{protocol_name}'"))
}

pub async fn connect_to_rcs(env: &FhoEnvironment) -> Result<RemoteControlProxy> {
    let retry_count = 1;
    let mut tries = 0;
    // TODO(b/287693891): Remove explicit retries/timeouts here so they can be
    // configurable instead.
    loop {
        tries += 1;
        let res = RemoteControlProxy::try_from_env(env).await;
        if res.is_ok() || tries > retry_count {
            // Using `TryFromEnv` on `RemoteControlProxy` already contains user error information,
            // which will be propagated after exiting the loop.
            break Ok(res?);
        }
    }
}

pub async fn connect_to_rcs_fdomain(env: &FhoEnvironment) -> Result<FRemoteControlProxy> {
    let retry_count = 1;
    let mut tries = 0;
    // TODO(b/287693891): Remove explicit retries/timeouts here so they can be
    // configurable instead.
    loop {
        tries += 1;
        let res = FRemoteControlProxy::try_from_env(env).await;
        if res.is_ok() || tries > retry_count {
            // Using `TryFromEnv` on `RemoteControlProxy` already contains user error information,
            // which will be propagated after exiting the loop.
            break Ok(res?);
        }
    }
}

pub async fn open_moniker<P>(
    rcs: &RemoteControlProxy,
    capability_set: rcs::OpenDirType,
    moniker: &str,
    timeout: Duration,
) -> Result<P>
where
    P: Proxy + 'static,
    P::Protocol: DiscoverableProtocolMarker,
{
    let (proxy, server_end) = fidl::endpoints::create_proxy::<P::Protocol>().unwrap();
    rcs::open_with_timeout::<P::Protocol>(
        timeout,
        moniker,
        capability_set,
        rcs,
        server_end.into_channel(),
    )
    .await
    .with_user_message(|| {
        let protocol_name = P::Protocol::PROTOCOL_NAME;
        format!("Failed to connect to protocol '{protocol_name}' at moniker '{moniker}' within {} seconds", timeout.as_secs_f64())
    })?;
    Ok(proxy)
}

pub async fn open_moniker_fdomain<P>(
    rcs: &FRemoteControlProxy,
    capability_set: rcs_fdomain::OpenDirType,
    moniker: &str,
    timeout: Duration,
) -> Result<P>
where
    P: FProxy + 'static,
    P::Protocol: FDiscoverableProtocolMarker,
{
    let (proxy, server_end) = rcs
        .client()
        .map_err(|e| crate::Error::Unexpected(e.into()))?
        .create_proxy::<P::Protocol>()
        .await
        .unwrap();
    rcs_fdomain::open_with_timeout::<P::Protocol>(
        timeout,
        moniker,
        capability_set,
        rcs,
        server_end.into_channel(),
    )
    .await
    .with_user_message(|| {
        let protocol_name = P::Protocol::PROTOCOL_NAME;
        format!("Failed to connect to protocol '{protocol_name}' at moniker '{moniker}' within {} seconds", timeout.as_secs_f64())
    })?;
    Ok(proxy)
}

pub async fn load_daemon_protocol<P>(env: &FhoEnvironment) -> Result<P>
where
    P: Proxy + Clone + 'static,
    P::Protocol: DiscoverableProtocolMarker,
{
    let svc_name = <P::Protocol as DiscoverableProtocolMarker>::PROTOCOL_NAME;
    let daemon = DaemonProxy::try_from_env(env).await.map_err(|err| anyhow::Error::from(err))?;
    let (proxy, server_end) = create_proxy()?;

    daemon
        .connect_to_protocol(svc_name, server_end.into_channel())
        .await
        .bug_context("Connecting to protocol")?
        .map_err(|err| Error::User(errors::map_daemon_error(svc_name, err)))?;

    Ok(proxy)
}
