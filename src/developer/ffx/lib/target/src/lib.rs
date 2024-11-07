// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use addr::TargetAddr;
use anyhow::{Context as _, Result};
use compat_info::CompatibilityInfo;
use errors::{ffx_bail, FfxError};
use ffx_config::keys::TARGET_DEFAULT_KEY;
use ffx_config::EnvironmentContext;
use fidl::endpoints::create_proxy;
use fidl::prelude::*;
use fidl_fuchsia_developer_ffx::{
    self as ffx, DaemonError, DaemonProxy, TargetCollectionMarker, TargetCollectionProxy,
    TargetInfo, TargetMarker, TargetQuery,
};
use fidl_fuchsia_developer_remotecontrol::{RemoteControlMarker, RemoteControlProxy};
use fidl_fuchsia_net as net;
use futures::{select, Future, FutureExt, TryStreamExt};
use std::net::IpAddr;
use std::time::Duration;
use thiserror::Error;
use timeout::timeout;
use tracing::{debug, info};

#[cfg(test)]
use mockall::predicate::*;

pub mod connection;
pub mod ssh_connector;

mod fidl_pipe;
mod resolve;
mod target_connector;

pub use connection::ConnectionError;
pub use discovery::desc::{Description, FastbootInterface};
pub use discovery::query::TargetInfoQuery;
pub use fidl_pipe::{create_overnet_socket, FidlPipe};
pub use resolve::{
    get_discovery_stream, maybe_locally_resolve_target_spec, resolve_target_address,
    resolve_target_query_to_info, resolve_target_query_with, Resolution,
};
pub use target_connector::{
    FDomainConnection, OvernetConnection, TargetConnection, TargetConnectionError, TargetConnector,
};

/// Re-export of [`fidl_fuchsia_developer_ffx::TargetProxy`] for ease of use
pub use fidl_fuchsia_developer_ffx::TargetProxy;

const FASTBOOT_INLINE_TARGET: &str = "ffx.fastboot.inline_target";

/// The default target name if no target spec is given (for debugging, reporting to the user, etc).
/// TODO(b/371222096): Use this everywhere (will require a bit of digging).
pub const UNSPECIFIED_TARGET_NAME: &str = "[unspecified]";

/// Attempt to connect to RemoteControl on a target device using a connection to a daemon.
///
/// The optional |target| is a string matcher as defined in fuchsia.developer.ffx.TargetQuery
/// fidl table.
#[tracing::instrument]
pub async fn get_remote_proxy(
    target_spec: Option<String>,
    daemon_proxy: DaemonProxy,
    proxy_timeout: Duration,
    mut target_info: Option<&mut Option<TargetInfo>>,
    context: &EnvironmentContext,
) -> Result<RemoteControlProxy> {
    let mut target_info_out = None;
    let res = loop {
        match get_remote_proxy_impl(
            &target_spec,
            &daemon_proxy,
            &proxy_timeout,
            &mut target_info_out,
            &context,
        )
        .await
        {
            Ok(p) => break Ok(p),
            Err(e) => {
                let e = e.downcast::<FfxError>()?;
                let FfxError::TargetConnectionError { err, .. } = e else {
                    break Err(e.into());
                };
                match err {
                    ffx::TargetConnectionError::KeyVerificationFailure
                    | ffx::TargetConnectionError::InvalidArgument
                    | ffx::TargetConnectionError::PermissionDenied => {
                        break (Err(anyhow::Error::new(e)))
                    }
                    _ => {
                        let retry_info =
                            format!("Retrying connection after non-fatal error encountered: {e}");
                        eprintln!("{}", retry_info.as_str());
                        tracing::info!("{}", retry_info.as_str());
                        // Insert a small delay to prevent too tight of a spinning loop.
                        fuchsia_async::Timer::new(Duration::from_millis(20)).await;
                        continue;
                    }
                }
            }
        }
    };
    if let Some(ref mut info_out) = target_info {
        **info_out = target_info_out.clone();
    }
    res
}

