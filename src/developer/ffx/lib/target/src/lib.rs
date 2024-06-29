// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use addr::TargetAddr;
use anyhow::{bail, Context as _, Result};
use compat_info::CompatibilityInfo;
use discovery::{DiscoverySources, TargetEvent, TargetHandle, TargetState};
use errors::{ffx_bail, FfxError};
use ffx_config::keys::TARGET_DEFAULT_KEY;
use ffx_config::EnvironmentContext;
use fidl::endpoints::create_proxy;
use fidl::prelude::*;
use fidl_fuchsia_developer_ffx::{
    self as ffx, DaemonError, DaemonProxy, TargetCollectionMarker, TargetCollectionProxy,
    TargetInfo, TargetMarker, TargetQuery,
};
use fidl_fuchsia_developer_remotecontrol::{
    IdentifyHostResponse, RemoteControlMarker, RemoteControlProxy,
};
use fidl_fuchsia_net as net;
use fuchsia_async::TimeoutExt;
use futures::future::join_all;
use futures::{select, Future, FutureExt, StreamExt, TryStreamExt};
use itertools::Itertools;
use netext::IsLocalAddr;
use std::cmp::Ordering;
use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;
use thiserror::Error;
use timeout::timeout;
use tracing::{debug, info};

mod connection;

#[cfg(test)]
use {mockall::mock, mockall::predicate::*};

mod fidl_pipe;
mod overnet_connector;
mod ssh_connector;

const SSH_PORT_DEFAULT: u16 = 22;
const DEFAULT_SSH_TIMEOUT_MS: u64 = 10000;

pub use connection::{Connection, ConnectionError};
pub use discovery::desc::{Description, FastbootInterface};
pub use discovery::query::TargetInfoQuery;
pub use fidl_pipe::{create_overnet_socket, FidlPipe};
pub use overnet_connector::{OvernetConnection, OvernetConnector};
pub use ssh_connector::SshConnector;

/// Re-export of [`fidl_fuchsia_developer_ffx::TargetProxy`] for ease of use
pub use fidl_fuchsia_developer_ffx::TargetProxy;

const FASTBOOT_INLINE_TARGET: &str = "ffx.fastboot.inline_target";
const CONFIG_LOCAL_DISCOVERY_TIMEOUT: &str = "discovery.timeout";
const CONFIG_TARGET_SSH_TIMEOUT: &str = "target.host_pipe_ssh_timeout";

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
    // See if we need to do local resolution. (Do it here not in
    // open_target_with_fut because o_t_w_f is not async)
    let target_spec = maybe_locally_resolve_target_spec(target_spec, context).await?;
    let (target_proxy, target_proxy_fut) =
        open_target_with_fut(target_spec.clone(), daemon_proxy, proxy_timeout, context)?;
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
            res = target_proxy_fut => {
                res?;
                if let Some(ref mut info_out) = target_info {
                    **info_out = Some(target_proxy.identity().await?);
                }
            }
        }
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
    !ffx_config::is_usb_discovery_disabled(ctx).await
        || !ffx_config::is_mdns_discovery_disabled(ctx).await
}

// Descriptions are used for matching against a TargetInfoQuery
fn handle_to_description(handle: &TargetHandle) -> Description {
    let (addresses, serial) = match &handle.state {
        TargetState::Product(target_addr) => (target_addr.clone(), None),
        TargetState::Fastboot(discovery::FastbootTargetState { serial_number: sn, .. }) => {
            (vec![], Some(sn.clone()))
        }
        _ => (vec![], None),
    };
    Description { nodename: handle.node_name.clone(), addresses, serial, ..Default::default() }
}

// Group the information collected when resolving the address. (This is
// particularly important for the rcs_proxy, which we may need when resolving
// a manual target -- we don't want make an RCS connection just to resolve the
// name, drop it, then re-establish it later.)
enum ResolutionTarget {
    Addr(SocketAddr),
    Serial(String),
}

