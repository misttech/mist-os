// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
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
        let (proxy, server_end) = create_proxy()?;
        open_moniker(
            proxy,
            server_end,
            &rcs_instance,
            OpenDirType::ExposedDir,
            &self.moniker,
            self.timeout,
        )
        .await
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
///     #[with(fho::moniker("core/foo/thing"))]
///     foo_proxy: FooProxy,
/// }
/// ```
pub fn moniker<P: Proxy>(moniker: impl AsRef<str>) -> WithMoniker<P> {
    WithMoniker {
        moniker: moniker.as_ref().to_owned(),
        timeout: DEFAULT_PROXY_TIMEOUT,
        _p: Default::default(),
    }
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
impl TryFromEnv for ffx_fidl::FastbootProxy {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        env.injector.fastboot_factory().await.user_message("Failed to create fastboot proxy")
    }
}

#[async_trait(?Send)]
impl TryFromEnv for fidl_fuchsia_developer_remotecontrol::RemoteControlProxy {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        env.injector.remote_factory().await.user_message("Failed to create remote control proxy")
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
impl TryFromEnv for EnvironmentContext {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        Ok(env.context.clone())
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
