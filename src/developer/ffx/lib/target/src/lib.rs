// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use addr::TargetAddr;
use anyhow::{Context as _, Result};
use compat_info::CompatibilityInfo;
use discovery::{DiscoverySources, TargetEvent, TargetHandle, TargetState};
use errors::{ffx_bail, FfxError};
use ffx_config::{keys::TARGET_DEFAULT_KEY, EnvironmentContext};
use fidl::{endpoints::create_proxy, prelude::*};
use fidl_fuchsia_developer_ffx::{
    self as ffx, DaemonError, DaemonProxy, TargetAddrInfo, TargetCollectionMarker,
    TargetCollectionProxy, TargetInfo, TargetIp, TargetMarker, TargetQuery,
};
use fidl_fuchsia_developer_remotecontrol::{RemoteControlMarker, RemoteControlProxy};
use fidl_fuchsia_net as net;
use futures::{select, Future, FutureExt, StreamExt, TryStreamExt};
use itertools::Itertools;
use netext::IsLocalAddr;
use std::cmp::Ordering;
use std::net::{IpAddr, Ipv6Addr};
use std::net::{SocketAddr, SocketAddrV6};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use timeout::timeout;
use tracing::{debug, info};

mod desc;
mod fidl_pipe;
mod overnet_connector;
mod query;
mod ssh_connector;

const SSH_PORT_DEFAULT: u16 = 22;

pub use desc::{Description, FastbootInterface};
pub use fidl_pipe::create_overnet_socket;
pub use fidl_pipe::FidlPipe;
pub use query::TargetInfoQuery;

/// Re-export of [`fidl_fuchsia_developer_ffx::TargetProxy`] for ease of use
pub use fidl_fuchsia_developer_ffx::TargetProxy;

const FASTBOOT_INLINE_TARGET: &str = "ffx.fastboot.inline_target";
const DISCOVERY_TIMEOUT: &str = "discovery.timeout";

