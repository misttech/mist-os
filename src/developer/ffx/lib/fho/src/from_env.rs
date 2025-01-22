// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::connector::{DirectConnector, NetworkConnector};
use crate::fho_env::{DeviceLookup, FhoConnectionBehavior};
use crate::{FhoEnvironment, TryFromEnv, TryFromEnvWith};
use async_trait::async_trait;
use errors::FfxError;
use fdomain_client::fidl::{
    DiscoverableProtocolMarker as FDiscoverableProtocolMarker, FDomainResourceDialect,
    Proxy as FProxy,
};
use ffx_build_version::VersionInfo;
use ffx_command_error::{return_bug, return_user_error, FfxContext, Result};
use ffx_config::EnvironmentContext;
use ffx_daemon_proxy::{DaemonVersionCheck, Injection};
use ffx_target::ssh_connector::SshConnector;
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

#[async_trait(?Send)]
impl<T> TryFromEnv for Result<T>
where
    T: TryFromEnv,
{
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        Ok(T::try_from_env(env).await)
    }
}

#[async_trait(?Send)]
impl TryFromEnv for VersionInfo {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        Ok(env.environment_context().build_info())
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

/// A connector lets a tool make multiple attempts to connect to an object. It
/// retains the environment in the tool body to allow this.
#[derive(Clone)]
pub struct Connector<T: TryFromEnv> {
    env: FhoEnvironment,
    _connects_to: std::marker::PhantomData<T>,
}

async fn knock_rcs(
    target: &Option<String>,
    tc_proxy: &ffx_fidl::TargetCollectionProxy,
    open_target_timeout: Duration,
    knock_target_timeout: Duration,
) -> Result<()> {
    loop {
        match ffx_target::knock_target_by_name(
            target,
            tc_proxy,
            open_target_timeout,
            knock_target_timeout,
        )
        .await
        {
            Ok(()) => break,
            Err(ffx_target::KnockError::CriticalError(e)) => return Err(e.into()),
            Err(ffx_target::KnockError::NonCriticalError(_)) => {
                // Should we log the error? It'll spam like hell.
            }
        };
    }
    Ok(())
}

async fn init_daemon_behavior(context: &EnvironmentContext) -> Result<FhoConnectionBehavior> {
    let build_info = context.build_info();
    let overnet_injector = Injection::initialize_overnet(
        context.clone(),
        None,
        DaemonVersionCheck::SameVersionInfo(build_info),
    )
    .await?;

    Ok(FhoConnectionBehavior::DaemonConnector(Arc::new(overnet_injector)))
}

async fn daemon_try_connect<T: TryFromEnv>(
    env: &FhoEnvironment,
    log_target_wait: &mut impl FnMut(&Option<String>, &Option<crate::Error>) -> Result<()>,
    open_target_timeout: Duration,
    knock_target_timeout: Duration,
) -> Result<T> {
    loop {
        return match T::try_from_env(env).await {
            Err(ffx_command_error::Error::User(e)) => {
                match e.downcast::<target_errors::FfxTargetError>() {
                    Ok(target_errors::FfxTargetError::DaemonError {
                        err: ffx_fidl::DaemonError::Timeout,
                        target,
                        ..
                    }) => {
                        let Ok(daemon_proxy) = ffx_fidl::DaemonProxy::try_from_env(env).await
                        else {
                            // Let the initial try_from_env detect this error.
                            continue;
                        };
                        let (tc_proxy, server_end) =
                            fidl::endpoints::create_proxy::<ffx_fidl::TargetCollectionMarker>();
                        let Ok(Ok(())) = daemon_proxy
                            .connect_to_protocol(
                                ffx_fidl::TargetCollectionMarker::PROTOCOL_NAME,
                                server_end.into_channel(),
                            )
                            .await
                        else {
                            // Let the rcs_proxy_connector detect this error too.
                            continue;
                        };
                        log_target_wait(&target, &None)?;
                        // The daemon version of this check uses a "knock" against RCS, which is
                        // essentially: keep a channel open to RCS for about a second, and if no
                        // error events come in on the channel during that time, we consider it
                        // "safe." This isn't something strictly necessary (and is not being used
                        // in the daemonless version). This was implemented when reliability with
                        // overnet was pretty spotty (when it was primarily a mesh network), and
                        // was a means to determine if a connection was "real" or if it was
                        // something stale.
                        //
                        // For non-daemon connections this isn't necessary, and we
                        // can operate under the assumption that if we have connected to an
                        // instance of an RCS proxy, we are therefore able to use it.t
                        knock_rcs(&target, &tc_proxy, open_target_timeout, knock_target_timeout)
                            .await?;
                        continue;
                    }
                    Ok(other) => return Err(Into::<FfxError>::into(other).into()),
                    Err(e) => return Err(e.into()),
                }
            }
            other => other,
        };
    }
}

async fn direct_connector_try_connect<T: TryFromEnv>(
    env: &FhoEnvironment,
    dc: &Arc<dyn DirectConnector>,
    log_target_wait: &mut impl FnMut(&Option<String>, &Option<crate::Error>) -> Result<()>,
) -> Result<T> {
    loop {
        match dc.connect().await {
            Ok(()) => {}
            Err(err) => {
                let e = err.downcast_non_fatal()?;
                tracing::debug!("error when attempting to connect with connector: {e}");
                log_target_wait(&dc.target_spec(), &Some(crate::Error::User(e)))?;
                // This is just a small wait to prevent busy-looping. The delay is arbitrary.
                fuchsia_async::Timer::new(Duration::from_millis(50)).await;
                continue;
            }
        }
        return match T::try_from_env(env).await {
            Err(conn_error) => {
                let e = conn_error.downcast_non_fatal()?;
                tracing::debug!("error when trying to connect using TryFromEnv: {e}");
                log_target_wait(&dc.target_spec(), &Some(crate::Error::User(e)))?;
                if let Err(e) = dc.rcs_proxy().await {
                    tracing::debug!("unable to get RCS proxy after TryFromEnv failure: {e}");
                } else {
                    // This state is really only possible if:
                    //
                    // a.) There is a bug. This just shouldn't happen with regular usage of FHO.
                    // b.) A user has created a `impl TryFromEnv` structure that returns a
                    //     non-fatal error implying a "retry" must happen.
                    //
                    // Hence log a warning, as both cases are odd behavior.
                    tracing::warn!(
                        "despite TryFromEnv failure, able to get RCS proxy from device connection"
                    );
                }
                continue;
            }
            Ok(res) => Ok(res),
        };
    }
}

impl<T: TryFromEnv> Connector<T> {
    const OPEN_TARGET_TIMEOUT: Duration = Duration::from_millis(500);
    const KNOCK_TARGET_TIMEOUT: Duration = ffx_target::DEFAULT_RCS_KNOCK_TIMEOUT;