impl ResolutionTarget {
    // If the target is a product, pull out the "best" address from the
    // target, and return it.
    fn from_target_handle(target: &TargetHandle) -> Result<ResolutionTarget> {
        match &target.state {
            TargetState::Product(ref addresses) => {
                if addresses.is_empty() {
                    bail!("Target discovered but does not contain addresses: {target:?}");
                }
                let mut addrs_sorted = addresses
                    .into_iter()
                    .map(SocketAddr::from)
                    .sorted_by(|a1, a2| {
                        match (a1.ip().is_link_local_addr(), a2.ip().is_link_local_addr()) {
                            (true, true) | (false, false) => Ordering::Equal,
                            (true, false) => Ordering::Less,
                            (false, true) => Ordering::Greater,
                        }
                    })
                    .collect::<Vec<_>>();
                let mut sock: SocketAddr = addrs_sorted.pop().unwrap();
                if sock.port() == 0 {
                    sock.set_port(SSH_PORT_DEFAULT);
                }
                Ok(ResolutionTarget::Addr(sock))
            }
            TargetState::Fastboot(fts) => Ok(ResolutionTarget::Serial(fts.serial_number.clone())),
            state => {
                Err(anyhow::anyhow!("Target discovered but not in the correct state: {state:?}"))
            }
        }
    }

    fn to_spec(&self) -> String {
        match &self {
            ResolutionTarget::Addr(ssh_addr) => {
                format!("{ssh_addr}")
            }
            ResolutionTarget::Serial(serial) => {
                format!("serial:{serial}")
            }
        }
    }
}

pub struct Resolution {
    target: ResolutionTarget,
    discovered: Option<TargetHandle>,
    connection: Option<Connection>,
    rcs_proxy: Option<RemoteControlProxy>,
    identify_host_response: Option<IdentifyHostResponse>,
}

impl Resolution {
    fn from_target(target: ResolutionTarget) -> Self {
        Self {
            target,
            discovered: None,
            connection: None,
            rcs_proxy: None,
            identify_host_response: None,
        }
    }
    fn from_addr(sa: SocketAddr) -> Self {
        let scope_id = if let SocketAddr::V6(addr) = sa { addr.scope_id() } else { 0 };
        let port = match sa.port() {
            0 => SSH_PORT_DEFAULT,
            p => p,
        };
        Self::from_target(ResolutionTarget::Addr(TargetAddr::new(sa.ip(), scope_id, port).into()))
    }

    fn from_target_handle(th: TargetHandle) -> Result<Self> {
        let target = ResolutionTarget::from_target_handle(&th)?;
        Ok(Self { discovered: Some(th), ..Self::from_target(target) })
    }

    pub fn addr(&self) -> Result<SocketAddr> {
        match self.target {
            ResolutionTarget::Addr(addr) => Ok(addr),
            _ => bail!("target resolved to serial, not socket_addr"),
        }
    }

    pub async fn get_connection(&mut self, context: &EnvironmentContext) -> Result<&Connection> {
        if self.connection.is_none() {
            let connector = SshConnector::new(self.addr()?, context).await?;
            let conn = Connection::new(connector)
                .await
                .map_err(|e| KnockError::CriticalError(e.into()))?;
            self.connection = Some(conn);
        }
        Ok(self.connection.as_ref().unwrap())
    }

    pub async fn get_rcs_proxy(
        &mut self,
        context: &EnvironmentContext,
    ) -> Result<&RemoteControlProxy> {
        if self.rcs_proxy.is_none() {
            let conn = self.get_connection(context).await?;
            self.rcs_proxy = Some(conn.rcs_proxy().await?);
        }
        Ok(self.rcs_proxy.as_ref().unwrap())
    }

    pub async fn identify(
        &mut self,
        context: &EnvironmentContext,
    ) -> Result<&IdentifyHostResponse> {
        if self.identify_host_response.is_none() {
            let rcs_proxy = self.get_rcs_proxy(context).await?;
            self.identify_host_response = Some(
                rcs_proxy
                    .identify_host()
                    .await?
                    .map_err(|e| anyhow::anyhow!("Error identifying host: {e:?}"))?,
            );
        }
        Ok(self.identify_host_response.as_ref().unwrap())
    }
}

