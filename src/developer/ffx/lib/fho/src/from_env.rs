// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::connector::DirectConnector;
use crate::fho_env::{DeviceLookup, FhoConnectionBehavior};
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
use ffx_target::TargetInfoQuery;
use fidl::encoding::DefaultFuchsiaResourceDialect;
use fidl::endpoints::{DiscoverableProtocolMarker, Proxy};
use fidl_fuchsia_developer_ffx as ffx_fidl;
use futures::future::LocalBoxFuture;
use rcs::OpenDirType;
use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

mod from_toolbox;
mod helpers;

pub use from_toolbox::*;
pub(crate) use helpers::*;

const DEFAULT_PROXY_TIMEOUT: Duration = Duration::from_secs(15);

#[async_trait(?Send)]
pub trait CheckEnv {
    async fn check_env(self, env: &FhoEnvironment) -> Result<()>;
}

/// The default implementation of device lookup and resolution. Primarily used for simpler testing.
#[doc(hidden)]
#[derive(Clone)]
pub struct DeviceLookupDefaultImpl;

impl DeviceLookup for DeviceLookupDefaultImpl {
    fn target_spec(&self, env: EnvironmentContext) -> LocalBoxFuture<'_, Result<Option<String>>> {
        Box::pin(async move {
            ffx_target::get_target_specifier(&env).await.bug_context("looking up target specifier")
        })
    }

    fn resolve_target_query_to_info(
        &self,
        query: TargetInfoQuery,
        ctx: EnvironmentContext,
    ) -> LocalBoxFuture<'_, Result<Vec<ffx_fidl::TargetInfo>>> {
        Box::pin(async move {
            ffx_target::resolve_target_query_to_info(query, &ctx)
                .await
                .bug_context("resolving target")
        })
    }
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

#[async_trait(?Send)]
impl TryFromEnv for ffx_fidl::TargetInfo {
    /// Retrieve `TargetInfo` for a target matching a specifier. Fails if more than one target
    /// matches.
    ///
    /// Note that if no target is specified in configuration or on the command-line that this will
    /// end up attempting to connect to all discoverable targets which may be problematic in test or
    /// lab environments.
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        let targets = Vec::<Self>::try_from_env(env).await?;
        if targets.len() > 1 {
            return_user_error!("Found more than one target: {targets:#?}.");
        } else {
            Ok(targets[0].clone())
        }
    }
}

#[async_trait(?Send)]
impl TryFromEnv for Vec<ffx_fidl::TargetInfo> {
    /// Retrieve `TargetInfo` for any targets matching a specifier.
    ///
    /// Note that if no target is specified in configuration or on the command-line that this will
    /// end up attempting to connect to all discoverable targets which may be problematic in test or
    /// lab environments.
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        let looker = env.lookup().await;

        let lookup: &Box<dyn DeviceLookup> = if let Some(ref lookup) = *looker {
            lookup
        } else {
            let l = Box::new(DeviceLookupDefaultImpl);
            env.set_lookup(l.clone()).await;
            return Self::try_from_env(env).await;
        };
        let target = lookup.target_spec(env.environment_context().clone()).await?;
        let targets = lookup
            .resolve_target_query_to_info(
                TargetInfoQuery::from(target.clone()),
                env.environment_context().clone(),
            )
            .await?;
        if targets.is_empty() {
            match target.as_ref() {
                Some(t) => {
                    return_user_error!("Could not discover any targets for specifier '{}'.", t)
                }
                None => return_user_error!("Could not discover any targets."),
            }
        }
        Ok(targets)
    }
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

/// A decorator for proxy types in [`crate::FfxTool`] implementations so you can
/// specify the moniker for the component exposing the proxy you're loading.
///
/// This is actually an alias to [`toolbox_or`], so it will also try
/// your tool's default toolbox first.
///
/// Example:
///
/// ```rust
/// #[derive(FfxTool)]
/// struct Tool {
///     #[with(fho::moniker("core/foo/thing"))]
///     foo_proxy: FooProxy,
/// }
/// ```
pub fn moniker<P: Proxy>(
    moniker: impl AsRef<str>,
) -> WithToolbox<P, DefaultFuchsiaResourceDialect> {
    toolbox_or(moniker)
}