async fn get_remote_proxy_impl(
    target_spec: &Option<String>,
    daemon_proxy: &DaemonProxy,
    proxy_timeout: &Duration,
    target_info: &mut Option<TargetInfo>,
    context: &EnvironmentContext,
) -> Result<RemoteControlProxy> {
    // See if we need to do local resolution. (Do it here not in
    // open_target_with_fut because o_t_w_f is not async)
    let mut target_spec =
        resolve::maybe_locally_resolve_target_spec(target_spec.clone(), context).await?;
    let (target_proxy, target_proxy_fut) =
        open_target_with_fut(target_spec.clone(), daemon_proxy.clone(), *proxy_timeout, context)?;
    let mut target_proxy_fut = target_proxy_fut.boxed_local().fuse();
    let (remote_proxy, remote_server_end) = create_proxy::<RemoteControlMarker>()?;
    let mut open_remote_control_fut =
        target_proxy.open_remote_control(remote_server_end).boxed_local().fuse();
    let res = loop {
        select! {
            res = open_remote_control_fut => {
                match res {
                    Err(e) => {
                        // Getting here is most likely the result of a PEER_CLOSED error, which
                        // may be because the target_proxy closure has propagated faster than
                        // the error (which can happen occasionally). To counter this, wait for
                        // the target proxy to complete, as it will likely only need to be
                        // polled once more (open_remote_control_fut partially depends on it).
                        target_proxy_fut.await?;
                        return Err(e.into());
                    }
                    Ok(r) => break(r),
                }
            }
            res = target_proxy_fut => res?,
        }
    };
    let info = target_proxy.identity().await?;
    *target_info = Some(info.clone());
    // Only replace the target spec info if we're going from less info to more info.
    // Don't want to overwrite it otherwise.
    match (target_spec.as_ref(), info.nodename, info.ssh_address) {
        (None, Some(n), Some(s)) => target_spec.replace(format!("{n} at {}", TargetAddr::from(s))),
        (None, None, Some(s)) => target_spec.replace(TargetAddr::from(s).to_string()),
        (None, Some(n), None) => target_spec.replace(format!("{n}")),
        (_, _, _) => None,
    };
    let target_spec = target_spec.as_ref().map(ToString::to_string);
    match res {
        Ok(_) => Ok(remote_proxy),
        Err(err) => Err(anyhow::Error::new(FfxError::TargetConnectionError {
            err,
            target: target_spec,
            logs: Some(target_proxy.get_ssh_logs().await?),
        })),
    }
}

/// Attempt to connect to a target given a connection to a daemon.
///
/// The returned future must be polled to completion. It is returned separately
/// from the TargetProxy to enable immediately pushing requests onto the TargetProxy
/// before connecting to the target completes.
///
/// The optional |target| is a string matcher as defined in fuchsia.developer.ffx.TargetQuery
/// fidl table.
#[tracing::instrument]
pub fn open_target_with_fut<'a, 'b: 'a>(
    target: Option<String>,
    daemon_proxy: DaemonProxy,
    target_timeout: Duration,
    env_context: &'b EnvironmentContext,
) -> Result<(TargetProxy, impl Future<Output = Result<()>> + 'a)> {
    let (tc_proxy, tc_server_end) = create_proxy::<TargetCollectionMarker>()?;
    let (target_proxy, target_server_end) = create_proxy::<TargetMarker>()?;
    let t_clone = target.clone();
    let target_collection_fut = async move {
        daemon_proxy
            .connect_to_protocol(
                TargetCollectionMarker::PROTOCOL_NAME,
                tc_server_end.into_channel(),
            )
            .await?
            .map_err(|err| FfxError::DaemonError { err, target: t_clone })?;
        Result::<()>::Ok(())
    };
    let t_clone = target.clone();
    let target_handle_fut = async move {
        let is_fastboot_inline = env_context.get(FASTBOOT_INLINE_TARGET).unwrap_or(false);
        if is_fastboot_inline {
            if let Some(ref serial_number) = target {
                tracing::trace!("got serial number: {serial_number}");
                timeout(target_timeout, tc_proxy.add_inline_fastboot_target(&serial_number))
                    .await??;
            }
        }
        timeout(
            target_timeout,
            tc_proxy.open_target(
                &TargetQuery { string_matcher: t_clone.clone(), ..Default::default() },
                target_server_end,
            ),
        )
        .await
        .map_err(|_| FfxError::DaemonError { err: DaemonError::Timeout, target: t_clone })??
        .map_err(|err| FfxError::OpenTargetError { err, target })?;
        Result::<()>::Ok(())
    };
    let fut = async move {
        let ((), ()) = futures::try_join!(target_collection_fut, target_handle_fut)?;
        Ok(())
    };

    Ok((target_proxy, fut))
}

