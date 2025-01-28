// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::connector::DirectConnector;
use crate::fho_env::FhoConnectionBehavior;
use crate::{FhoEnvironment, TryFromEnv, TryFromEnvWith};
use async_trait::async_trait;
use errors::FfxError;
use fdomain_client::fidl::{
    DiscoverableProtocolMarker as FDiscoverableProtocolMarker, FDomainResourceDialect,
    Proxy as FProxy,
};
use ffx_command_error::{return_user_error, FfxContext, Result};
use ffx_config::EnvironmentContext;
use ffx_daemon_proxy::{DaemonVersionCheck, Injection};
use fidl::encoding::DefaultFuchsiaResourceDialect;
use fidl::endpoints::{DiscoverableProtocolMarker, Proxy};
use rcs::OpenDirType;
use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

mod helpers;

pub(crate) use helpers::*;

#[async_trait(?Send)]
pub trait CheckEnv {
    async fn check_env(self, env: &FhoEnvironment) -> Result<()>;
}

/// Checks if the experimental config flag is set. This gates the execution of the command.
/// If the flag is set to `true`, this returns `Ok(())`, else returns an error.
pub struct AvailabilityFlag<T>(pub T);

#[async_trait(?Send)]
impl<T: AsRef<str>> CheckEnv for AvailabilityFlag<T> {
    async fn check_env(self, env: &FhoEnvironment) -> Result<()> {
        let flag = self.0.as_ref();
        if env.environment_context().get(flag).unwrap_or(false) {
            Ok(())
        } else {
            return_user_error!(
                "This is an experimental subcommand.  To enable this subcommand run 'ffx config set {} true'",
                flag
            );
        }
    }
}

pub async fn init_daemon_behavior(context: &EnvironmentContext) -> Result<FhoConnectionBehavior> {
    let build_info = context.build_info();
    let overnet_injector = Injection::initialize_overnet(
        context.clone(),
        None,
        DaemonVersionCheck::SameVersionInfo(build_info),
    )
    .await?;

    Ok(FhoConnectionBehavior::DaemonConnector(Arc::new(overnet_injector)))
}

/// The implementation of the decorator returned by [`moniker`].
pub struct WithMoniker<P, D> {
    moniker: String,
    timeout: Duration,
    _p: PhantomData<(fn() -> P, D)>,
}

#[async_trait(?Send)]
impl<P> TryFromEnvWith for WithMoniker<P, DefaultFuchsiaResourceDialect>
where
    P: Proxy + 'static,
    P::Protocol: DiscoverableProtocolMarker,
{
    type Output = P;
    async fn try_from_env_with(self, env: &FhoEnvironment) -> Result<Self::Output> {
        let rcs_instance = connect_to_rcs(&env).await?;
        open_moniker(&rcs_instance, OpenDirType::ExposedDir, &self.moniker, self.timeout).await
    }
}

#[async_trait(?Send)]
impl<P> TryFromEnvWith for WithMoniker<P, FDomainResourceDialect>
where
    P: FProxy + 'static,
    P::Protocol: FDiscoverableProtocolMarker,
{
    type Output = P;
    async fn try_from_env_with(self, env: &FhoEnvironment) -> Result<Self::Output> {
        let rcs_instance = connect_to_rcs_fdomain(&env).await?;
        open_moniker_fdomain(
            &rcs_instance,
            rcs_fdomain::OpenDirType::ExposedDir,
            &self.moniker,
            self.timeout,
        )
        .await
    }
}

#[async_trait(?Send)]
impl TryFromEnv for fdomain_fuchsia_developer_remotecontrol::RemoteControlProxy {
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
                dc.rcs_proxy_fdomain().await.map_err(Into::into)
            }
            FhoConnectionBehavior::DaemonConnector(dc) => match dc.remote_factory_fdomain().await {
                Ok(p) => Ok(p),
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

#[async_trait(?Send)]
impl TryFromEnv for fidl_fuchsia_developer_remotecontrol::RemoteControlProxy {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        let behavior = if let Some(behavior) = env.behavior().await {
            behavior
        } else {
            let b = init_daemon_behavior(env.environment_context()).await?;
            env.set_behavior(b.clone()).await;
            b
        };
        match behavior {
            FhoConnectionBehavior::DaemonConnector(daemon) => match daemon.remote_factory().await {
                Ok(p) => Ok(p),
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
                direct.rcs_proxy().await.map_err(Into::into)
            }
        }
    }
}

#[async_trait(?Send)]
impl TryFromEnv for ffx_writer::SimpleWriter {
    async fn try_from_env(_env: &FhoEnvironment) -> Result<Self> {
        Ok(ffx_writer::SimpleWriter::new())
    }
}

#[async_trait(?Send)]
impl<T: serde::Serialize> TryFromEnv for ffx_writer::MachineWriter<T> {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        Ok(ffx_writer::MachineWriter::new(env.ffx_command().global.machine))
    }
}

#[async_trait(?Send)]
impl<T: serde::Serialize + schemars::JsonSchema> TryFromEnv
    for ffx_writer::VerifiedMachineWriter<T>
{
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        Ok(ffx_writer::VerifiedMachineWriter::new(env.ffx_command().global.machine))
    }
}

// Returns a DirectConnector only if we have a direct connection. Returns None for
// a daemon connection.
#[async_trait(?Send)]
impl TryFromEnv for Option<Arc<dyn DirectConnector>> {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        match env.behavior().await {
            Some(FhoConnectionBehavior::DirectConnector(ref direct)) => Ok(Some(direct.clone())),
            _ => Ok(None),
        }
    }
}