    /// Try to get a `T` from the environment. Will wait for the target to
    /// appear if it is non-responsive. If that occurs, `log_target_wait` will
    /// be called prior to waiting.
    pub async fn try_connect(
        &self,
        mut log_target_wait: impl FnMut(&Option<String>, &Option<crate::Error>) -> Result<()>,
    ) -> Result<T> {
        if let Some(behavior) = self.env.behavior().await {
            match behavior {
                FhoConnectionBehavior::DaemonConnector(_) => {
                    daemon_try_connect(
                        &self.env,
                        &mut log_target_wait,
                        Self::OPEN_TARGET_TIMEOUT,
                        Self::KNOCK_TARGET_TIMEOUT,
                    )
                    .await
                }
                FhoConnectionBehavior::DirectConnector(ref dc) => {
                    direct_connector_try_connect::<T>(&self.env, dc, &mut log_target_wait).await
                }
            }
        } else {
            return_bug!("Behavior must be initialized at this point")
        }
    }
}

#[async_trait(?Send)]
impl<T: TryFromEnv> TryFromEnv for Connector<T> {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        if env.behavior().await.is_none() {
            let b = init_daemon_behavior(env.environment_context()).await?;
            env.set_behavior(b.clone()).await;
        }
        if env.lookup().await.is_none() {
            env.set_lookup(Box::new(DeviceLookupDefaultImpl)).await
        }
        Ok(Connector { env: env.clone(), _connects_to: Default::default() })
    }
}