pub async fn is_discovery_enabled(ctx: &EnvironmentContext) -> bool {
    // TODO (b/355292969): put back the discovery check after we've addressed the flakes associated
    // with client-side discovery. (Currently re-enabled, but I want to validate the flake before resolving
    // this bug -slgrady 8/7/24)
    // true
    !ffx_config::is_usb_discovery_disabled(ctx).await
        || !ffx_config::is_mdns_discovery_disabled(ctx).await
}

#[derive(Debug, Error)]
pub enum KnockError {
    #[error("critical error encountered: {0:?}")]
    CriticalError(anyhow::Error),
    #[error("non-critical error encountered: {0:?}")]
    NonCriticalError(#[from] anyhow::Error),
}

// Derive from rcs knock timeout as this is the minimum amount of time to knock.
// Uses nanos to ensure that if RCS_KNOCK_TIMEOUT changes it is using the smallest unit possible.
//
// This is written as such due to some inconsistencies with Duration::from_nanos where `as_nanos()`
// returns a u128 but `from_nanos()` takes a u64.
pub const DEFAULT_RCS_KNOCK_TIMEOUT: Duration =
    Duration::new(rcs::RCS_KNOCK_TIMEOUT.as_secs() * 3, rcs::RCS_KNOCK_TIMEOUT.subsec_nanos() * 3);

impl From<ConnectionError> for KnockError {
    fn from(e: ConnectionError) -> Self {
        match e {
            ConnectionError::KnockError(ke) => KnockError::NonCriticalError(ke.into()),
            other => KnockError::CriticalError(other.into()),
        }
    }
}

/// Attempts to "knock" a target to determine if it is up and connectable via RCS.
///
/// This is intended to be run in a loop, with a non-critical error implying the caller
/// should call again, and a critical error implying the caller should raise the error
/// and no longer loop.
pub async fn knock_target(target: &TargetProxy) -> Result<(), KnockError> {
    knock_target_with_timeout(target, DEFAULT_RCS_KNOCK_TIMEOUT).await
}

#[derive(Debug, Clone, Copy)]
pub enum WaitFor {
    DeviceOnline,
    DeviceOffline,
}

const DOWN_REPOLL_DELAY_MS: u64 = 500;

pub async fn wait_for_device(
    wait_timeout: Option<Duration>,
    env: &EnvironmentContext,
    target_spec: Option<String>,
    behavior: WaitFor,
) -> Result<(), ffx_command::Error> {
    wait_for_device_inner(LocalRcsKnockerImpl, wait_timeout, env, target_spec, behavior).await
}

async fn wait_for_device_inner(
    knocker: impl RcsKnocker,
    wait_timeout: Option<Duration>,
    env: &EnvironmentContext,
    target_spec: Option<String>,
    behavior: WaitFor,
) -> Result<(), ffx_command::Error> {
    let target_spec_clone = target_spec.clone();
    let env = env.clone();
    let knock_fut = async {
        loop {
            futures_lite::future::yield_now().await;
            break match knocker.knock_rcs(target_spec_clone.clone(), &env).await {
                Err(e) => {
                    tracing::debug!("unable to knock target: {e:?}");
                    if let WaitFor::DeviceOffline = behavior {
                        Ok(())
                    } else {
                        if let KnockError::CriticalError(e) = e {
                            Err(ffx_command::Error::Unexpected(e.into()))
                        } else {
                            tracing::debug!("error non-critical. retrying.");
                            async_io::Timer::after(Duration::from_millis(DOWN_REPOLL_DELAY_MS))
                                .await;
                            continue;
                        }
                    }
                }
                Ok(()) => {
                    if let WaitFor::DeviceOffline = behavior {
                        async_io::Timer::after(Duration::from_millis(DOWN_REPOLL_DELAY_MS)).await;
                        continue;
                    } else {
                        Ok(())
                    }
                }
            };
        }
    };
    let timer = if wait_timeout.is_some() {
        async_io::Timer::after(wait_timeout.unwrap())
    } else {
        async_io::Timer::never()
    };
    futures_lite::FutureExt::or(knock_fut, async {
        timer.await;
        Err(ffx_command::Error::User(
            FfxError::DaemonError { err: DaemonError::Timeout, target: target_spec }.into(),
        ))
    })
    .await
}

#[cfg_attr(test, mockall::automock)]
trait RcsKnocker {
    fn knock_rcs(
        &self,
        target_spec: Option<String>,
        env: &EnvironmentContext,
    ) -> impl Future<Output = Result<(), KnockError>>;
}

struct LocalRcsKnockerImpl;

impl<T: RcsKnocker + ?Sized> RcsKnocker for Box<T> {
    fn knock_rcs(
        &self,
        target_spec: Option<String>,
        env: &EnvironmentContext,
    ) -> impl Future<Output = Result<(), KnockError>> {
        (**self).knock_rcs(target_spec, env)
    }
}

impl RcsKnocker for LocalRcsKnockerImpl {
    async fn knock_rcs(
        &self,
        target_spec: Option<String>,
        env: &EnvironmentContext,
    ) -> Result<(), KnockError> {
        knock_target_daemonless(&target_spec, env, Some(DEFAULT_RCS_KNOCK_TIMEOUT)).await.map(
            |compat| {
                let msg = match compat {
                    Some(c) => format!("Received compat info: {c:?}"),
                    None => format!("No compat info received"),
                };
                tracing::debug!("Knocked target. {msg}");
            },
        )
    }
}

/// Attempts to "knock" a target to determine if it is up and connectable via RCS, within
/// a specified timeout.
///
/// This is intended to be run in a loop, with a non-critical error implying the caller
/// should call again, and a critical error implying the caller should raise the error
/// and no longer loop.
///
/// The timeout must be longer than `rcs::RCS_KNOCK_TIMEOUT`
async fn knock_target_with_timeout(
    target: &TargetProxy,
    rcs_timeout: Duration,
) -> Result<(), KnockError> {
    if rcs_timeout <= rcs::RCS_KNOCK_TIMEOUT {
        return Err(KnockError::CriticalError(anyhow::anyhow!(
            "rcs verification timeout must be greater than {:?}",
            rcs::RCS_KNOCK_TIMEOUT
        )));
    }
    let (rcs_proxy, remote_server_end) = create_proxy::<RemoteControlMarker>()
        .map_err(|e| KnockError::NonCriticalError(e.into()))?;
    timeout(rcs_timeout, target.open_remote_control(remote_server_end))
        .await
        .context("timing out")?
        .context("opening remote_control")?
        .map_err(|e| anyhow::anyhow!("open remote control err: {:?}", e))?;
    rcs::knock_rcs(&rcs_proxy)
        .await
        .map_err(|e| KnockError::NonCriticalError(anyhow::anyhow!("{e:?}")))
}

/// Same as `knock_target_with_timeout` but takes a `TargetCollection` and an
/// optional target name and finds the target to knock. Uses the configured
/// default target if `target_name` is `None`.
pub async fn knock_target_by_name(
    target_name: &Option<String>,
    target_collection_proxy: &TargetCollectionProxy,
    open_timeout: Duration,
    rcs_timeout: Duration,
) -> Result<(), KnockError> {
    let (target_proxy, target_remote) =
        create_proxy::<TargetMarker>().map_err(|e| KnockError::NonCriticalError(e.into()))?;

    timeout::timeout(
        open_timeout,
        target_collection_proxy.open_target(
            &TargetQuery { string_matcher: target_name.clone(), ..Default::default() },
            target_remote,
        ),
    )
    .await
    .map_err(|_e| {
        KnockError::NonCriticalError(errors::ffx_error!("Timeout opening target.").into())
    })?
    .map_err(|e| {
        KnockError::CriticalError(
            errors::ffx_error!("Lost connection to the Daemon. Full context:\n{}", e).into(),
        )
    })?
    .map_err(|e| {
        KnockError::CriticalError(errors::ffx_error!("Error opening target: {:?}", e).into())
    })?;

    knock_target_with_timeout(&target_proxy, rcs_timeout).await
}

/// Identical to the above "knock_target" but does not use the daemon.
///
/// Keep in mind because there is no daemon being used, the connection process must be bootstrapped
/// for each attempt, so this function may need more time to run than the functions that perform
/// this action through the daemon (which is presumed to be already active). As a result, if
/// `knock_timeout` is set to `None`, the default timeout will be set to 2 times
/// `DEFAULT_RCS_KNOCK_TIMEOUT`.
pub async fn knock_target_daemonless(
    target_spec: &Option<String>,
    context: &EnvironmentContext,
    knock_timeout: Option<Duration>,
) -> Result<Option<CompatibilityInfo>, KnockError> {
    let knock_timeout = knock_timeout.unwrap_or(DEFAULT_RCS_KNOCK_TIMEOUT * 2);
    let res_future = async {
        tracing::trace!("resolving target spec address from {target_spec:?}");
        let res = resolve::resolve_target_address(target_spec, context).await?;
        tracing::debug!("daemonless knock connecting to address {}", res.addr()?);
        let conn = match res.connection {
            Some(c) => c,
            None => {
                let conn = connection::Connection::new(
                    ssh_connector::SshConnector::new(res.addr()?, context).await?,
                )
                .await
                .map_err(|e| KnockError::CriticalError(e.into()))?;
                tracing::debug!("daemonless knock connection established");
                let _ =
                    conn.rcs_proxy().await.map_err(|e| KnockError::NonCriticalError(e.into()))?;
                conn
            }
        };
        Ok(conn.compatibility_info())
    };
    futures_lite::pin!(res_future);
    timeout::timeout(knock_timeout, res_future)
        .await
        .map_err(|e| KnockError::NonCriticalError(e.into()))?
}

/// Get the target specifier.  This uses the normal config mechanism which
/// supports flexible config values: it can be a string naming the target, or
/// a list of strings, in which case the first valid entry is used. (The most
/// common use of this functionality would be to specify an array of environment
/// variables, e.g. ["$FUCHSIA_TARGET_ADDR", "FUCHSIA_NODENAME"]).
/// The result is a string which can be turned into a `TargetInfoQuery` to match
/// against the available targets (by name, address, etc). We don't return the query
/// itself, because some callers assume the specifier is the name of the target,
/// for the purposes of error messages, etc.  E.g. The repo server only works if
/// an explicit _name_ is provided.  In other contexts, it is valid for the specifier
/// to be a substring, a network address, etc.
pub async fn get_target_specifier(context: &EnvironmentContext) -> Result<Option<String>> {
    let target_spec = context.get(TARGET_DEFAULT_KEY)?;
    match target_spec {
        Some(ref target) => info!("Target specifier: ['{target:?}']"),
        None => debug!("No target specified"),
    }
    Ok(target_spec)
}

pub async fn add_manual_target(
    target_collection_proxy: &TargetCollectionProxy,
    addr: IpAddr,
    scope_id: u32,
    port: u16,
    wait: bool,
) -> Result<()> {
    let ip = match addr {
        IpAddr::V6(i) => net::IpAddress::Ipv6(net::Ipv6Address { addr: i.octets().into() }),
        IpAddr::V4(i) => net::IpAddress::Ipv4(net::Ipv4Address { addr: i.octets().into() }),
    };
    let addr = if port > 0 {
        ffx::TargetAddrInfo::IpPort(ffx::TargetIpPort { ip, port, scope_id })
    } else {
        ffx::TargetAddrInfo::Ip(ffx::TargetIp { ip, scope_id })
    };

    let (client, mut stream) =
        fidl::endpoints::create_request_stream::<ffx::AddTargetResponder_Marker>()
            .context("create endpoints")?;
    target_collection_proxy
        .add_target(
            &addr,
            &ffx::AddTargetConfig { verify_connection: Some(wait), ..Default::default() },
            client,
        )
        .context("calling AddTarget")?;
    let res = if let Ok(Some(req)) = stream.try_next().await {
        match req {
            ffx::AddTargetResponder_Request::Success { .. } => Ok(()),
            ffx::AddTargetResponder_Request::Error { err, .. } => Err(err),
        }
    } else {
        ffx_bail!("ffx lost connection to the daemon before receiving a response.");
    };

    // Change TargetAddrInfo to TargetAddr so ip can be extracted.
    // This is similar logic found in get_ssh_address().
    const DEFAULT_SSH_PORT: u16 = 22;
    let taddr = TargetAddr::from(&addr);
    let taddr_str = match taddr.ip() {
        IpAddr::V4(_) => format!("{}", taddr),
        IpAddr::V6(_) => format!("[{}]", taddr),
    };

    // Pass formatted ip and port to target connection error, so it is more user friendly
    res.map_err(|e| {
        let err = e.connection_error.unwrap();
        let logs = e.connection_error_logs.map(|v| v.join("\n"));
        let target = Some(format!(
            "{}:{}",
            taddr_str,
            if taddr.port() == 0 { DEFAULT_SSH_PORT } else { taddr.port() }
        ));
        FfxError::TargetConnectionError { err, target, logs }.into()
    })
}

#[cfg(test)]
mod test {
    use super::*;
    use ffx_command::bug;
    use ffx_config::macro_deps::serde_json::Value;
    use ffx_config::{test_init, ConfigLevel};
    use futures_lite::future::{pending, ready};

