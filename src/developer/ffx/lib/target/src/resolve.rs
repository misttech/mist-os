// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use addr::TargetAddr;
use anyhow::{anyhow, bail, Context, Result};
use discovery::desc::Description;
use discovery::query::TargetInfoQuery;
use discovery::{
    DiscoverySources, FastbootConnectionState, TargetEvent, TargetHandle, TargetState,
};
use ffx_config::EnvironmentContext;
use fidl_fuchsia_developer_ffx::{self as ffx};
use fidl_fuchsia_developer_remotecontrol::{IdentifyHostResponse, RemoteControlProxy};
use fuchsia_async::TimeoutExt;
use futures::future::join_all;
use futures::{pin_mut, select, FutureExt, StreamExt};
use itertools::Itertools;
use netext::IsLocalAddr;
use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;
use target_errors::FfxTargetError;

use crate::connection::Connection;
use crate::ssh_connector::SshConnector;

const CONFIG_TARGET_SSH_TIMEOUT: &str = "target.host_pipe_ssh_timeout";
const CONFIG_LOCAL_DISCOVERY_TIMEOUT: &str = "discovery.timeout";
const TARGET_DEFAULT_PORT: u16 = 22;
const DEFAULT_SSH_TIMEOUT_MS: u64 = 10000;

#[cfg(test)]
use {mockall::mock, mockall::predicate::*};

/// Check if daemon discovery is disabled, resolving locally if so.
pub async fn maybe_locally_resolve_target_spec(
    target_spec: Option<String>,
    env_context: &EnvironmentContext,
) -> Result<Option<String>> {
    if crate::is_discovery_enabled(env_context).await {
        Ok(target_spec)
    } else {
        locally_resolve_target_spec(target_spec, &QueryResolver::default(), env_context).await
    }
}

