// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Error};
use async_helpers::hanging_get::asynchronous as hanging_get;
use fidl_fuchsia_bluetooth_sys::{self as sys, AccessRequest, AccessRequestStream};
use fuchsia_bluetooth::types::pairing_options::{BondableMode, PairingOptions};
use fuchsia_bluetooth::types::{Peer, PeerId, Technology};
use fuchsia_sync::Mutex;
use futures::future::{pending, BoxFuture};
use futures::stream::{Count, FuturesUnordered, Stream, StreamExt};
use futures::{select, FutureExt};
use log::{debug, info, trace, warn};
use std::collections::HashMap;
use std::mem;
use std::sync::Arc;

use crate::host_dispatcher::*;
use crate::watch_peers::PeerWatcher;

#[derive(Default)]
struct AccessSession {
    peers_seen: Arc<Mutex<HashMap<PeerId, Peer>>>,
    /// Active `sys.Access` clients that are requesting to suppress connections. Connections will be
    /// suppressed as long as there is one or more outstanding `ProcedureToken`.
    suppress_connections_sessions: FuturesUnordered<Count<sys::ProcedureTokenRequestStream>>,
    /// Only one discovery session is stored per Access session at a time;
    /// if an Access client requests discovery while holding an existing session token,
    /// the old session is replaced, and the old session token is invalidated.
    discovery_session: Option<BoxFuture<'static, ()>>,
    discoverable_session: Option<BoxFuture<'static, ()>>,
}

pub async fn run(hd: HostDispatcher, mut stream: AccessRequestStream) -> Result<(), Error> {
    let mut watch_peers_subscriber = hd.watch_peers().await;
    let mut session: AccessSession = Default::default();
    let mut discovery_pending = pending().boxed();
    let mut discoverable_pending = pending().boxed();

    loop {
        select! {
            event_opt = stream.next() => {
                match event_opt {
                    Some(event) => handler(hd.clone(), &mut watch_peers_subscriber, &mut session, event?).await?,
                    None => break,
                }
            }
            _token_dropped = session.suppress_connections_sessions.select_next_some() => {
                // If this was the last token to be dropped, make the active host connectable again.
                if session.suppress_connections_sessions.is_empty() {
                    let res = hd.set_connectable(/*connectable=*/ true).await;
                    trace!("Last suppress connections token dropped. Setting connectable: {res:?}");
                }
            }
            _ = session.discovery_session.as_mut().unwrap_or(&mut discovery_pending).fuse() => {
                // drop the boxed future, which owns the discovery session token
                session.discovery_session = None;
            }
            _ = session.discoverable_session.as_mut().unwrap_or(&mut discoverable_pending).fuse() => {
                session.discoverable_session = None;
            }
        };
    }
    Ok(())
}

