// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use errors::FfxError;
use ffx_command::{return_user_error, FfxCommandLine, FfxContext, Result};
use ffx_config::EnvironmentContext;
use ffx_core::Injector;
use ffx_fidl::VersionInfo;
use fidl::endpoints::{DiscoverableProtocolMarker, Proxy};
use fidl_fuchsia_developer_ffx as ffx_fidl;
use rcs::OpenDirType;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::time::Duration;
use std::{rc::Rc, sync::Arc};

mod from_toolbox;
mod helpers;

pub use from_toolbox::*;
pub(crate) use helpers::*;

const DEFAULT_PROXY_TIMEOUT: Duration = Duration::from_secs(15);

#[async_trait(?Send)]
pub trait TryFromEnv: Sized {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self>;
}

#[async_trait(?Send)]
pub trait CheckEnv {
    async fn check_env(self, env: &FhoEnvironment) -> Result<()>;
}

#[async_trait(?Send)]
pub trait TryFromEnvWith: 'static {
    type Output: 'static;
    async fn try_from_env_with(self, env: &FhoEnvironment) -> Result<Self::Output>;
}

#[derive(Clone)]
pub struct FhoEnvironment {
    pub ffx: FfxCommandLine,
    pub context: EnvironmentContext,
    pub injector: Arc<dyn Injector>,
}

/// This is so that you can use a () somewhere that generically expects something
/// to be TryFromEnv, but there's no meaningful type to put there.
#[async_trait(?Send)]
impl TryFromEnv for () {
    async fn try_from_env(_env: &FhoEnvironment) -> Result<Self> {
        Ok(())
    }
}

#[async_trait(?Send)]
impl<T> TryFromEnv for Arc<T>
where
    T: TryFromEnv,
{
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        T::try_from_env(env).await.map(Arc::new)
    }
}

#[async_trait(?Send)]
impl<T> TryFromEnv for Rc<T>
where
    T: TryFromEnv,
{
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        T::try_from_env(env).await.map(Rc::new)
    }
}

#[async_trait(?Send)]
impl<T> TryFromEnv for Box<T>
where
    T: TryFromEnv,
{
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        T::try_from_env(env).await.map(Box::new)
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
        Ok(env.context.build_info())
    }
}

/// Checks if the experimental config flag is set. This gates the execution of the command.
/// If the flag is set to `true`, this returns `Ok(())`, else returns an error.
pub struct AvailabilityFlag<T>(pub T);