pub struct DirectTargetConnector<T: TryFromEnv> {
    pub inner: Connector<T>,
    connector: Arc<NetworkConnector<SshConnector>>,
}

#[async_trait(?Send)]
impl<T: TryFromEnv> TryFromEnv for DirectTargetConnector<T> {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        // Configure the environment to use a direct connector
        let connector: Arc<NetworkConnector<SshConnector>> = Arc::new(
            NetworkConnector::<ffx_target::ssh_connector::SshConnector>::new(
                &env.environment_context(),
            )
            .await?,
        );

        let direct_env = env.clone();
        direct_env.set_behavior(FhoConnectionBehavior::DirectConnector(connector.clone())).await;
        if direct_env.lookup().await.is_none() {
            direct_env.set_lookup(Box::new(DeviceLookupDefaultImpl)).await
        }
        Ok(DirectTargetConnector {
            connector,
            inner: Connector { env: direct_env, _connects_to: Default::default() },
        })
    }
}

/// This is prototype code for the daemonless direct-non-strict connection.
impl<T: TryFromEnv> DirectTargetConnector<T> {
    /// Try to get a `T` from the environment. Will wait for the target to
    /// appear if it is non-responsive. If that occurs, `log_target_wait` will
    /// be called prior to waiting.
    #[allow(dead_code)]
    pub async fn try_connect(
        &self,
        log_target_wait: impl FnMut(&Option<String>, &Option<crate::Error>) -> Result<()>,
    ) -> Result<T> {
        self.inner.try_connect(log_target_wait).await
    }

    #[allow(dead_code)]
    pub async fn get_address(&self) -> Option<std::net::SocketAddr> {
        self.connector.device_address().await
    }
    #[allow(dead_code)]
    pub async fn get_ssh_host_address(&self) -> Option<String> {
        self.connector.host_ssh_address().await
    }
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

/// Gets the actively configured SDK from the environment
#[async_trait(?Send)]
impl TryFromEnv for ffx_config::Sdk {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        env.environment_context().get_sdk().user_message("Could not load currently active SDK")
    }
}

/// Gets the actively configured SDK from the environment
#[async_trait(?Send)]
impl TryFromEnv for ffx_config::SdkRoot {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        env.environment_context().get_sdk_root().user_message("Could not load currently active SDK")
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
impl TryFromEnv for ffx_fidl::TargetProxy {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        if env.behavior().await.is_none() {
            let b = init_daemon_behavior(env.environment_context()).await?;
            env.set_behavior(b.clone()).await;
        }
        match env.injector::<Self>().await?.target_factory().await.map_err(|e| {
            // This error case happens when there are multiple targets in target list.
            // So let's print out the ffx error message directly (which comes from OpenTargetError::QueryAmbiguous)
            // rather than just returning "Failed to create target proxy" which is not helpful.
            if let Some(ffx_e) = &e.downcast_ref::<FfxError>() {
                let message = format!("{ffx_e}");
                Err(e).user_message(message)
            } else {
                Err(e).user_message("Failed to create target proxy")
            }
        }) {
            Ok(p) => Ok(p),
            Err(e) => e,
        }
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

#[async_trait(?Send)]
impl TryFromEnv for EnvironmentContext {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        Ok(env.environment_context().clone())
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
    use crate::connector::MockDirectConnector;
    use crate::fho_env::{FhoConnectionBehavior, MockDeviceLookup};
    use fidl_fuchsia_developer_remotecontrol::{RemoteControlMarker, RemoteControlProxy};

    use super::*;