pub trait QueryResolverT {
    #[allow(async_fn_in_trait)]
    async fn resolve_target_query(
        &self,
        query: TargetInfoQuery,
        ctx: &EnvironmentContext,
    ) -> Result<Vec<TargetHandle>>;

    #[allow(async_fn_in_trait)]
    async fn try_resolve_manual_target(
        &self,
        name: &str,
        ctx: &EnvironmentContext,
    ) -> Result<Option<Resolution>>;

    #[allow(async_fn_in_trait)]
    async fn resolve_target_address(
        &self,
        target_spec: &Option<String>,
        ctx: &EnvironmentContext,
    ) -> Result<Resolution> {
        let query = TargetInfoQuery::from(target_spec.clone());
        if let TargetInfoQuery::Addr(a) = query {
            return Ok(Resolution::from_addr(a));
        }
        let res = self.resolve_single_target(&target_spec, ctx).await?;
        let target_spec_info = target_spec.clone().unwrap_or_else(|| "<unspecified>".to_owned());
        tracing::debug!("resolved target spec {target_spec_info} to address {:?}", res.addr());
        Ok(res)
    }

    #[allow(async_fn_in_trait)]
    async fn resolve_single_target(
        &self,
        target_spec: &Option<String>,
        env_context: &EnvironmentContext,
    ) -> Result<Resolution, anyhow::Error> {
        let target_spec_info = target_spec.clone().unwrap_or_else(|| "<unspecified>".to_owned());
        let query = TargetInfoQuery::from(target_spec.clone());
        if let TargetInfoQuery::NodenameOrSerial(ref s) = query {
            match self.try_resolve_manual_target(s, env_context).await {
                Err(e) => {
                    tracing::debug!("Failed to resolve target {s} as manual target: {e:?}");
                }
                Ok(Some(res)) => return Ok(res),
                _ => (), // Keep going
            }
        }
        let mut handles = self.resolve_target_query(query, env_context).await?;
        if handles.len() == 0 {
            return Err(anyhow::anyhow!(
                "unable to resolve address for target '{target_spec_info}'"
            ));
        }
        if handles.len() > 1 {
            return Err(FfxError::DaemonError {
                err: DaemonError::TargetAmbiguous,
                target: target_spec.clone(),
            }
            .into());
        }
        Ok(Resolution::from_target_handle(handles.remove(0))?)
    }
}

#[cfg(test)]
mock! {
    QueryResolverT{}
    impl QueryResolverT for QueryResolverT {
        async fn resolve_target_query(
            &self,
            query: TargetInfoQuery,
            ctx: &EnvironmentContext,
        ) -> Result<Vec<TargetHandle>>;
        async fn try_resolve_manual_target(
            &self,
            name: &str,
            ctx: &EnvironmentContext,
        ) -> Result<Option<Resolution>>;
    }
}

/// Attempts to resolve a TargetInfoQuery into a list of discovered targets.
/// Useful when multiple results are reasonable (e.g. from `ffx target list`)
pub async fn resolve_target_query(
    query: TargetInfoQuery,
    ctx: &EnvironmentContext,
) -> Result<Vec<TargetHandle>> {
    resolve_target_query_with_sources(
        query,
        ctx,
        DiscoverySources::MDNS
            | DiscoverySources::USB
            | DiscoverySources::MANUAL
            | DiscoverySources::EMULATOR,
    )
    .await
}

pub async fn resolve_target_query_to_info(
    query: TargetInfoQuery,
    ctx: &EnvironmentContext,
) -> Result<Vec<ffx::TargetInfo>> {
    let handles = resolve_target_query(query, ctx).await?;
    let targets =
        join_all(handles.into_iter().map(|t| async { get_handle_info(t, ctx).await })).await;
    targets.into_iter().collect::<Result<Vec<ffx::TargetInfo>>>()
}