    #[fuchsia::test]
    async fn test_target_wait_too_short_timeout() {
        let (proxy, _server) = fidl::endpoints::create_proxy::<ffx::TargetMarker>().unwrap();
        let res = knock_target_with_timeout(&proxy, rcs::RCS_KNOCK_TIMEOUT).await;
        assert!(res.is_err());
        let res = knock_target_with_timeout(
            &proxy,
            rcs::RCS_KNOCK_TIMEOUT.checked_sub(Duration::new(0, 1)).unwrap(),
        )
        .await;
        assert!(res.is_err());
    }

    #[fuchsia::test(threads = 1)]
    async fn test_get_empty_default_target() {
        let env = test_init().await.unwrap();
        // Just in case, we need to remove the env variables mentioned
        // in the default config for "default.target". Because of the way
        // EnvironmentContext::env_var() works, we need to remove it from both
        // the context and the actual environment.
        const NODENAME_KEY: &str = "FUCHSIA_NODENAME";
        const ADDR_KEY: &str = "FUCHSIA_DEVICE_ADDR";
        let mut context = env.context.clone();

        context.remove_var(NODENAME_KEY);
        let fuchsia_nodename = std::env::var_os(NODENAME_KEY);
        if fuchsia_nodename.is_some() {
            // UNSAFE: remove_var() should not be called in multithreaded
            // environments; this test is explicitly marked as single-threaded,
            // so it is safe.
            unsafe {
                std::env::remove_var(NODENAME_KEY);
            }
        }
        context.remove_var(ADDR_KEY);
        let fuchsia_device_addr = std::env::var_os(ADDR_KEY);
        if fuchsia_device_addr.is_some() {
            // UNSAFE: remove_var() should not be called in multithreaded
            // environments; this test is explicitly marked as single-threaded,
            // so it is safe.
            unsafe {
                std::env::remove_var(ADDR_KEY);
            }
        }
        let target_spec = get_target_specifier(&context).await.unwrap();
        assert_eq!(target_spec, None);
        if let Some(nodename) = fuchsia_nodename {
            // UNSAFE: set_var() should not be called in multithreaded
            // environments; this test is explicitly marked as single-threaded,
            // so it is safe.
            unsafe {
                std::env::set_var(NODENAME_KEY, &nodename);
            }
        }
        if let Some(device_addr) = fuchsia_device_addr {
            // UNSAFE: set_var() should not be called in multithreaded
            // environments; this test is explicitly marked as single-threaded,
            // so it is safe.
            unsafe {
                std::env::set_var(ADDR_KEY, &device_addr);
            }
        }
    }

