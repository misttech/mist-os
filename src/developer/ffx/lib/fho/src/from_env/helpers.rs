// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::FhoEnvironment;
use ffx_command::{Error, FfxContext, Result};
use fidl::endpoints::{DiscoverableProtocolMarker, Proxy, ServerEnd};
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
        let res = env.injector.remote_factory().await;
        if res.is_ok() || tries > retry_count {
            break res;
        }
    }
    .with_user_message(|| {
        format!("Failed to connect to remote control protocol on target after {retry_count} tries. Is the target device connected and functioning?")
    })
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

pub async fn load_daemon_protocol<P>(env: &FhoEnvironment) -> Result<P>
where
    P: Proxy + Clone + 'static,
    P::Protocol: DiscoverableProtocolMarker,
{
    let svc_name = <P::Protocol as DiscoverableProtocolMarker>::PROTOCOL_NAME;
    let daemon = env.injector.daemon_factory().await.map_err(|err| anyhow::Error::from(err))?;
    let (proxy, server_end) = create_proxy()?;

    daemon
        .connect_to_protocol(svc_name, server_end.into_channel())
        .await
        .bug_context("Connecting to protocol")?
        .map_err(|err| Error::User(errors::map_daemon_error(svc_name, err)))?;

    Ok(proxy)
}
