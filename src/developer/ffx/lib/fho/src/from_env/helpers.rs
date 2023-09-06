// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::FhoEnvironment;
use ffx_command::{user_error, Error, FfxContext, Result};
use fidl::endpoints::{DiscoverableProtocolMarker, Proxy, ServerEnd};
use fidl_fuchsia_developer_ffx::DaemonError;
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
    proxy: P,
    server_end: ServerEnd<P::Protocol>,
    rcs: &RemoteControlProxy,
    capability_set: rcs::OpenDirType,
    moniker: &str,
    timeout: Duration,
) -> Result<P>
where
    P: Proxy + 'static,
    P::Protocol: DiscoverableProtocolMarker,
{
    let protocol_name = P::Protocol::PROTOCOL_NAME; // for error messages.
    rcs::open_with_timeout::<P::Protocol>(
        timeout,
        moniker,
        capability_set,
        rcs,
        server_end.into_channel(),
    )
    .await
    .with_user_message(|| {
        format!("Failed to connect to protocol '{protocol_name}' at moniker '{moniker}' within {} seconds", timeout.as_secs_f64())
    })?;
    Ok(proxy)
}

pub fn map_daemon_error(svc_name: &str, err: DaemonError) -> Error {
    match err {
        DaemonError::ProtocolNotFound => user_error!(
            "The daemon protocol '{svc_name}' did not match any protocols on the daemon
If you are not developing this plugin or the protocol it connects to, then this is a bug

Please report it at http://fxbug.dev/new/ffx+User+Bug."
        ),
        DaemonError::ProtocolOpenError => user_error!(
            "The daemon protocol '{svc_name}' failed to open on the daemon.

If you are developing the protocol, there may be an internal failure when invoking the start
function. See the ffx.daemon.log for details at `ffx config get log.dir -p sub`.

If you are NOT developing this plugin or the protocol it connects to, then this is a bug.

Please report it at http://fxbug.dev/new/ffx+User+Bug."
        ),
        unexpected => user_error!(
"While attempting to open the daemon protocol '{svc_name}', received an unexpected error:

{unexpected:?}

This is not intended behavior and is a bug.
Please report it at http://fxbug.dev/new/ffx+User+Bug."

        ),
    }
    .into()
}

pub async fn load_daemon_protocol<P>(env: &FhoEnvironment) -> Result<P>
where
    P: Proxy + Clone + 'static,
    P::Protocol: DiscoverableProtocolMarker,
{
    let svc_name = <P::Protocol as DiscoverableProtocolMarker>::PROTOCOL_NAME;
    let daemon = env.injector.daemon_factory().await?;
    let (proxy, server_end) = create_proxy()?;

    daemon
        .connect_to_protocol(svc_name, server_end.into_channel())
        .await
        .bug_context("Connecting to protocol")?
        .map_err(|err| map_daemon_error(svc_name, err))?;

    Ok(proxy)
}