    #[fuchsia::test]
    async fn test_set_default_target() {
        let env = test_init().await.unwrap();
        env.context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .set(Value::String("some_target".to_owned()))
            .await
            .unwrap();

        let target_spec = get_target_specifier(&env.context).await.unwrap();
        assert_eq!(target_spec, Some("some_target".to_owned()));
    }

    #[fuchsia::test]
    async fn test_default_first_target_in_array() {
        let env = test_init().await.unwrap();
        let ts: Vec<Value> = ["t1", "t2"].iter().map(|s| Value::String(s.to_string())).collect();
        env.context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .set(Value::Array(ts))
            .await
            .unwrap();

        let target_spec = get_target_specifier(&env.context).await.unwrap();
        assert_eq!(target_spec, Some("t1".to_owned()));
    }

    #[fuchsia::test]
    async fn test_default_missing_env_ignored() {
        let env = test_init().await.unwrap();
        let ts: Vec<Value> =
            ["$THIS_BETTER_NOT_EXIST", "t2"].iter().map(|s| Value::String(s.to_string())).collect();
        env.context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .set(Value::Array(ts))
            .await
            .unwrap();

        let target_spec = get_target_specifier(&env.context).await.unwrap();
        assert_eq!(target_spec, Some("t2".to_owned()));
    }