#[async_trait(?Send)]
impl<T: AsRef<str>> CheckEnv for AvailabilityFlag<T> {
    async fn check_env(self, env: &FhoEnvironment) -> Result<()> {
        let flag = self.0.as_ref();
        if env.context.get(flag).await.unwrap_or(false) {
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
pub struct Connector<T: TryFromEnv> {
    env: FhoEnvironment,
    _connects_to: std::marker::PhantomData<T>,
}

impl<T: TryFromEnv> Connector<T> {
    pub const OPEN_TARGET_TIMEOUT: Duration = Duration::from_millis(500);
    pub const KNOCK_TARGET_TIMEOUT: Duration = ffx_target::DEFAULT_RCS_KNOCK_TIMEOUT;

    /// Try to get a `T` from the environment. Will wait for the target to
    /// appear if it is non-responsive. If that occurs, `log_target_wait` will
    /// be called prior to waiting.
    pub async fn try_connect(
        &self,
        mut log_target_wait: impl FnMut(&Option<String>) -> Result<()>,
    ) -> Result<T> {
        loop {
            return match T::try_from_env(&self.env).await {
                Err(ffx_command::Error::User(e)) => {
                    match e.downcast::<errors::FfxError>() {
                        Ok(errors::FfxError::DaemonError {
                            err: ffx_fidl::DaemonError::Timeout,
                            target,
                            ..
                        }) => {
                            let Ok(daemon_proxy) = self.env.injector.daemon_factory().await else {
                                // Let the initial try_from_env detect this error.
                                continue;
                            };
                            let (tc_proxy, server_end) =
                                fidl::endpoints::create_proxy::<ffx_fidl::TargetCollectionMarker>()
                                    .expect("Could not create FIDL proxy");
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
                            log_target_wait(&target)?;
                            loop {
                                match ffx_target::knock_target_by_name(
                                    &target,
                                    &tc_proxy,
                                    Self::OPEN_TARGET_TIMEOUT,
                                    Self::KNOCK_TARGET_TIMEOUT,
                                )
                                .await
                                {
                                    Ok(()) => break,
                                    Err(ffx_target::KnockError::CriticalError(e)) => {
                                        return Err(e.into())
                                    }
                                    Err(ffx_target::KnockError::NonCriticalError(_)) => {
                                        // Should we log the error? It'll spam like hell.
                                    }
                                };
                            }
                            continue;
                        }
                        Ok(other) => return Err(other.into()),
                        Err(e) => return Err(e.into()),
                    }
                }
                other => other,
            };
        }
    }
}

#[async_trait(?Send)]
impl<T: TryFromEnv> TryFromEnv for Connector<T> {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        Ok(Connector { env: env.clone(), _connects_to: Default::default() })
    }
}

/// Allows you to defer the initialization of an object in your tool struct
/// until you need it (if at all) or apply additional combinators on it (like
/// custom timeout logic or anything like that).
///
/// If you need to defer something that requires a decorator, use the
/// [`deferred`] decorator around it.
///
/// Example:
/// ```rust
/// #[derive(FfxTool)]
/// struct Tool {
///     daemon: fho::Deferred<fho::DaemonProxy>,
/// }
/// impl fho::FfxMain for Tool {
///     type Writer = fho::SimpleWriter;
///     async fn main(self, _writer: fho::SimpleWriter) -> fho::Result<()> {
///         let daemon = self.daemon.await?;
///         writeln!(writer, "Loaded the daemon proxy!");
///     }
/// }
/// ```
pub struct Deferred<T: 'static>(Pin<Box<dyn Future<Output = Result<T>>>>);
#[async_trait(?Send)]
impl<T> TryFromEnv for Deferred<T>
where
    T: TryFromEnv,
{
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        let env = env.clone();
        Ok(Self(Box::pin(async move { T::try_from_env(&env).await })))
    }
}

impl<T: 'static> Deferred<T> {
    /// Use the value provided to create a test-able Deferred value.
    pub fn from_output(output: Result<T>) -> Self {
        Self(Box::pin(async move { output }))
    }
}

/// The implementation of the decorator returned by [`deferred`]
pub struct WithDeferred<T>(T);
#[async_trait(?Send)]
impl<T> TryFromEnvWith for WithDeferred<T>
where
    T: TryFromEnvWith + 'static,
{
    type Output = Deferred<T::Output>;
    async fn try_from_env_with(self, env: &FhoEnvironment) -> Result<Self::Output> {
        let env = env.clone();
        Ok(Deferred(Box::pin(async move { self.0.try_from_env_with(&env).await })))
    }
}

/// A decorator for proxy types in [`crate::FfxTool`] implementations so you can
/// specify the moniker for the component exposing the proxy you're loading.
///
/// Example:
///
/// ```rust
/// #[derive(FfxTool)]
/// struct Tool {
///     #[with(fho::deferred(fho::moniker("/core/foo/thing")))]
///     foo_proxy: fho::Deferred<FooProxy>,
/// }
/// ```
pub fn deferred<T: TryFromEnvWith>(inner: T) -> WithDeferred<T> {
    WithDeferred(inner)
}

impl<T> Future for Deferred<T> {
    type Output = Result<T>;
    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        self.0.as_mut().poll(cx)
    }
}

/// Gets the actively configured SDK from the environment
#[async_trait(?Send)]
impl TryFromEnv for ffx_config::Sdk {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        env.context.get_sdk().await.user_message("Could not load currently active SDK")
    }
}

/// Gets the actively configured SDK from the environment
#[async_trait(?Send)]
impl TryFromEnv for ffx_config::SdkRoot {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        env.context.get_sdk_root().await.user_message("Could not load currently active SDK")
    }
}

/// The implementation of the decorator returned by [`moniker`] and [`moniker_timeout`]
pub struct WithMoniker<P> {
    moniker: String,
    timeout: Duration,
    _p: PhantomData<fn() -> P>,
}