/// Same as [`moniker`] but for FDomain
pub fn moniker_f<P: FProxy>(moniker: impl AsRef<str>) -> WithToolbox<P, FDomainResourceDialect> {
    toolbox_or_f(moniker)
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

#[async_trait(?Send)]
impl TryFromEnv for ffx_fidl::DaemonProxy {
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
            .map_err(|e| crate::user_error!("{}", e))
    }
}

#[async_trait(?Send)]
impl TryFromEnv for Option<ffx_fidl::DaemonProxy> {
    /// Attempts to connect to the ffx daemon, returning Ok(None) if no instance of the daemon is
    /// started. If you would like to use the normal flow of attempting to connect to the daemon,
    /// and starting a new instance of the daemon if none is currently present, you should use the
    /// impl for `ffx_fidl::DaemonProxy`, which returns a `Result<ffx_fidl::DaemonProxy>`.
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        if env.behavior().await.is_none() {
            let b = init_daemon_behavior(env.environment_context()).await?;
            env.set_behavior(b.clone()).await;
        }
        let res = env
            .injector::<Self>()
            .await?
            .try_daemon()
            .await
            .user_message("Failed internally while checking for daemon.")?;
        Ok(res)
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

#[cfg(test)]
mod tests {
    use crate::fho_env::MockDeviceLookup;

    use super::*;

    #[fuchsia::test]
    async fn test_target_info_try_from_env_none_is_okay() {
        let config_env = ffx_config::test_init().await.unwrap();
        let mut mock = MockDeviceLookup::new();
        mock.expect_target_spec().times(1).returning(|_e| Box::pin(async { Ok(None) }));
        mock.expect_resolve_target_query_to_info()
            .times(1)
            .returning(|_q, _e| Box::pin(async { Ok(vec![ffx_fidl::TargetInfo::default()]) }));
        let tool_env = crate::testing::ToolEnv::new()
            .make_environment_with_lookup(config_env.context.clone(), mock);
        let info = ffx_fidl::TargetInfo::try_from_env(&tool_env).await.unwrap();
        assert_eq!(info, ffx_fidl::TargetInfo::default());
    }

    #[fuchsia::test]
    async fn test_target_info_try_from_env_no_targets_is_error() {
        let config_env = ffx_config::test_init().await.unwrap();
        let mut mock = MockDeviceLookup::new();
        mock.expect_target_spec().times(1).returning(|_e| Box::pin(async { Ok(None) }));
        mock.expect_resolve_target_query_to_info()
            .times(1)
            .returning(|_q, _e| Box::pin(async { Ok(vec![]) }));
        let tool_env = crate::testing::ToolEnv::new()
            .make_environment_with_lookup(config_env.context.clone(), mock);

        let result = ffx_fidl::TargetInfo::try_from_env(&tool_env).await;
        assert!(result.is_err());
    }

    #[fuchsia::test]
    async fn test_target_info_try_from_env_specifier_with_no_targets_is_error() {
        let config_env = ffx_config::test_init().await.unwrap();
        let mut mock = MockDeviceLookup::new();
        mock.expect_target_spec()
            .times(1)
            .returning(|_e| Box::pin(async { Ok(Some("frobinator".to_string())) }));
        mock.expect_resolve_target_query_to_info()
            .times(1)
            .returning(|_q, _e| Box::pin(async { Ok(vec![]) }));
        let tool_env = crate::testing::ToolEnv::new()
            .make_environment_with_lookup(config_env.context.clone(), mock);
        let result = ffx_fidl::TargetInfo::try_from_env(&tool_env).await;
        assert!(result.is_err());
    }

    #[fuchsia::test]
    async fn test_target_info_try_from_env_too_many_targets_is_error() {
        let config_env = ffx_config::test_init().await.unwrap();
        let mut mock = MockDeviceLookup::new();
        mock.expect_target_spec()
            .times(1)
            .returning(|_e| Box::pin(async { Ok(Some("frobinator".to_string())) }));
        mock.expect_resolve_target_query_to_info().times(1).returning(|_q, _e| {
            Box::pin(async {
                Ok(vec![ffx_fidl::TargetInfo::default(), ffx_fidl::TargetInfo::default()])
            })
        });
        let tool_env = crate::testing::ToolEnv::new()
            .make_environment_with_lookup(config_env.context.clone(), mock);
        let result = ffx_fidl::TargetInfo::try_from_env(&tool_env).await;
        assert!(result.is_err());
    }
}