    #[fuchsia::test]
    async fn test_default_env_present() {
        std::env::set_var("MY_LITTLE_TMPKEY", "t1");
        let env = test_init().await.unwrap();
        let ts: Vec<Value> =
            ["$MY_LITTLE_TMPKEY", "t2"].iter().map(|s| Value::String(s.to_string())).collect();
        env.context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .set(Value::Array(ts))
            .await
            .unwrap();

        let target_spec = get_target_specifier(&env.context).await.unwrap();
        assert_eq!(target_spec, Some("t1".to_owned()));
        std::env::remove_var("MY_LITTLE_TMPKEY");
    }

    #[fuchsia::test]
    async fn test_bad_timeout() {
        let env = test_init().await.unwrap();
        assert!(knock_target_daemonless(
            &Some("foo".to_string()),
            &env.context,
            Some(rcs::RCS_KNOCK_TIMEOUT)
        )
        .await
        .is_err());
    }

    #[fuchsia::test]
    async fn wait_for_device_knock_works() {
        let mut mock = MockRcsKnocker::new();
        mock.expect_knock_rcs().returning(|_, _| Box::pin(async { Ok(()) }));
        let env = ffx_config::test_init().await.unwrap();
        let res = wait_for_device_inner(
            mock,
            Some(Duration::from_secs(10000)),
            &env.context,
            Some("foo".to_string()),
            WaitFor::DeviceOnline,
        )
        .await;
        assert!(res.is_ok(), "{:?}", res);
    }