async fn get_handle_info(
    handle: TargetHandle,
    context: &EnvironmentContext,
) -> Result<ffx::TargetInfo> {
    let (target_state, addresses) = match handle.state {
        TargetState::Unknown => (ffx::TargetState::Unknown, None),
        TargetState::Product(target_addrs) => (ffx::TargetState::Product, Some(target_addrs)),
        TargetState::Fastboot(_) => (ffx::TargetState::Fastboot, None),
        TargetState::Zedboot => (ffx::TargetState::Zedboot, None),
    };
    let RetrievedTargetInfo { rcs_state, product_config, board_config, ssh_address } =
        if let Some(ref target_addrs) = addresses {
            RetrievedTargetInfo::get(context, target_addrs).await?
        } else {
            RetrievedTargetInfo::default()
        };
    let addresses =
        addresses.map(|ta| ta.into_iter().map(|x| x.into()).collect::<Vec<ffx::TargetAddrInfo>>());
    Ok(ffx::TargetInfo {
        nodename: handle.node_name,
        addresses,
        rcs_state: Some(rcs_state),
        target_state: Some(target_state),
        board_config,
        product_config,
        ssh_address: ssh_address.map(|a| TargetAddr::from(a).into()),
        ..Default::default()
    })
}

async fn try_get_target_info(
    addr: addr::TargetAddr,
    context: &EnvironmentContext,
) -> Result<(Option<String>, Option<String>), KnockError> {
    let connector =
        SshConnector::new(addr.into(), context).await.context("making ssh connector")?;
    let conn = Connection::new(connector).await.context("making direct connection")?;
    let rcs = conn.rcs_proxy().await.context("getting RCS proxy")?;
    let (pc, bc) = match rcs.identify_host().await {
        Ok(Ok(id_result)) => (id_result.product_config, id_result.board_config),
        _ => (None, None),
    };
    Ok((pc, bc))
}

struct RetrievedTargetInfo {
    rcs_state: ffx::RemoteControlState,
    product_config: Option<String>,
    board_config: Option<String>,
    ssh_address: Option<SocketAddr>,
}

impl Default for RetrievedTargetInfo {
    fn default() -> Self {
        Self {
            rcs_state: ffx::RemoteControlState::Unknown,
            product_config: None,
            board_config: None,
            ssh_address: None,
        }
    }
}

impl RetrievedTargetInfo {
    async fn get(context: &EnvironmentContext, addrs: &[addr::TargetAddr]) -> Result<Self> {
        let ssh_timeout: u64 =
            ffx_config::get(CONFIG_TARGET_SSH_TIMEOUT).await.unwrap_or(DEFAULT_SSH_TIMEOUT_MS);
        let ssh_timeout = Duration::from_millis(ssh_timeout);
        for addr in addrs {
            tracing::debug!("Trying to make a connection to {addr:?}");

            // If the port is 0, we treat that as the default ssh port.
            let mut addr = *addr;
            if addr.port() == 0 {
                addr.set_port(SSH_PORT_DEFAULT);
            }

            match try_get_target_info(addr, context)
                .on_timeout(ssh_timeout, || {
                    Err(KnockError::NonCriticalError(anyhow::anyhow!("knock_rcs() timed out")))
                })
                .await
            {
                Ok((product_config, board_config)) => {
                    return Ok(Self {
                        rcs_state: ffx::RemoteControlState::Up,
                        product_config,
                        board_config,
                        ssh_address: Some(addr.into()),
                    });
                }
                Err(KnockError::NonCriticalError(e)) => {
                    tracing::debug!("Could not connect to {addr:?}: {e:?}");
                    continue;
                }
                e => {
                    tracing::debug!("Got error {e:?} when trying to connect to {addr:?}");
                    return Ok(Self {
                        rcs_state: ffx::RemoteControlState::Unknown,
                        product_config: None,
                        board_config: None,
                        ssh_address: None,
                    });
                }
            }
        }
        Ok(Self {
            rcs_state: ffx::RemoteControlState::Down,
            product_config: None,
            board_config: None,
            ssh_address: None,
        })
    }
}

pub async fn resolve_target_query_with(
    query: TargetInfoQuery,
    ctx: &EnvironmentContext,
    usb: bool,
    mdns: bool,
) -> Result<Vec<TargetHandle>> {
    let mut sources = DiscoverySources::MANUAL | DiscoverySources::EMULATOR;
    if usb {
        sources = sources | DiscoverySources::USB;
    }
    if mdns {
        sources = sources | DiscoverySources::MDNS;
    }
    resolve_target_query_with_sources(query, ctx, sources).await
}