    #[fuchsia::test]
    async fn test_connector_try_connect_fail_reconnect_and_rcs_eventual_success() {
        let config_env = ffx_config::test_init().await.unwrap();

        let mut mock_connector = MockDirectConnector::new();
        mock_connector.expect_device_address().returning(|| Box::pin(async { None }));
        mock_connector.expect_target_spec().returning(|| None);
        let mut seq = mockall::Sequence::new();
        mock_connector.expect_connect().times(3).in_sequence(&mut seq).returning(|| {
            Box::pin(async {
                Err(crate::Error::User(
                    crate::NonFatalError(anyhow::anyhow!("we just need to try again")).into(),
                ))
            })
        });
        mock_connector
            .expect_connect()
            .times(1)
            .in_sequence(&mut seq)
            .returning(|| Box::pin(async { Ok(()) }));
        mock_connector.expect_rcs_proxy().times(2).in_sequence(&mut seq).returning(|| {
            Box::pin(async {
                Err(crate::Error::User(
                    crate::NonFatalError(
                        anyhow::anyhow!("we must retry connecting to RCS!").into(),
                    )
                    .into(),
                ))
            })
        });
        mock_connector
            .expect_connect()
            .times(1)
            .in_sequence(&mut seq)
            .returning(|| Box::pin(async { Ok(()) }));
        mock_connector.expect_rcs_proxy().times(1).in_sequence(&mut seq).returning(|| {
            Box::pin(async {
                // This will return an unusable proxy, but we're not going to use it so it's not
                // important.
                let (proxy, _) = fidl::endpoints::create_proxy::<RemoteControlMarker>();
                Ok(proxy)
            })
        });
        let tool_env = crate::testing::ToolEnv::new().make_environment_with_behavior(
            config_env.context.clone(),
            FhoConnectionBehavior::DirectConnector(Arc::new(mock_connector)),
        );

        let connector = Connector::<RemoteControlProxy>::try_from_env(&tool_env).await.unwrap();
        let res = connector.try_connect(|_, _| Ok(())).await;
        assert!(res.is_ok(), "Expected success: {:?}", res);
    }

    #[fuchsia::test]
    async fn test_connector_try_connect_fail_after_successful_connection() {
        let config_env = ffx_config::test_init().await.unwrap();
        let mut mock_connector = MockDirectConnector::new();
        mock_connector.expect_device_address().returning(|| Box::pin(async { None }));
        mock_connector.expect_target_spec().returning(|| None);
        let mut seq = mockall::Sequence::new();
        mock_connector
            .expect_connect()
            .times(1)
            .in_sequence(&mut seq)
            .returning(|| Box::pin(async { Ok(()) }));
        mock_connector.expect_rcs_proxy().times(1).in_sequence(&mut seq).returning(|| {
            Box::pin(async {
                Err(crate::Error::Unexpected(anyhow::anyhow!("something critical failed!").into()))
            })
        });
        let tool_env = crate::testing::ToolEnv::new().make_environment_with_behavior(
            config_env.context.clone(),
            FhoConnectionBehavior::DirectConnector(Arc::new(mock_connector)),
        );

        let connector = Connector::<RemoteControlProxy>::try_from_env(&tool_env).await.unwrap();
        let res = connector.try_connect(|_, _| Ok(())).await;
        assert!(res.is_err(), "Expected failure: {:?}", res);
    }

    #[fuchsia::test]
    async fn test_connector_try_connect_fail_after_critical_connection_error() {
        let config_env = ffx_config::test_init().await.unwrap();
        let mut mock_connector = MockDirectConnector::new();
        mock_connector.expect_connect().times(1).returning(|| {
            Box::pin(async {
                Err(crate::Error::Unexpected(anyhow::anyhow!("we're doomed!").into()))
            })
        });
        let tool_env = crate::testing::ToolEnv::new().make_environment_with_behavior(
            config_env.context.clone(),
            FhoConnectionBehavior::DirectConnector(Arc::new(mock_connector)),
        );

        let connector = Connector::<RemoteControlProxy>::try_from_env(&tool_env).await.unwrap();
        let res = connector.try_connect(|_, _| Ok(())).await;
        assert!(res.is_err(), "Expected failure: {:?}", res);
    }

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