    #[fuchsia::test]
    async fn wait_for_device_hangs_indefinitely() {
        let mut mock = MockRcsKnocker::new();
        mock.expect_knock_rcs().returning(|_, _| Box::pin(pending()));
        let env = ffx_config::test_init().await.unwrap();
        let res = wait_for_device_inner(
            mock,
            Some(Duration::from_secs(5)),
            &env.context,
            Some("foo".to_string()),
            WaitFor::DeviceOnline,
        )
        .await;
        assert!(res.is_err(), "{:?}", res);
    }

    #[fuchsia::test]
    async fn wait_for_device_critical_error_causes_failure() {
        let mut mock = MockRcsKnocker::new();
        mock.expect_knock_rcs().times(1).returning(|_, _| {
            Box::pin(async { Err(KnockError::CriticalError(bug!("Oh no!").into())) })
        });
        let env = ffx_config::test_init().await.unwrap();
        let res = wait_for_device_inner(
            mock,
            Some(Duration::from_secs(5)),
            &env.context,
            Some("foo".to_string()),
            WaitFor::DeviceOnline,
        )
        .await;
        assert!(res.is_err(), "{:?}", res);
    }

    #[fuchsia::test]
    async fn wait_for_device_critical_error_does_not_cause_failure_waiting_for_down() {
        let mut mock = MockRcsKnocker::new();
        mock.expect_knock_rcs().times(1).returning(|_, _| {
            Box::pin(async { Err(KnockError::CriticalError(bug!("Oh no!").into())) })
        });
        let env = ffx_config::test_init().await.unwrap();
        let res = wait_for_device_inner(
            mock,
            Some(Duration::from_secs(5)),
            &env.context,
            Some("foo".to_string()),
            WaitFor::DeviceOffline,
        )
        .await;
        assert!(res.is_ok(), "{:?}", res);
    }