async fn resolve_target_query_with_sources(
    query: TargetInfoQuery,
    ctx: &EnvironmentContext,
    sources: DiscoverySources,
) -> Result<Vec<TargetHandle>> {
    // Get nodename, in case we're trying to find an exact match
    QueryResolver::new(sources).resolve_target_query(query, ctx).await
}

/// Attempts to resolve the query into a target's ssh-able address. It is an error
/// if it the query doesn't match exactly one target.
// Perhaps refactor as connect_to_target() -> Result<Connection>, since that seems
// to be the only way this function is used?
pub async fn resolve_target_address(
    target_spec: &Option<String>,
    ctx: &EnvironmentContext,
) -> Result<Resolution> {
    QueryResolver::default().resolve_target_address(target_spec, ctx).await
}

fn query_matches_handle(query: &TargetInfoQuery, h: &TargetHandle) -> bool {
    match query {
        TargetInfoQuery::NodenameOrSerial(ref s) => {
            if let Some(nn) = &h.node_name {
                if nn == s {
                    return true;
                }
            }
            if let TargetState::Fastboot(fts) = &h.state {
                if fts.serial_number == *s {
                    return true;
                }
            }
        }
        TargetInfoQuery::Serial(ref s) => {
            if let TargetState::Fastboot(fts) = &h.state {
                if fts.serial_number == *s {
                    return true;
                }
            }
        }
        TargetInfoQuery::Addr(ref sa) => {
            if let TargetState::Product(addrs) = &h.state {
                if addrs.iter().any(|a| a.ip() == sa.ip()) {
                    return true;
                }
            }
        }
        TargetInfoQuery::First => {}
    }
    false
}

struct QueryResolver {
    sources: DiscoverySources,
}

impl Default for QueryResolver {
    fn default() -> Self {
        Self::new(DiscoverySources::all())
    }
}

impl QueryResolver {
    fn new(sources: DiscoverySources) -> Self {
        Self { sources }
    }
}

impl QueryResolverT for QueryResolver {
    async fn resolve_target_query(
        &self,
        query: TargetInfoQuery,
        ctx: &EnvironmentContext,
    ) -> Result<Vec<TargetHandle>> {
        // Get nodename, in case we're trying to find an exact match
        let query_clone = query.clone();
        let filter = move |handle: &TargetHandle| {
            let description = handle_to_description(handle);
            query_clone.match_description(&description)
        };
        let emu_instance_root: PathBuf = ctx.get(emulator_instance::EMU_INSTANCE_ROOT_DIR)?;
        let stream =
            discovery::wait_for_devices(filter, Some(emu_instance_root), true, false, self.sources)
                .await?;
        let discovery_delay = ctx.get(CONFIG_LOCAL_DISCOVERY_TIMEOUT).unwrap_or(2000);
        let delay = Duration::from_millis(discovery_delay);

        // This is tricky. We want the stream to complete immediately if we find
        // a target whose name/serial matches the query exactly. Otherwise, run
        // until the timer fires.
        // We can't use `Stream::wait_until()`, because that would require us
        // to return true for the found item, and false for the _next_ item.
        // But there may be no next item, so the stream would end up waiting
        // for the timer anyway. Instead, we create two futures: the timer, and
        // one that is ready when we find the target we're looking for. Then we
        // use `Stream::take_until()`, waiting until _either_ of those futures
        // is ready (by using `race()`). The only remaining tricky part is that
        // we need to examine each event to determine if it matches what we're
        // looking for -- so we interpose a closure via `Stream::map()` that
        // examines each item, before returning them unmodified.
        // Oh, and once we've got a set of results, if any of them are Err,
        // cause the whole thing to be an Err.  We could stop the race early in
        // case of failure by using the same technique, I suppose.
        let target_events: Result<Vec<TargetEvent>> = {
            let timer = fuchsia_async::Timer::new(delay).fuse();
            let found_target_event = async_utils::event::Event::new();
            let found_it = found_target_event.wait().fuse();
            let results: Vec<Result<_>> = stream
                .map(move |ev| {
                    if let Ok(TargetEvent::Added(ref h)) = ev {
                        if query_matches_handle(&query, h) {
                            found_target_event.signal();
                        }
                        ev
                    } else {
                        unreachable!()
                    }
                })
                .take_until(futures_lite::future::race(timer, found_it))
                .collect()
                .await;
            // Fail if any results are Err
            tracing::debug!("target events results: {results:?}");
            let r: Result<Vec<_>> = results.into_iter().collect();
            r
        };

        // Extract handles from Added events
        let added_handles: Vec<_> = target_events?
            .into_iter()
            .map(|e| {
                if let discovery::TargetEvent::Added(handle) = e {
                    handle
                } else {
                    unreachable!()
                }
            })
            .collect();

        // Sometimes libdiscovery returns multiple Added events for the same target (I think always
        // user emulators). The information is always the same, let's just extract the unique entries.
        let unique_handles = added_handles.into_iter().collect::<HashSet<_>>();
        Ok(unique_handles.into_iter().collect())
    }

