// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use errors::{ffx_bail, ffx_error};
use ffx_command::{Error, FfxContext, Result};
use ffx_fidl::DaemonError;
use fidl::endpoints::Proxy;
use fidl_fuchsia_developer_ffx as ffx_fidl;
use selectors::{self, VerboseError};
use std::time::Duration;
use std::{rc::Rc, sync::Arc};

use crate::FhoEnvironment;

#[async_trait(?Send)]
pub trait TryFromEnv: Sized {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self>;
}

#[async_trait(?Send)]
pub trait CheckEnv {
    async fn check_env(self, env: &FhoEnvironment) -> Result<()>;
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

/// Checks if the experimental config flag is set. This gates the execution of the command.
/// If the flag is set to `true`, this returns `Ok(())`, else returns an error.
pub struct AvailabilityFlag<T>(pub T);

#[async_trait(?Send)]
impl<T: AsRef<str>> CheckEnv for AvailabilityFlag<T> {
    async fn check_env(self, _env: &FhoEnvironment) -> Result<()> {
        let flag = self.0.as_ref();
        if ffx_config::get(flag).await.unwrap_or(false) {
            Ok(())
        } else {
            ffx_bail!(
                "This is an experimental subcommand.  To enable this subcommand run 'ffx config set {} true'",
                flag
            );
        }
    }
}

/// A trait for looking up a Fuchsia component when using the Protocol struct.
///
/// Example usage;
/// ```rust
/// struct FooSelector;
/// impl FuchsiaComponentSelector for FooSelector {
///     const SELECTOR: &'static str = "core/selector/thing";
/// }
///
/// #[derive(FfxTool)]
/// struct Tool {
///     foo_proxy: Protocol<FooProxy, FooSelector>,
/// }
/// ```
pub trait FuchsiaComponentSelector {
    const SELECTOR: &'static str;
}

/// A wrapper type used to look up protocols on a Fuchsia target. Whatever has been set as the
/// default target in the environment will be where the proxy is connected.
#[derive(Debug, Clone)]
pub struct Protocol<P: Clone, S, const TIMEOUT: u64 = 15> {
    proxy: P,
    _s: std::marker::PhantomData<fn(S) -> ()>,
}

impl<P: Clone, S, const TIMEOUT: u64> Protocol<P, S, TIMEOUT> {
    pub fn new(proxy: P) -> Self {
        Self { proxy, _s: Default::default() }
    }
}

impl<P: Clone, S, const TIMEOUT: u64> std::ops::Deref for Protocol<P, S, TIMEOUT> {
    type Target = P;

    fn deref(&self) -> &Self::Target {
        &self.proxy
    }
}

#[async_trait(?Send)]
impl<P: Proxy + Clone, S: FuchsiaComponentSelector, const TIMEOUT: u64> TryFromEnv
    for Protocol<P, S, TIMEOUT>
where
    P::Protocol: fidl::endpoints::DiscoverableProtocolMarker,
{
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        let (proxy, server_end) = fidl::endpoints::create_proxy::<P::Protocol>()
            .with_user_message(|| format!("Failed creating proxy for selector {}", S::SELECTOR))?;
        let _ = selectors::parse_selector::<VerboseError>(S::SELECTOR)
            .with_bug_context(|| format!("Parsing selector {}", S::SELECTOR))?;
        let retry_count = 1;
        let mut tries = 0;
        // TODO(fxbug.dev/113143): Remove explicit retries/timeouts here so they can be
        // configurable instead.
        let rcs_instance = loop {
            tries += 1;
            let res = env.injector.remote_factory().await;
            if res.is_ok() || tries > retry_count {
                break res;
            }
        }?;
        rcs::connect_with_timeout(
            Duration::from_secs(TIMEOUT),
            S::SELECTOR,
            &rcs_instance,
            server_end.into_channel(),
        )
        .await?;
        Ok(Protocol::new(proxy))
    }
}

#[derive(Debug, Clone)]
pub struct DaemonProtocol<P: Clone> {
    proxy: P,
}

impl<P: Clone> DaemonProtocol<P> {
    pub fn new(proxy: P) -> Self {
        Self { proxy }
    }
}

impl<P: Clone> DaemonProtocol<P> {
    pub fn into_inner(self) -> P {
        self.proxy
    }
}

impl<P: Clone> std::ops::Deref for DaemonProtocol<P> {
    type Target = P;

    fn deref(&self) -> &Self::Target {
        &self.proxy
    }
}

fn map_daemon_error(svc_name: &str, err: DaemonError) -> Error {
    match err {
        DaemonError::ProtocolNotFound => ffx_error!(
            "The daemon protocol '{svc_name}' did not match any protocols on the daemon
If you are not developing this plugin or the protocol it connects to, then this is a bug

Please report it at http://fxbug.dev/new/ffx+User+Bug."
        ),
        DaemonError::ProtocolOpenError => ffx_error!(
            "The daemon protocol '{svc_name}' failed to open on the daemon.

If you are developing the protocol, there may be an internal failure when invoking the start
function. See the ffx.daemon.log for details at `ffx config get log.dir -p sub`.

If you are NOT developing this plugin or the protocol it connects to, then this is a bug.

Please report it at http://fxbug.dev/new/ffx+User+Bug."
        ),
        unexpected => ffx_error!(
"While attempting to open the daemon protocol '{svc_name}', received an unexpected error:

{unexpected:?}

This is not intended behavior and is a bug.
Please report it at http://fxbug.dev/new/ffx+User+Bug."

        ),
    }
    .into()
}

#[async_trait(?Send)]
impl<P: Proxy + Clone> TryFromEnv for DaemonProtocol<P>
where
    P::Protocol: fidl::endpoints::DiscoverableProtocolMarker,
{
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        let svc_name = <P::Protocol as fidl::endpoints::DiscoverableProtocolMarker>::PROTOCOL_NAME;
        let (proxy, server_end) = fidl::endpoints::create_proxy::<P::Protocol>()
            .with_user_message(|| format!("Failed creating proxy for service {}", svc_name))?;
        let daemon = env.injector.daemon_factory().await?;

        daemon
            .connect_to_protocol(svc_name, server_end.into_channel())
            .await
            .bug_context("Connecting to protocol")?
            .map_err(|err| map_daemon_error(svc_name, err))
            .map(|_| DaemonProtocol { proxy })
    }
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
impl TryFromEnv for ffx_writer::Writer {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        env.injector.writer().await.user_message("Failed to create writer")
    }
}