fn replace_default_port(sa: SocketAddr) -> SocketAddr {
    let mut sa = sa;
    if sa.port() == 0 {
        sa.set_port(TARGET_DEFAULT_PORT);
    }
    sa
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
        // If an address is passed in, make sure that the default port is filled in if it hadn't
        // been explicit, then just pass it on to the daemon as is
        TargetInfoQuery::Addr(addr) => format!("{}", replace_default_port(addr)),
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

trait QueryResolverT {
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
    ) -> Result<Resolution, FfxTargetError> {
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
    ) -> Result<Resolution, FfxTargetError> {
        let query = TargetInfoQuery::from(target_spec.clone());
        let mut handles;

        let handles_fut = self.resolve_target_query(query.clone(), env_context).fuse();
        // We want to query both the manual targets and the discoverable handles concurrently.
        if let TargetInfoQuery::NodenameOrSerial(ref s) = query {
            let manual_target_fut = self.try_resolve_manual_target(s, env_context).fuse();
            pin_mut!(manual_target_fut);
            pin_mut!(handles_fut);
            loop {
                select! {
                    mtr = manual_target_fut => match mtr {
                        Err(e) => {
                            tracing::debug!("Failed to resolve target {s} as manual target: {e:?}");
                            // Keep going, waiting for the discovery to complete
                        }
                        Ok(Some(res)) => return Ok(res), // We found a manual target, so we're done
                        _ => (), // Keep going
                    },
                    handles_res = handles_fut => {
                        // We got a response from discovery
                        handles = handles_res.map_err(|_| FfxTargetError::OpenTargetError { err: ffx::OpenTargetError::FailedDiscovery, target: target_spec.clone()})?;
                        break;
                    },
                }
            }
        } else {
            // If the query is not a nodename, we won't even bother with trying to resolve a manual
            // target.
            handles = handles_fut.await.map_err(|_| FfxTargetError::OpenTargetError {
                err: ffx::OpenTargetError::FailedDiscovery,
                target: target_spec.clone(),
            })?;
        };
        if handles.len() == 0 {
            return Err(FfxTargetError::OpenTargetError {
                err: ffx::OpenTargetError::TargetNotFound,
                target: target_spec.clone(),
            });
        }
        if handles.len() > 1 {
            return Err(FfxTargetError::OpenTargetError {
                err: ffx::OpenTargetError::QueryAmbiguous,
                target: target_spec.clone(),
            });
        }
        // Unwrap() is okay because we validate that we have at least one entry
        Ok(Resolution::from_target_handle(handles.remove(0)).unwrap())
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
        // TODO(colnnelson): Add DiscoverySources::FASTBOOT_FILE once feature
        // is announced.
        DiscoverySources::MDNS
            | DiscoverySources::USB
            | DiscoverySources::MANUAL
            | DiscoverySources::EMULATOR
            | DiscoverySources::FASTBOOT_FILE,
    )
    .await
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

async fn try_get_target_info(
    addr: SocketAddr,
    context: &EnvironmentContext,
) -> Result<(Option<String>, Option<String>), crate::KnockError> {
    let connector = SshConnector::new(addr, context).await.context("making ssh connector")?;
    let conn = Connection::new(connector).await.context("making direct connection")?;
    let rcs = conn.rcs_proxy().await.context("getting RCS proxy")?;
    let (pc, bc) = match rcs.identify_host().await {
        Ok(Ok(id_result)) => (id_result.product_config, id_result.board_config),
        _ => (None, None),
    };
    Ok((pc, bc))
}

impl RetrievedTargetInfo {
    async fn get(context: &EnvironmentContext, addrs: &[addr::TargetAddr]) -> Result<Self> {
        let ssh_timeout: u64 =
            ffx_config::get(CONFIG_TARGET_SSH_TIMEOUT).unwrap_or(DEFAULT_SSH_TIMEOUT_MS);
        let ssh_timeout = Duration::from_millis(ssh_timeout);
        for addr in addrs {
            // Ensure there's a port
            let addr = replace_default_port(addr.into());
            tracing::debug!("Trying to make a connection to {addr:?}");

            match try_get_target_info(addr, context)
                .on_timeout(ssh_timeout, || {
                    Err(crate::KnockError::NonCriticalError(anyhow::anyhow!(
                        "knock_rcs() timed out"
                    )))
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
                Err(crate::KnockError::NonCriticalError(e)) => {
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

pub async fn resolve_target_query_to_info(
    query: TargetInfoQuery,
    ctx: &EnvironmentContext,
) -> Result<Vec<ffx::TargetInfo>> {
    let handles = resolve_target_query(query, ctx).await?;
    let targets =
        join_all(handles.into_iter().map(|t| async { get_handle_info(t, ctx).await })).await;
    targets.into_iter().collect::<Result<Vec<ffx::TargetInfo>>>()
}

pub async fn resolve_target_query_with(
    query: TargetInfoQuery,
    ctx: &EnvironmentContext,
    usb: bool,
    mdns: bool,
) -> Result<Vec<TargetHandle>> {
    let mut sources =
        DiscoverySources::MANUAL | DiscoverySources::EMULATOR | DiscoverySources::FASTBOOT_FILE;
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
) -> Result<Resolution, FfxTargetError> {
    QueryResolver::default().resolve_target_address(target_spec, ctx).await
}
struct QueryResolver {
    sources: DiscoverySources,
}

impl Default for QueryResolver {
    fn default() -> Self {
        Self::new(DiscoverySources::all())
    }
}

/// Return a stream of handles matching the query. If a target
/// matches the query exactly (i.e. the query is the full name
/// of the target, not a substring), return the handle immediately
/// and close the stream. Otherwise, run until the timeout
/// (default 2 seconds, configurable via "discovery.timeout").
/// The contents of the stream can be filtered by USB and mDNS.
pub async fn get_discovery_stream(
    query: TargetInfoQuery,
    usb: bool,
    mdns: bool,
    ctx: &EnvironmentContext,
) -> Result<impl futures::Stream<Item = Result<TargetHandle>>> {
    let mut sources =
        DiscoverySources::MANUAL | DiscoverySources::EMULATOR | DiscoverySources::FASTBOOT_FILE;
    if usb {
        sources = sources | DiscoverySources::USB;
    }
    if mdns {
        sources = sources | DiscoverySources::MDNS;
    }
    // Get nodename, in case we're trying to find an exact match
    QueryResolver::new(sources).get_discovery_stream(query, ctx).await
}

impl QueryResolver {
    fn new(sources: DiscoverySources) -> Self {
        Self { sources }
    }

    async fn get_discovery_stream(
        &self,
        query: TargetInfoQuery,
        ctx: &EnvironmentContext,
    ) -> Result<impl futures::Stream<Item = Result<TargetHandle>>> {
        let query_clone = query.clone();
        let filter = move |handle: &TargetHandle| {
            let description = handle_to_description(handle);
            query_clone.match_description(&description)
        };
        let emu_instance_root: PathBuf = ctx.get(emulator_instance::EMU_INSTANCE_ROOT_DIR)?;
        let fastboot_file_path: Option<PathBuf> =
            ctx.get(fastboot_file_discovery::FASTBOOT_FILE_PATH).ok();
        let discovery_delay = ctx.get(CONFIG_LOCAL_DISCOVERY_TIMEOUT).unwrap_or(2000);
        let delay = Duration::from_millis(discovery_delay);
        let stream = discovery::wait_for_devices(
            filter,
            Some(emu_instance_root),
            fastboot_file_path,
            true,
            false,
            self.sources,
        )
        .await?;

        // This is tricky. We want the stream to complete immediately if we find
        // a target whose name/serial matches the query exactly. Otherwise, run
        // until the timer fires.
        // We can't use `Stream::take_until()`, because that would require us
        // to return true for the found item, and false for the _next_ item.
        // But there may be no next item, so the stream would end up waiting
        // for the timer anyway. Instead, we create two futures: the timer, and
        // one that is ready when we find the target we're looking for. Then we
        // use `Stream::take_until()`, waiting until _either_ of those futures
        // is ready (by using `race()`). The only remaining tricky part is that
        // we need to examine each event to determine if it matches what we're
        // looking for -- so we interpose a closure via `Stream::map()` that
        // examines each item, before returning them unmodified.
        // We could stop the race early in case of failure by using the same
        // technique, I suppose.
        let timer = fuchsia_async::Timer::new(delay).fuse();
        let found_target_event = async_utils::event::Event::new();
        let found_it = found_target_event.wait().fuse();
        // We can see the same handle multiple times (e.g. if it produces multiple
        // mDNS events during our timeout period). "seen" lets us dedup those
        // handles.
        let seen = Rc::new(RefCell::new(HashSet::new()));
        Ok(stream
            .filter_map(move |ev| {
                let found_ev = found_target_event.clone();
                let q_clone = query.clone();
                let seen = seen.clone();
                async move {
                    match ev {
                        Ok(TargetEvent::Added(ref h)) => {
                            if seen.borrow().contains(h) {
                                None
                            } else {
                                if query_matches_handle(&q_clone, h) {
                                    found_ev.signal();
                                }
                                seen.borrow_mut().insert(h.clone());
                                Some(Ok((*h).clone()))
                            }
                        }
                        // We've only asked for Added events
                        Ok(_) => unreachable!(),
                        Err(e) => Some(Err(e)),
                    }
                }
            })
            .take_until(futures_lite::future::race(timer, found_it)))
    }
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

// Descriptions are used for matching against a TargetInfoQuery
fn handle_to_description(handle: &TargetHandle) -> Description {
    let (addresses, serial) = match &handle.state {
        TargetState::Product(target_addr) => (target_addr.clone(), None),
        TargetState::Fastboot(discovery::FastbootTargetState {
            serial_number: sn,
            connection_state,
        }) => {
            let addresses = match connection_state {
                FastbootConnectionState::Usb => Vec::<TargetAddr>::new(),
                FastbootConnectionState::Tcp(addresses) => addresses.to_vec(),
                FastbootConnectionState::Udp(addresses) => addresses.to_vec(),
            };
            (addresses, Some(sn.clone()))
        }
        _ => (vec![], None),
    };
    Description { nodename: handle.node_name.clone(), addresses, serial, ..Default::default() }
}

impl QueryResolverT for QueryResolver {
    async fn resolve_target_query(
        &self,
        query: TargetInfoQuery,
        ctx: &EnvironmentContext,
    ) -> Result<Vec<TargetHandle>> {
        let results: Vec<Result<_>> = self.get_discovery_stream(query, ctx).await?.collect().await;
        // Fail if any results are Err
        tracing::debug!("target events results: {results:?}");
        // If any of the results in the stream are Err, cause the whole thing to
        // be an Err.
        results.into_iter().collect()
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
            ffx_config::get(CONFIG_TARGET_SSH_TIMEOUT).unwrap_or(DEFAULT_SSH_TIMEOUT_MS);
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

// Group the information collected when resolving the address. (This is
// particularly important for the rcs_proxy, which we may need when resolving
// a manual target -- we don't want make an RCS connection just to resolve the
// name, drop it, then re-establish it later.)
#[derive(Debug)]
enum ResolutionTarget {
    Addr(SocketAddr),
    Serial(String),
}

fn sort_socket_addrs(a1: &SocketAddr, a2: &SocketAddr) -> Ordering {
    match (a1.ip().is_link_local_addr(), a2.ip().is_link_local_addr()) {
        (true, true) | (false, false) => Ordering::Equal,
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
    }
}

fn choose_socketaddr_from_addresses(
    target: &TargetHandle,
    addresses: &Vec<TargetAddr>,
) -> Result<SocketAddr> {
    if addresses.is_empty() {
        bail!("Target discovered but does not contain addresses: {target:?}");
    }
    let mut addrs_sorted = addresses
        .into_iter()
        .map(SocketAddr::from)
        .sorted_by(sort_socket_addrs)
        .collect::<Vec<_>>();
    let sock: SocketAddr = addrs_sorted.pop().ok_or_else(|| {
        anyhow!("Choosing a socketaddr from a list of addresses, must contain at least one address")
    })?;
    Ok(sock)
}

impl ResolutionTarget {
    // If the target is a product, pull out the "best" address from the
    // target, and return it.
    fn from_target_handle(target: &TargetHandle) -> Result<ResolutionTarget> {
        match &target.state {
            TargetState::Product(ref addresses) => {
                let sock = choose_socketaddr_from_addresses(&target, addresses)?;
                let addr: SocketAddr = replace_default_port(sock);
                Ok(ResolutionTarget::Addr(addr))
            }
            TargetState::Fastboot(fts) => match &fts.connection_state {
                FastbootConnectionState::Usb => {
                    Ok(ResolutionTarget::Serial(fts.serial_number.clone()))
                }
                FastbootConnectionState::Tcp(addresses) => {
                    let sock: SocketAddr = choose_socketaddr_from_addresses(&target, addresses)?;
                    Ok(ResolutionTarget::Addr(sock))
                }
                FastbootConnectionState::Udp(addresses) => {
                    if addresses.is_empty() {
                        bail!("Target discovered but does not contain addresses: {target:?}");
                    }
                    let sock: SocketAddr = choose_socketaddr_from_addresses(&target, addresses)?;
                    Ok(ResolutionTarget::Addr(sock))
                }
            },
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

#[derive(Debug)]
pub struct Resolution {
    target: ResolutionTarget,
    discovered: Option<TargetHandle>,
    pub connection: Option<Connection>,
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
            0 => TARGET_DEFAULT_PORT,
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
                .map_err(|e| crate::KnockError::CriticalError(e.into()))?;
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

#[cfg(test)]
mod test {
    use super::*;
    use std::net::{Ipv6Addr, SocketAddrV6};

    #[fuchsia::test]
    async fn test_sort_socket_addrs() {
        struct TestCase {
            a1: SocketAddr,
            a2: SocketAddr,
            want_res: Ordering,
            name: String,
        }

        let cases: Vec<TestCase> = vec![
            TestCase {
                a1: "127.0.0.1:8082".parse().expect("Valid SocketAddr"),
                a2: "[::1]:8082".parse().expect("Valid SocketAddr"),
                want_res: Ordering::Equal,
                name: "Loopback v4 and v6 equal".to_string(),
            },
            TestCase {
                a1: "192.168.1.1:8082".parse().expect("Valid SocketAddr"),
                a2: "[2002:2::%2]:8082".parse().expect("Valid SocketAddr"),
                want_res: Ordering::Equal,
                name: "Non local v4 and non local v6 local equal".to_string(),
            },
            TestCase {
                // Local
                a1: SocketAddr::V6(SocketAddrV6::new(
                    Ipv6Addr::new(0xfe80, 0, 0, 0, 1, 6, 7, 8),
                    8080,
                    0,
                    1,
                )),
                // Non Local
                a2: SocketAddr::V6(SocketAddrV6::new(
                    Ipv6Addr::new(0x2607, 0xf8b0, 0x4005, 0x805, 0, 0, 0, 0x200e),
                    8080,
                    0,
                    1,
                )),
                want_res: Ordering::Less,
                name: "local v6 and non v6 local less".to_string(),
            },
            TestCase {
                // Local
                a1: SocketAddr::V6(SocketAddrV6::new(
                    Ipv6Addr::new(0xfe80, 0, 0, 0, 1, 6, 7, 8),
                    8080,
                    0,
                    1,
                )),
                // Local
                a2: SocketAddr::V6(SocketAddrV6::new(
                    Ipv6Addr::new(0xfe80, 0, 0, 0, 2, 6, 7, 8),
                    8080,
                    0,
                    1,
                )),
                want_res: Ordering::Equal,
                name: "local v6 and local v6 local equal".to_string(),
            },
        ];

        for case in cases {
            let got = sort_socket_addrs(&case.a1, &case.a2);
            assert_eq!(
                got, case.want_res,
                "TestCase: {0}. Results differ: {1:?}, {2:?}",
                case.name, got, case.want_res
            );
        }
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

    // XXX Creating a reasonable test for the rest of the behavior:
    // * partial matching of names
    // * timing out when no matching targets return
    // * returning early when there is an exact name match
    // requires mocking a Stream<Item=TargetEvent>, which is difficult since a
    // trait for returning such a stream can't be made since the items are not ?
    // Sized (according to the rust compiler). So these additional tests will
    // require some more work.
}