/// Attempt to connect to RemoteControl on a target device using a connection to a daemon.
///
/// The optional |target| is a string matcher as defined in fuchsia.developer.ffx.TargetQuery
/// fidl table.
#[tracing::instrument]
pub async fn get_remote_proxy(
    target: Option<String>,
    daemon_proxy: DaemonProxy,
    proxy_timeout: Duration,
    mut target_info: Option<&mut Option<TargetInfo>>,
    env_context: &EnvironmentContext,
) -> Result<RemoteControlProxy> {
    let (target_proxy, target_proxy_fut) =
        open_target_with_fut(target.clone(), daemon_proxy, proxy_timeout, env_context)?;
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
    let target = target.as_ref().map(ToString::to_string);
    match res {
        Ok(_) => Ok(remote_proxy),
        Err(err) => Err(anyhow::Error::new(FfxError::TargetConnectionError {
            err,
            target,
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
        let is_fastboot_inline = env_context.get(FASTBOOT_INLINE_TARGET).await.unwrap_or(false);
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

pub(crate) fn target_addr_info_to_socket(ti: &TargetAddrInfo) -> SocketAddr {
    let (target_ip, port) = match ti {
        TargetAddrInfo::Ip(a) => (a.clone(), 0),
        TargetAddrInfo::IpPort(ip) => (TargetIp { ip: ip.ip, scope_id: ip.scope_id }, ip.port),
    };
    let socket = match target_ip {
        TargetIp { ip: net::IpAddress::Ipv4(net::Ipv4Address { addr }), .. } => {
            SocketAddr::new(IpAddr::from(addr), port)
        }
        TargetIp { ip: net::IpAddress::Ipv6(net::Ipv6Address { addr }), scope_id, .. } => {
            SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::from(addr), port, 0, scope_id))
        }
    };
    socket
}

/// Attempts to resolve the query into a target's ssh-able address. Returns Some(_) if a target has been
/// found, None otherwise.
pub async fn resolve_target_address(
    query: TargetInfoQuery,
    env_context: &EnvironmentContext,
) -> Result<Option<SocketAddr>> {
    let nodename_or_serial = match query {
        TargetInfoQuery::NodenameOrSerial(nnos) => nnos,
        TargetInfoQuery::First => {
            return Ok(None);
        }
        TargetInfoQuery::Addr(a) => {
            let scope_id = if let SocketAddr::V6(addr) = a { addr.scope_id() } else { 0 };
            return Ok(Some(TargetAddr::new(a.ip(), scope_id, SSH_PORT_DEFAULT).into()));
        }
    };
    let mut stream = discovery::wait_for_devices(
        move |handle: &TargetHandle| {
            // TODO(b/329328874): This should use query matching.
            // This will only match the nodename initially because checking the product state and
            // returning an error will make it clearer to the user what the source of the error is.
            handle.node_name.as_ref().map(|n| *n == nodename_or_serial).unwrap_or(false)
        },
        true,
        false,
        DiscoverySources::MDNS
            | DiscoverySources::USB
            | DiscoverySources::MANUAL
            | DiscoverySources::EMULATOR,
    )
    .await?;
    let delay = Duration::from_millis(env_context.get(DISCOVERY_TIMEOUT).await.unwrap_or(1000));
    let event = timeout(delay, async { stream.next().await.transpose() })
        .await??
        .expect("target event should have been received");
    match event {
        TargetEvent::Added(target) => match target.state {
            TargetState::Product(ref addresses) => {
                if addresses.is_empty() {
                    return Err(anyhow::anyhow!(
                        "Target discovered but does not contain addresses: {target:?}"
                    ));
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
                    sock.set_port(SSH_PORT_DEFAULT)
                }
                Ok(Some(sock))
            }
            state => {
                Err(anyhow::anyhow!("Target discovered but not in the correct state: {state:?}"))
            }
        },
        _ => unreachable!(),
    }
}

#[derive(Debug, Error)]
pub enum KnockError {
    #[error("critical error encountered: {0:?}")]
    CriticalError(anyhow::Error),
    #[error("non-critical error encountered: {0:?}")]
    NonCriticalError(#[from] anyhow::Error),
}

const RCS_TIMEOUT: Duration = Duration::from_secs(3);

/// Attempts to "knock" a target to determine if it is up and connectable via RCS.
///
/// This is intended to be run in a loop, with a non-critical error implying the caller
/// should call again, and a critical error implying the caller should raise the error
/// and no longer loop.
pub async fn knock_target(target: &TargetProxy) -> Result<(), KnockError> {
    knock_target_with_timeout(target, RCS_TIMEOUT).await
}

/// Attempts to "knock" a target to determine if it is up and connectable via RCS, within
/// a specified timeout.
///
/// This is intended to be run in a loop, with a non-critical error implying the caller
/// should call again, and a critical error implying the caller should raise the error
/// and no longer loop.
pub async fn knock_target_with_timeout(
    target: &TargetProxy,
    rcs_timeout: Duration,
) -> Result<(), KnockError> {
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

struct OvernetClient {
    node: Arc<overnet_core::Router>,
}

impl OvernetClient {
    async fn locate_remote_control_node(&self) -> Result<overnet_core::NodeId> {
        let lpc = self.node.new_list_peers_context().await;
        let node_id;
        'found: loop {
            let new_peers = lpc.list_peers().await?;
            for peer in &new_peers {
                let peer_has_remote_control =
                    peer.services.contains(&RemoteControlMarker::PROTOCOL_NAME.to_string());
                if peer_has_remote_control {
                    node_id = peer.node_id;
                    break 'found;
                }
            }
        }
        Ok(node_id)
    }

    /// This is the remote control proxy that should be used for everything.
    ///
    /// If this is dropped, it will close the FidlPipe connection.
    pub(crate) async fn connect_remote_control(&self) -> Result<RemoteControlProxy> {
        let (server, client) = fidl::Channel::create();
        let node_id = self.locate_remote_control_node().await?;
        let _ = self
            .node
            .connect_to_service(node_id, RemoteControlMarker::PROTOCOL_NAME, server)
            .await?;
        let proxy = RemoteControlProxy::new(fidl::AsyncChannel::from_channel(client));
        Ok(proxy)
    }
}

/// Identical to the above "knock" but does not use the daemon.
///
/// Unlike other errors, this is not intended to be run in a tight loop.
pub async fn knock_target_daemonless(
    target_query_string: String,
    context: &EnvironmentContext,
) -> Result<Option<CompatibilityInfo>, KnockError> {
    let query = TargetInfoQuery::from(target_query_string.as_str());
    let addr = resolve_target_address(query, context)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!("Unable to resolve address of target '{target_query_string}'")
        })
        .context("resolving target address")?;
    let node = overnet_core::Router::new(None)?;
    let fidl_pipe =
        FidlPipe::new(context.clone(), addr, node.clone()).await.context("starting fidl pipe")?;
    knock_target_daemonless_with(node, fidl_pipe).await
}

async fn knock_target_daemonless_with(
    node: Arc<overnet_core::Router>,
    fidl_pipe: FidlPipe,
) -> Result<Option<CompatibilityInfo>, KnockError> {
    let error_stream = fidl_pipe.error_stream();
    let client = OvernetClient { node };
    // These are the two places where errors can propagate to the user. For other code using the FIDL
    // pipe it might be trickier to ensure that errors from the pipe are caught. For things like
    // FHO integration these errors will probably just be handled outside of the main subtool.
    let result = async move {
        let rcs_proxy = client.connect_remote_control().await?;
        rcs::knock_rcs(&rcs_proxy).await.map_err(|e| anyhow::anyhow!("{e:?}"))
    }
    .await;
    match result {
        Ok(()) => Ok(fidl_pipe.compatibility_info()),
        Err(e) => {
            let mut pipe_errors = Vec::new();
            while let Ok(err) = error_stream.try_recv() {
                pipe_errors.push(err);
            }
            if !pipe_errors.is_empty() {
                return Err(KnockError::NonCriticalError(anyhow::anyhow!(
                    "Error getting RCS proxy: {e:?}\n{:?}",
                    pipe_errors
                )));
            }
            Err(KnockError::NonCriticalError(anyhow::anyhow!("{:?}", e)).into())
        }
    }
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
    let target_spec = context.get(TARGET_DEFAULT_KEY).await?;
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
    use crate::overnet_connector::{OvernetConnection, OvernetConnector};
    use async_channel::Receiver;
    use ffx_config::{macro_deps::serde_json::Value, test_init, ConfigLevel};
    use fidl_fuchsia_developer_remotecontrol as rcs;
    use fuchsia_async::Task;

    fn create_overnet_circuit(router: Arc<overnet_core::Router>) -> fidl::AsyncSocket {
        let (local_socket, remote_socket) = fidl::Socket::create_stream();
        let local_socket = fidl::AsyncSocket::from_socket(local_socket);

        let socket = fidl::AsyncSocket::from_socket(remote_socket);
        let (mut rx, mut tx) = futures::AsyncReadExt::split(socket);
        Task::spawn(async move {
            let (errors_sender, errors) = futures::channel::mpsc::unbounded();
            if let Err(e) = futures::future::join(
                circuit::multi_stream::multi_stream_node_connection_to_async(
                    router.circuit_node(),
                    &mut rx,
                    &mut tx,
                    true,
                    circuit::Quality::NETWORK,
                    errors_sender,
                    "client".to_owned(),
                ),
                errors
                    .map(|e| {
                        eprintln!("A client circuit stream failed: {e:?}");
                    })
                    .collect::<()>(),
            )
            .map(|(result, ())| result)
            .await
            {
                if let circuit::Error::ConnectionClosed(msg) = e {
                    eprintln!("testing overnet link closed: {:?}", msg);
                } else {
                    eprintln!("error handling Overnet link: {:?}", e);
                }
            }
        })
        .detach();

        local_socket
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

    #[derive(Debug, Clone, Eq, PartialEq)]
    enum FakeOvernetBehavior {
        CloseRcsImmediately,
        KeepRcsOpen,
    }

    #[derive(Debug)]
    struct FakeOvernet {
        circuit_node: Arc<overnet_core::Router>,
        error_receiver: Receiver<anyhow::Error>,
        behavior: FakeOvernetBehavior,
    }

    impl FakeOvernet {
        async fn handle_transaction(
            req: rcs::RemoteControlRequest,
            behavior: &FakeOvernetBehavior,
        ) {
            match req {
                rcs::RemoteControlRequest::OpenCapability { server_channel, responder, .. } => {
                    match behavior {
                        FakeOvernetBehavior::KeepRcsOpen => {
                            // We're just going to assume this capability is always going to be
                            // RCS, and avoid string matching for the sake of avoiding changes
                            // to monikers and/or capability connecting.
                            let mut stream = rcs::RemoteControlRequestStream::from_channel(
                                fidl::AsyncChannel::from_channel(server_channel),
                            );
                            // This task is here to ensure the channel stays open, but won't
                            // necessarily do anything.
                            Task::spawn(async move {
                                while let Ok(Some(req)) = stream.try_next().await {
                                    eprintln!("Got a request: {req:?}");
                                }
                            })
                            .detach();
                        }
                        FakeOvernetBehavior::CloseRcsImmediately => {
                            drop(server_channel);
                        }
                    }
                    responder.send(Ok(())).unwrap();
                }
                _ => panic!("Received an unexpected request: {req:?}"),
            }
        }
    }

    impl OvernetConnector for FakeOvernet {
        async fn connect(&mut self) -> Result<OvernetConnection> {
            let circuit_socket = create_overnet_circuit(self.circuit_node.clone());
            let (rcs_sender, rcs_receiver) = async_channel::unbounded();
            self.circuit_node
                .register_service(
                    rcs::RemoteControlMarker::PROTOCOL_NAME.to_owned(),
                    move |channel| {
                        let _ = rcs_sender.try_send(channel).unwrap();
                        Ok(())
                    },
                )
                .await
                .unwrap();
            let behavior = self.behavior.clone();
            let rcs_task = Task::local(async move {
                while let Ok(channel) = rcs_receiver.recv().await {
                    let mut stream = rcs::RemoteControlRequestStream::from_channel(
                        fidl::AsyncChannel::from_channel(channel),
                    );
                    while let Ok(Some(req)) = stream.try_next().await {
                        Self::handle_transaction(req, &behavior).await;
                    }
                }
            });
            let (circuit_reader, circuit_writer) = tokio::io::split(circuit_socket);
            Ok(OvernetConnection {
                output: Box::new(tokio::io::BufReader::new(circuit_reader)),
                input: Box::new(circuit_writer),
                errors: self.error_receiver.clone(),
                compat: None,
                main_task: Some(rcs_task),
            })
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_overnet_rcs_knock() {
        let node = overnet_core::Router::new(None).unwrap();
        let overnet_socket = create_overnet_socket(node.clone()).unwrap();
        let (reader, writer) = tokio::io::split(overnet_socket);
        let circuit_node = overnet_core::Router::new(None).unwrap();
        let (_sender, error_receiver) = async_channel::unbounded();
        let circuit = FakeOvernet {
            circuit_node,
            error_receiver,
            behavior: FakeOvernetBehavior::KeepRcsOpen,
        };
        let fidl_pipe =
            FidlPipe::start_internal("127.0.0.1:22".parse().unwrap(), reader, writer, circuit)
                .await
                .unwrap();
        assert!(knock_target_daemonless_with(node, fidl_pipe).await.is_ok());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_overnet_rcs_knock_failure_disconnect() {
        let node = overnet_core::Router::new(None).unwrap();
        let overnet_socket = create_overnet_socket(node.clone()).unwrap();
        let (reader, writer) = tokio::io::split(overnet_socket);
        let circuit_node = overnet_core::Router::new(None).unwrap();
        let (error_sender, error_receiver) = async_channel::unbounded();
        let circuit = FakeOvernet {
            circuit_node: circuit_node.clone(),
            error_receiver,
            behavior: FakeOvernetBehavior::CloseRcsImmediately,
        };
        let fidl_pipe =
            FidlPipe::start_internal("[::1]:22".parse().unwrap(), reader, writer, circuit)
                .await
                .unwrap();
        error_sender.send(anyhow::anyhow!("kaboom")).await.unwrap();
        let err = knock_target_daemonless_with(node, fidl_pipe).await;
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("kaboom"));
    }
}