    #[fuchsia::test]
    async fn non_critical_error_causes_eventual_timeout() {
        let mut mock = MockRcsKnocker::new();
        mock.expect_knock_rcs().returning(|_, _| {
            Box::pin(async { Err(KnockError::NonCriticalError(bug!("Oh no!").into())) })
        });
        let env = ffx_config::test_init().await.unwrap();
        let res = wait_for_device_inner(
            mock,
            Some(Duration::from_secs(3)),
            &env.context,
            Some("foo".to_string()),
            WaitFor::DeviceOnline,
        )
        .await;
        assert!(res.is_err(), "{:?}", res);
    }

    #[fuchsia::test]
    async fn non_critical_error_returns_ok_for_down_target() {
        let mut mock = MockRcsKnocker::new();
        mock.expect_knock_rcs().returning(|_, _| {
            Box::pin(async { Err(KnockError::NonCriticalError(bug!("Oh no!").into())) })
        });
        let env = ffx_config::test_init().await.unwrap();
        let res = wait_for_device_inner(
            mock,
            Some(Duration::from_secs(5)),
            &env.context,
            Some("foo".to_string()),
            WaitFor::DeviceOffline,
        )
        .await;
        assert!(res.is_ok(), "{:?}", res);
    }

    #[fuchsia::test]
    async fn knock_error_reattempt_successful() {
        let mut mock = MockRcsKnocker::new();
        let mut seq = mockall::Sequence::new();
        mock.expect_knock_rcs().times(1).in_sequence(&mut seq).returning(|_, _| {
            Box::pin(ready(Err(KnockError::NonCriticalError(bug!("timeout").into()))))
        });
        mock.expect_knock_rcs()
            .times(1)
            .in_sequence(&mut seq)
            .returning(|_, _| Box::pin(ready(Ok(()))));
        let env = ffx_config::test_init().await.unwrap();
        let res = wait_for_device_inner(
            mock,
            Some(Duration::from_secs(10)),
            &env.context,
            Some("foo".to_string()),
            WaitFor::DeviceOnline,
        )
        .await;
        assert!(res.is_ok(), "{:?}", res);
    }

    #[fuchsia::test]
    async fn wait_for_offline_after_online() {
        let mut mock = MockRcsKnocker::new();
        let mut seq = mockall::Sequence::new();
        mock.expect_knock_rcs()
            .times(1)
            .in_sequence(&mut seq)
            .returning(|_, _| Box::pin(ready(Ok(()))));
        mock.expect_knock_rcs()
            .times(1)
            .in_sequence(&mut seq)
            .returning(|_, _| Box::pin(ready(Ok(()))));
        mock.expect_knock_rcs().times(1).in_sequence(&mut seq).returning(|_, _| {
            Box::pin(ready(Err(KnockError::NonCriticalError(
                bug!("Oh no it's not connected").into(),
            ))))
        });
        let env = ffx_config::test_init().await.unwrap();
        let res = wait_for_device_inner(
            mock,
            Some(Duration::from_secs(10)),
            &env.context,
            Some("foo".to_string()),
            WaitFor::DeviceOffline,
        )
        .await;
        assert!(res.is_ok(), "{:?}", res);
    }

    #[fuchsia::test]
    async fn wait_for_down_when_able_to_connect_to_device() {
        let mut mock = MockRcsKnocker::new();
        mock.expect_knock_rcs().returning(|_, _| Box::pin(ready(Ok(()))));
        let env = ffx_config::test_init().await.unwrap();
        let res = wait_for_device_inner(
            mock,
            Some(Duration::from_secs(5)),
            &env.context,
            Some("foo".to_string()),
            WaitFor::DeviceOffline,
        )
        .await;
        assert!(res.is_err(), "{:?}", res);
    }
}
