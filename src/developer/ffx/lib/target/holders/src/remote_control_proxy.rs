// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::ops::Deref;
use std::time::Duration;

use crate::init_connection_behavior;
use async_trait::async_trait;
use errors::FfxError;
use ffx_command_error::{bug, FfxContext as _, Result};
use fho::{FhoConnectionBehavior, FhoEnvironment, TryFromEnv};
use fidl::endpoints::{DiscoverableProtocolMarker, Proxy};
use fidl_fuchsia_developer_remotecontrol::RemoteControlProxy;

#[derive(Clone, Debug)]
pub struct RemoteControlProxyHolder(RemoteControlProxy);

impl Deref for RemoteControlProxyHolder {
    type Target = RemoteControlProxy;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<RemoteControlProxy> for RemoteControlProxyHolder {
    fn from(value: RemoteControlProxy) -> Self {
        RemoteControlProxyHolder(value)
    }
}

#[async_trait(?Send)]
impl TryFromEnv for RemoteControlProxyHolder {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        let behavior = if let Some(behavior) = env.behavior().await {
            behavior
        } else {
            let b = init_connection_behavior(env.environment_context()).await?;
            env.set_behavior(b.clone()).await;
            b
        };
        match behavior {
            FhoConnectionBehavior::DaemonConnector(daemon) => match daemon.remote_factory().await {
                Ok(p) => Ok(p.into()),
                Err(e) => {
                    if let Some(ffx_e) = &e.downcast_ref::<FfxError>() {
                        let message = format!("Failed connecting to remote control proxy: {ffx_e}");
                        Err(e).user_message(message)
                    } else {
                        Err(e).user_message("Failed to create remote control proxy. Please check the connection to the target;`ffx doctor -v` may help diagnose the issue.")
                    }
                }
            },
            FhoConnectionBehavior::DirectConnector(direct) => {
                let conn = direct.connection().await?;
                let cc = conn.lock().await;
                let c = cc.deref();
                c.as_ref()
                    .ok_or(bug!("Connection not yet initialized"))?
                    .rcs_proxy()
                    .await
                    .bug()
                    .map(Into::into)
                    .map_err(Into::into)
            }
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
    let (proxy, server_end) = fidl::endpoints::create_proxy::<P::Protocol>();
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

/// Sets up a fake proxy of type `T` handing requests to the given callback and returning
/// their responses.
///
/// This is basically the same thing as `ffx_plugin` used to generate for
/// each proxy argument, but uses a generic instead of text replacement.
pub fn fake_proxy<T: fidl::endpoints::Proxy>(
    mut handle_request: impl FnMut(fidl::endpoints::Request<T::Protocol>) + 'static,
) -> T {
    use futures::TryStreamExt;
    let (proxy, mut stream) = fidl::endpoints::create_proxy_and_stream::<T::Protocol>();
    fuchsia_async::Task::local(async move {
        while let Ok(Some(req)) = stream.try_next().await {
            handle_request(req);
        }
    })
    .detach();
    proxy
}