#[async_trait(?Send)]
impl<P> TryFromEnvWith for WithMoniker<P>
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
pub fn moniker<P: Proxy>(moniker: impl AsRef<str>) -> WithToolbox<P> {
    toolbox_or(moniker)
}

/// Like [`moniker`], but lets you also specify an override for the default
/// timeout.
pub fn moniker_timeout<P: Proxy>(moniker: impl AsRef<str>, timeout_secs: u64) -> WithMoniker<P> {
    WithMoniker {
        moniker: moniker.as_ref().to_owned(),
        timeout: Duration::from_secs(timeout_secs),
        _p: Default::default(),
    }
}

#[derive(Debug, Clone)]
pub struct DaemonProtocol<P: Clone>(P);

#[derive(Debug, Clone, Default)]
pub struct WithDaemonProtocol<P>(PhantomData<fn() -> P>);

impl<P: Clone> DaemonProtocol<P> {
    pub fn new(proxy: P) -> Self {
        Self(proxy)
    }
}

impl<P: Clone> DaemonProtocol<P> {
    pub fn into_inner(self) -> P {
        self.0
    }
}

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
        env.injector.daemon_factory().await.user_message("Failed to create daemon proxy")
    }
}

#[async_trait(?Send)]
impl TryFromEnv for Option<ffx_fidl::DaemonProxy> {
    /// Attempts to connect to the ffx daemon, returning Ok(None) if no instance of the daemon is
    /// started. If you would like to use the normal flow of attempting to connect to the daemon,
    /// and starting a new instance of the daemon if none is currently present, you should use the
    /// impl for `ffx_fidl::DaemonProxy`, which returns a `Result<ffx_fidl::DaemonProxy>`.
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        let res = env
            .injector
            .try_daemon()
            .await
            .user_message("Failed internally while checking for daemon.")?;
        Ok(res)
    }
}

#[async_trait(?Send)]
impl TryFromEnv for ffx_fidl::TargetProxy {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        env.injector.target_factory().await.user_message("Failed to create target proxy")
    }
}

#[async_trait(?Send)]
impl TryFromEnv for fidl_fuchsia_developer_remotecontrol::RemoteControlProxy {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        match env.injector.remote_factory().await {
            Ok(p) => Ok(p),
            Err(e) => {
                if let Some(ffx_e) = &e.downcast_ref::<FfxError>() {
                    let message = format!("{ffx_e} when creating remotecontrol proxy");
                    Err(e).user_message(message)
                } else {
                    Err(e).user_message("Failed to create remote control proxy")
                }
            }
        }
    }
}

#[async_trait(?Send)]
impl TryFromEnv for ffx_writer::Writer {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        env.injector.writer().await.user_message("Failed to create writer")
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
        Ok(ffx_writer::MachineWriter::new(env.ffx.global.machine))
    }
}

#[async_trait(?Send)]
impl<T: serde::Serialize + schemars::JsonSchema> TryFromEnv
    for ffx_writer::VerifiedMachineWriter<T>
{
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        Ok(ffx_writer::VerifiedMachineWriter::new(env.ffx.global.machine))
    }
}

#[async_trait(?Send)]
impl TryFromEnv for EnvironmentContext {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        Ok(env.context.clone())
    }
}

#[async_trait(?Send)]
impl<T> TryFromEnv for PhantomData<T> {
    async fn try_from_env(_env: &FhoEnvironment) -> Result<Self> {
        Ok(PhantomData)
    }
}
#[cfg(test)]
mod tests {
    use ffx_command::Error;

    use super::*;

    #[derive(Debug)]
    struct AlwaysError;
    #[async_trait(?Send)]
    impl TryFromEnv for AlwaysError {
        async fn try_from_env(_env: &FhoEnvironment) -> Result<Self> {
            Err(Error::User(anyhow::anyhow!("boom")))
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_deferred_err() {
        let config_env = ffx_config::test_init().await.unwrap();
        let tool_env = crate::testing::ToolEnv::new().make_environment(config_env.context.clone());

        Deferred::<AlwaysError>::try_from_env(&tool_env)
            .await
            .expect("Deferred result should be Ok")
            .await
            .expect_err("Inner AlwaysError should error after second await");
    }
}