    #[allow(async_fn_in_trait)]
    async fn try_resolve_manual_target(
        &self,
        name: &str,
        ctx: &EnvironmentContext,
    ) -> Result<Option<Resolution>> {
        // This is something that is often mocked for testing. An improvement here would be to use the
        // environment context for locating manual targets.
        let finder = manual_targets::Config::default();
        let ssh_timeout: u64 =
            ffx_config::get(CONFIG_TARGET_SSH_TIMEOUT).await.unwrap_or(DEFAULT_SSH_TIMEOUT_MS);
        let ssh_timeout = Duration::from_millis(ssh_timeout);
        let mut res = None;
        for t in manual_targets::watcher::parse_manual_targets(&finder).await.into_iter() {
            let addr = t.addr();
            let mut resolution = Resolution::from_addr(addr);
            let identify = resolution
                .identify(ctx)
                .on_timeout(ssh_timeout, || {
                    Err(anyhow::anyhow!(
                        "timeout after {ssh_timeout:?} identifying manual target {t:?}"
                    ))
                })
                .await?;

            if identify.nodename == Some(String::from(name)) {
                res = Some(resolution);
            }
        }
        Ok(res)
    }
}

/// Attempts to resolve the query into an explicit string query that can be
/// passed to the daemon. If already an address or serial number, just return
/// it. Otherwise, perform discovery to find the address or serial #. Returns
/// Some(_) if a target has been found, None otherwise.
async fn locally_resolve_target_spec<T: QueryResolverT>(
    target_spec: Option<String>,
    resolver: &T,
    env_context: &EnvironmentContext,
) -> Result<Option<String>> {
    let query = TargetInfoQuery::from(target_spec.clone());
    let explicit_spec = match query {
        TargetInfoQuery::Addr(addr) if addr.port() != 0 => format!("{addr}"),
        TargetInfoQuery::Serial(sn) => format!("serial:{sn}"),
        _ => {
            let resolution = resolver.resolve_single_target(&target_spec, env_context).await?;
            tracing::debug!(
                "Locally resolved target '{target_spec:?}' to {:?}",
                resolution.discovered
            );
            resolution.target.to_spec()
        }
    };

    Ok(Some(explicit_spec))
}

