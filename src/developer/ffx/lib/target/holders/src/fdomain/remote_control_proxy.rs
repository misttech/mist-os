// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::init_daemon_behavior;
use async_trait::async_trait;
use errors::FfxError;
use fdomain_client::fidl::{
    DiscoverableProtocolMarker as FDiscoverableProtocolMarker, Proxy as FProxy,
};
use fdomain_fuchsia_developer_remotecontrol::RemoteControlProxy;
use ffx_command_error::{Error, FfxContext as _, Result};
use fho::{FhoConnectionBehavior, FhoEnvironment, TryFromEnv};
use std::ops::Deref;
use std::time::Duration;

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
            let b = init_daemon_behavior(env.environment_context()).await?;
            env.set_behavior(b.clone()).await;
            b
        };
        match behavior {
            FhoConnectionBehavior::DirectConnector(dc) => {
                dc.rcs_proxy_fdomain().await.map(Into::into).map_err(Into::into)
            }
            FhoConnectionBehavior::DaemonConnector(dc) => match dc.remote_factory_fdomain().await {
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
        }
    }
}

pub(crate) async fn open_moniker_fdomain<P>(
    rcs: &RemoteControlProxy,
    capability_set: rcs_fdomain::OpenDirType,
    moniker: &str,
    timeout: Duration,
) -> Result<P>
where
    P: FProxy + 'static,
    P::Protocol: FDiscoverableProtocolMarker,
{
    let (proxy, server_end) =
        rcs.client().map_err(|e| Error::Unexpected(e.into()))?.create_proxy::<P::Protocol>();
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