async fn handler(
    hd: HostDispatcher,
    watch_peers_subscriber: &mut hanging_get::Subscriber<PeerWatcher>,
    session: &mut AccessSession,
    request: AccessRequest,
) -> Result<(), Error> {
    match request {
        AccessRequest::SetPairingDelegate { input, output, delegate, .. } => {
            warn!("fuchsia.bluetooth.sys.Access.SetPairingDelegate({:?}, {:?})", input, output);
            if let Err(e) = hd.set_pairing_delegate(delegate.into_proxy(), input, output) {
                warn!("Couldn't set PairingDelegate: {e:?}");
            }
            Ok(())
        }
        AccessRequest::SetLocalName { name, .. } => {
            info!("fuchsia.bluetooth.sys.Access.SetLocalName(..)");
            if let Err(e) = hd.set_name(name, NameReplace::Replace).await {
                warn!("Error setting local name: {:?}", e);
            }
            Ok(())
        }
        AccessRequest::SetDeviceClass { device_class, .. } => {
            info!("fuchsia.bluetooth.sys.Access.SetDeviceClass(..)");
            if let Err(e) = hd.set_device_class(device_class).await {
                warn!("Error setting local name: {:?}", e);
            }
            Ok(())
        }
        AccessRequest::MakeDiscoverable { token, responder } => {
            info!("fuchsia.bluetooth.sys.Access.MakeDiscoverable(..)");
            let stream = token.into_stream(); // into_stream never fails
            let result = hd
                .set_discoverable()
                .await
                .map(|token| {
                    session.discoverable_session =
                        Some(watch_stream_for_session(stream, token).boxed());
                })
                .map_err(Into::into);
            responder.send(result).map_err(Error::from)
        }
        AccessRequest::SetConnectionPolicy { payload, responder } => {
            info!("fuchsia.bluetooth.sys.Access.SetConnectionPolicy(..)");
            let Some(stream) = payload.suppress_bredr_connections.map(|token| token.into_stream())
            else {
                return responder.send(Err(sys::Error::InvalidArguments)).map_err(Error::from);
            };
            let result = hd
                .set_connectable(/*connectable=*/ false)
                .await
                .map(|_| {
                    session.suppress_connections_sessions.push(stream.count());
                })
                .map_err(Into::into);
            responder.send(result).map_err(Error::from)
        }
        AccessRequest::StartDiscovery { token, responder } => {
            info!("fuchsia.bluetooth.sys.Access.StartDiscovery(..)");
            let stream = token.into_stream(); // into_stream never fails
            let result = hd.start_discovery().await.map(|discovery_session| {
                debug!("StartDiscovery: discovery started");
                let mut wait_for_discovery_end = discovery_session.on_discovery_end();
                let discovery_fut = async move {
                    // Wait for either the client to drop its ProcedureToken or the Host server to
                    // terminate discovery.
                    select! {
                      _ = watch_stream_for_session(stream, discovery_session).fuse() => {
                          debug!("StartDiscovery: watch_stream_for_session completed");
                      }
                      _ = wait_for_discovery_end => {
                          debug!("StartDiscovery: wait_for_discovery_end completed");
                      }
                    }
                };
                session.discovery_session = Some(discovery_fut.boxed());
            });
            responder.send(result.map_err(Into::into)).map_err(Error::from)
        }
        AccessRequest::WatchPeers { responder } => {
            trace!("Received FIDL call: fuchsia.bluetooth.sys.Access.WatchPeers()");
            watch_peers_subscriber
                .register(PeerWatcher::new(session.peers_seen.clone(), responder))
                .await
                .map_err(|e| {
                    // If we cannot register the observation, we return an error from the handler
                    // function. This terminates the stream and will drop the channel, as we are unable
                    // to fulfill our contract for WatchPeers(). The client can attempt to reconnect and
                    // if successful will receive a fresh session with initial state of the world
                    format_err!("Failed to watch peers: {:?}", e)
                })
        }
        AccessRequest::Connect { id, responder } => {
            let id = PeerId::from(id);
            info!("fuchsia.bluetooth.sys.Access.Connect({})", id);
            let result = hd.connect(id).await;
            if let Err(e) = &result {
                warn!("Error connecting to peer {}: {:?}", id, e);
            }
            responder.send(result.map_err(Into::into))?;
            Ok(())
        }
        AccessRequest::Disconnect { id, responder } => {
            let id = PeerId::from(id);
            info!("fuchsia.bluetooth.sys.Access.Disconnect({})", id);
            let result = hd.disconnect(id).await;
            if let Err(e) = &result {
                warn!("Error disconnecting from peer {}: {:?}", id, e);
            }
            responder.send(result.map_err(Into::into))?;
            Ok(())
        }
        AccessRequest::Pair { id, options, responder } => {
            let id = PeerId::from(id);
            info!("fuchsia.bluetooth.sys.Access.Pair({})", id);
            let opts: PairingOptions = options.into();
            // We currently do not support NonBondable mode on the classic Br/Edr transport
            // If NonBondable is asked for a Br/Edr pairing, return an InvalidArguments error
            if opts.bondable == BondableMode::NonBondable && opts.transport == Technology::Classic {
                info!("Rejecting Pair: non-bondable mode not supported for BR/EDR");
                responder.send(Err(sys::Error::InvalidArguments))?;
                return Ok(());
            }
            let result = hd.pair(id, opts).await;
            if let Err(e) = &result {
                warn!("Error pairing with peer {}: {:?}", id, e);
            }
            let result = result.map_err(|e| match e.into() {
                sys::Error::PeerNotFound => sys::Error::PeerNotFound,
                sys::Error::InvalidArguments => sys::Error::InvalidArguments,
                // We map all other host errors to Error::Failed before reporting to the caller
                _ => sys::Error::Failed,
            });
            responder.send(result)?;
            Ok(())
        }
        AccessRequest::Forget { id, responder } => {
            let id = PeerId::from(id);
            info!("fuchsia.bluetooth.sys.Access.Forget({})", id);
            let result = hd.forget(id).await;
            if let Err(e) = &result {
                warn!("Error forgetting peer {}: {:?}", id, e);
            }
            responder.send(result.map_err(Into::into))?;
            Ok(())
        }
    }
}

async fn watch_stream_for_session<S: Stream + Send + 'static, T: Send + 'static>(
    stream: S,
    token: T,
) {
    stream.map(|_| ()).collect::<()>().await;
    // the remote end closed; drop our session token
    mem::drop(token);
    trace!("ProcedureToken dropped");
}