/// Check if daemon discovery is disabled, resolving locally if so.
pub async fn maybe_locally_resolve_target_spec(
    target_spec: Option<String>,
    env_context: &EnvironmentContext,
) -> Result<Option<String>> {
    if is_discovery_enabled(env_context).await {
        Ok(target_spec)
    } else {
        locally_resolve_target_spec(target_spec, &QueryResolver::default(), env_context).await
    }
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

const OPEN_TARGET_TIMEOUT: Duration = Duration::from_millis(1000);
const DOWN_REPOLL_DELAY_MS: u64 = 500;

pub async fn wait_for_device(
    wait_timeout: Option<Duration>,
    env: &EnvironmentContext,
    target_spec: Option<String>,
    target_collection: &TargetCollectionProxy,
    behavior: WaitFor,
) -> Result<(), ffx_command::Error> {
    if is_discovery_enabled(env).await {
        wait_for_device_inner(
            DaemonRcsKnockerImpl { tc_proxy: target_collection },
            wait_timeout,
            env,
            target_spec,
            behavior,
        )
        .await
    } else {
        wait_for_device_inner(LocalRcsKnockerImpl, wait_timeout, env, target_spec, behavior).await
    }
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
                Err(KnockError::CriticalError(e)) => Err(ffx_command::Error::Unexpected(e)),
                Err(KnockError::NonCriticalError(e)) => {
                    if let WaitFor::DeviceOffline = behavior {
                        Ok(())
                    } else {
                        tracing::debug!("unable to knock target: {e:?}");
                        async_io::Timer::after(Duration::from_millis(DOWN_REPOLL_DELAY_MS)).await;
                        continue;
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

impl<T: RcsKnocker + ?Sized> RcsKnocker for Box<T> {
    fn knock_rcs(
        &self,
        target_spec: Option<String>,
        env: &EnvironmentContext,
    ) -> impl Future<Output = Result<(), KnockError>> {
        (**self).knock_rcs(target_spec, env)
    }
}

struct LocalRcsKnockerImpl;

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

struct DaemonRcsKnockerImpl<'a> {
    tc_proxy: &'a TargetCollectionProxy,
}

impl<'a> RcsKnocker for DaemonRcsKnockerImpl<'a> {
    async fn knock_rcs(
        &self,
        target_spec: Option<String>,
        _env: &EnvironmentContext,
    ) -> Result<(), KnockError> {
        knock_target_by_name(
            &target_spec,
            self.tc_proxy,
            OPEN_TARGET_TIMEOUT,
            DEFAULT_RCS_KNOCK_TIMEOUT,
        )
        .await
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
        let res = resolve_target_address(target_spec, context).await?;
        tracing::debug!("daemonless knock connecting to address {}", res.addr()?);
        let conn = match res.connection {
            Some(c) => c,
            None => {
                let conn = Connection::new(SshConnector::new(res.addr()?, context).await?)
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
    res.map_err(|e| {
        let err = e.connection_error.unwrap();
        let logs = e.connection_error_logs.map(|v| v.join("\n"));
        let target = Some(format!("{addr:?}"));
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

    #[fuchsia::test]
    async fn test_get_empty_default_target() {
        let env = test_init().await.unwrap();
        let target_spec = get_target_specifier(&env.context).await.unwrap();
        assert_eq!(target_spec, None);
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
    async fn wait_for_device_critical_error_causes_failure_even_waiting_for_down() {
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
        assert!(res.is_err(), "{:?}", res);
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

    #[fuchsia::test]
    async fn test_can_resolve_target_locally() {
        let test_env = ffx_config::test_init().await.unwrap();
        let mut resolver = MockQueryResolverT::new();
        // A network address will resolve to itself
        let addr = "127.0.0.1:123".to_string();
        let addr_spec = Some(addr.clone());
        // Note that this will fail if we try to call resolve_target_spec()
        // since we haven't mocked a return value. So it's also checking that no
        // resolution is done.
        let target_spec =
            locally_resolve_target_spec(addr_spec.clone(), &resolver, &test_env.context)
                .await
                .unwrap();
        assert_eq!(target_spec, addr_spec.clone());

        // A serial spec will resolve to itself
        let sn = "abcdef".to_string();
        let sn_spec = format!("serial:{sn}");
        // Note that this will fail if we try to call resolve_target_spec()
        // since we still haven't mocked a return value. So it's also checking that no
        // resolution is done.
        let target_spec =
            locally_resolve_target_spec(Some(sn_spec.clone()), &resolver, &test_env.context)
                .await
                .unwrap();
        assert_eq!(target_spec, Some(sn_spec.clone()));

        // A DNS name will satisfy the resolution request
        let name_spec = Some("foobar".to_string());
        let sa = addr.parse::<SocketAddr>().unwrap();
        let state = TargetState::Product(vec![sa.into()]);
        let th = TargetHandle { node_name: name_spec.clone(), state };
        resolver.expect_resolve_target_query().return_once(move |_, _| Ok(vec![th]));
        resolver.expect_try_resolve_manual_target().return_once(move |_, _| Ok(None));
        let target_spec =
            locally_resolve_target_spec(name_spec.clone(), &resolver, &test_env.context)
                .await
                .unwrap();
        assert_eq!(target_spec, addr_spec);

        // A serial number for an existing target will satisfy the resolution request
        let mut resolver = MockQueryResolverT::new();
        let th = TargetHandle {
            node_name: None,
            state: TargetState::Fastboot(discovery::FastbootTargetState {
                serial_number: sn.clone(),
                connection_state: discovery::FastbootConnectionState::Usb,
            }),
        };
        resolver.expect_resolve_target_query().return_once(move |_, _| Ok(vec![th]));
        resolver.expect_try_resolve_manual_target().return_once(move |_, _| Ok(None));
        // Test with "<serial>", _not_ "serial:<serial>"
        let target_spec =
            locally_resolve_target_spec(Some(sn.clone()), &resolver, &test_env.context)
                .await
                .unwrap();
        assert_eq!(target_spec, Some(sn_spec));

        // An ambiguous name will result in an error
        let mut resolver = MockQueryResolverT::new();
        let name_spec = Some("foobar".to_string());
        let sa = addr.parse::<SocketAddr>().unwrap();
        let ts1 = TargetState::Product(vec![sa.into(), sa.into()]);
        let ts2 = TargetState::Product(vec![sa.into(), sa.into()]);
        let th1 = TargetHandle { node_name: name_spec.clone(), state: ts1 };
        let th2 = TargetHandle { node_name: name_spec.clone(), state: ts2 };
        resolver.expect_resolve_target_query().return_once(move |_, _| Ok(vec![th1, th2]));
        resolver.expect_try_resolve_manual_target().return_once(move |_, _| Ok(None));
        let target_spec_res =
            locally_resolve_target_spec(Some("foo".to_string()), &resolver, &test_env.context)
                .await;
        assert!(target_spec_res.is_err());
        assert!(dbg!(target_spec_res.unwrap_err().to_string()).contains("multiple targets"));
    }

    #[fuchsia::test]
    async fn test_locally_resolve_manual_before_mdns() {
        use manual_targets::ManualTargets;
        let test_env = ffx_config::test_init().await.unwrap();
        let mut resolver = MockQueryResolverT::new();
        let name_spec = Some("foobar".to_string());
        // Set up to return an mDNS address;
        let mdns_addr = "[fe80::8c14:9c4e:7c7c:c57]:123".to_string();
        let sa = mdns_addr.parse::<SocketAddr>().unwrap();
        let state = TargetState::Product(vec![sa.into()]);
        let th = TargetHandle { node_name: name_spec.clone(), state };
        resolver.expect_resolve_target_query().return_once(move |_, _| Ok(vec![th]));

        let mt_addr = "127.0.0.1:123".to_string();
        let mt_sa = mt_addr.parse::<SocketAddr>().unwrap();
        let mt_config = manual_targets::Config::default();
        // Also set up a manual target with a different result
        mt_config.add(mt_addr.clone(), None).await.expect("add manual target");
        resolver
            .expect_try_resolve_manual_target()
            .return_once(move |_, _| Ok(Some(Resolution::from_addr(mt_sa))));

        // Confirm that we get the manual address back
        let name_spec = Some("foobar".to_string());
        let target_spec =
            locally_resolve_target_spec(name_spec.clone(), &resolver, &test_env.context)
                .await
                .unwrap();
        assert_eq!(target_spec, Some(mt_addr));
    }

    // XXX Creating a reasonable test for the rest of the behavior:
    // * partial matching of names
    // * timing out when no matching targets return
    // * returning early when there is an exact name match
    // requires mocking a Stream<Item=TargetEvent>, which is difficult since a
    // trait for returning such a stream can't be made since the items are not ?
    // Sized (according to the rust compiler). So these additional tests will
    // require some more work.
}
