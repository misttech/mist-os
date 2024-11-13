// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, bail, Context, Result};
use ascendd::Ascendd;
use async_trait::async_trait;
use errors::ffx_error;
use ffx_build_version::build_info;
use ffx_config::EnvironmentContext;
use ffx_daemon_core::events::{self, EventHandler};
use ffx_daemon_events::{DaemonEvent, TargetConnectionState, TargetEvent, WireTrafficType};
use ffx_daemon_protocols::create_protocol_register_map;
use ffx_daemon_target::target::{self, Target, TargetProtocol, TargetTransport};
use ffx_daemon_target::target_collection::{TargetCollection, TargetUpdateFilter};
use ffx_daemon_target::zedboot::zedboot_discovery;
use ffx_metrics::{add_daemon_launch_event, add_daemon_metrics_event};
use ffx_stream_util::TryStreamUtilExt;
use ffx_target::Description;
use fidl::prelude::*;
use fidl_fuchsia_developer_ffx::{
    self as ffx, DaemonError, DaemonMarker, DaemonRequest, DaemonRequestStream,
    RepositoryRegistryMarker, TargetCollectionMarker, VersionInfo,
};
use fidl_fuchsia_developer_remotecontrol::{RemoteControlMarker, RemoteControlProxy};
use fidl_fuchsia_io::OpenFlags;
use fidl_fuchsia_overnet_protocol::NodeId;
use fidl_fuchsia_sys2 as fsys;
use fuchsia_async::{Task, TimeoutExt, Timer};
use futures::channel::{mpsc, oneshot};
use futures::executor::block_on;
use futures::prelude::*;
use manual_targets::{Config, ManualTargets};
use notify::{RecursiveMode, Watcher};
use overnet_core::ListablePeer;
use protocols::{DaemonProtocolProvider, ProtocolError, ProtocolRegister};
use rcs::RcsConnection;
use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
use std::cell::Cell;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

#[cfg(not(target_os = "macos"))]
use notify::RecommendedWatcher;

// `RecommendedWatcher` is a type alias to FsEvents in the notify crate.
// On mac this seems to have bugs about what's reported and when regarding
// file removal. Without PollWatcher the watcher would report a fresh file
// as having been deleted even if it is a new file.
//
// See https://fxbug.dev/42065810 for details on what happens when using the Default
// RecommendedWatcher (FsEvents).
//
// It's possible that in future versions of this crate this bug will be fixed,
// so it may be worth revisiting this in the future in order to make the code
// in this file a little cleaner and easier to read.
#[cfg(target_os = "macos")]
use notify::PollWatcher as RecommendedWatcher;

// Daemon

/// Determines if targets discovered should expire. Defaults to "true"
const DISCOVERY_EXPIRE_TARGETS: &str = "discovery.expire_targets";

/// Whether we should discover targets over USB. Defaults to "false"
const OVERNET_ENABLE_USB: &str = "overnet.enable_usb";

pub struct DaemonEventHandler {
    node: Arc<overnet_core::Router>,
    target_collection: Rc<TargetCollection>,
}

impl DaemonEventHandler {
    fn new(node: Arc<overnet_core::Router>, target_collection: Rc<TargetCollection>) -> Self {
        Self { node, target_collection }
    }

    #[tracing::instrument(skip(self))]
    async fn handle_overnet_peer(&self, node_id: u64) {
        tracing::debug!("Got overnet peer {node_id}");
        let rcs = match RcsConnection::new(Arc::clone(&self.node), &mut NodeId { id: node_id }) {
            Ok(rcs) => rcs,
            Err(e) => {
                tracing::error!(
                    "Target from Overnet {} failed to connect to RCS: {:?}",
                    node_id,
                    e
                );
                return;
            }
        };

        let identify = match rcs.identify_host().await {
            Ok(v) => v,
            Err(err) => {
                tracing::error!(
                    "Target from Overnet {} could not be identified: {:?}",
                    node_id,
                    err
                );
                return;
            }
        };

        let (update, addrs) =
            target::TargetUpdateBuilder::from_rcs_identify(rcs.clone(), &identify);

        let nodename = identify.nodename.clone().unwrap_or_default();
        let product_config = identify.product_config.clone().unwrap_or_default();
        let board_config = identify.board_config.clone().unwrap_or_default();
        let ids = identify.ids.clone().unwrap_or_default();

        match self.target_collection.update_target(
            &[
                TargetUpdateFilter::Ids(identify.ids.as_deref().unwrap_or(&[])),
                TargetUpdateFilter::NetAddrs(&addrs),
            ],
            update.build(),
            false,
        ) {
            // Print out better Peer information in logs
            0 => {
                tracing::error!(
                    "No targets match identity ['{nodename}', \
                    '{product_config}.{board_config}', {ids:?}] with node id: {node_id}"
                );
            }
            _ => {
                tracing::info!(
                    "Overnet peer identified as: ['{nodename}', \
                    '{product_config}.{board_config}', {ids:?}] with node id: {node_id}"
                );
            }
        }
    }

    #[tracing::instrument(skip(self))]
    async fn handle_overnet_peer_lost(&self, node_id: u64) {
        self.target_collection.update_target(
            &[TargetUpdateFilter::OvernetNodeId(node_id)],
            target::TargetUpdateBuilder::new().disconnected().build(),
            false,
        );
    }

    #[tracing::instrument(skip(self))]
    async fn handle_zedboot(&self, t: Description) {
        tracing::trace!(
            "Found new target via zedboot: {}",
            t.nodename.as_deref().unwrap_or("<unknown>")
        );

        let addrs = t.addresses.into_iter().map(|a| a.into()).collect::<Vec<_>>();

        let mut update = target::TargetUpdateBuilder::new()
            .net_addresses(&addrs)
            .discovered(TargetProtocol::Netsvc, TargetTransport::Network);

        if let Some(name) = t.nodename {
            update = update.identity(target::Identity::from_name(name));
        }

        self.target_collection.update_target(
            &[TargetUpdateFilter::NetAddrs(&addrs)],
            update.build(),
            true,
        );
    }
}

#[async_trait(?Send)]
impl DaemonProtocolProvider for Daemon {
    async fn open_protocol(&self, protocol_name: String) -> Result<fidl::Channel> {
        let (server, client) = fidl::Channel::create();
        self.protocol_register
            .open(
                protocol_name,
                protocols::Context::new(self.clone()),
                fidl::AsyncChannel::from_channel(server),
            )
            .await?;
        Ok(client)
    }

    fn overnet_node(&self) -> Result<Arc<overnet_core::Router>> {
        self.overnet_node.clone().ok_or_else(|| {
            anyhow!("Attempting to get overnet node for protocol when daemon is not started")
        })
    }

    async fn open_target_proxy(
        &self,
        target_identifier: Option<String>,
        moniker: &str,
        capability_name: &str,
    ) -> Result<fidl::Channel> {
        let (_, channel) =
            self.open_target_proxy_with_info(target_identifier, moniker, capability_name).await?;
        Ok(channel)
    }

    async fn get_target_event_queue(
        &self,
        target_identifier: Option<String>,
    ) -> Result<(Rc<Target>, events::Queue<TargetEvent>)> {
        let target = self
            .get_target(target_identifier)
            .await
            .map_err(|e| anyhow!("{:#?}", e))
            .context("getting default target")?;
        let events = target.events.clone();
        Ok((target, events))
    }

    async fn open_target_proxy_with_info(
        &self,
        target_identifier: Option<String>,
        moniker: &str,
        capability_name: &str,
    ) -> Result<(ffx::TargetInfo, fidl::Channel)> {
        let target = self.get_rcs_ready_target(target_identifier).await?;
        let rcs = target
            .rcs()
            .ok_or_else(|| anyhow!("rcs disconnected after event fired"))
            .context("getting rcs instance")?;
        let (server, client) = fidl::Channel::create();

        // TODO(awdavies): Handle these errors properly so the client knows what happened.
        rcs.proxy
            .open_capability(
                moniker,
                fsys::OpenDirType::ExposedDir,
                capability_name,
                server,
                OpenFlags::empty(),
            )
            .await
            .context("FIDL connection")?
            .map_err(|e| anyhow!("{:#?}", e))
            .context("proxy connect")?;

        tracing::debug!("Returning target and proxy for {}@{}", target.nodename_str(), target.id());
        Ok((target.as_ref().into(), client))
    }

    async fn get_target_info(
        &self,
        target_identifier: Option<String>,
    ) -> Result<ffx::TargetInfo, DaemonError> {
        let target = self
            .target_collection
            .query_single_enabled_target(&target_identifier.into())
            .map_err(|_| DaemonError::TargetAmbiguous)?
            .ok_or(DaemonError::TargetNotFound)?;
        Ok(target.as_ref().into())
    }

    #[tracing::instrument(level = "info", skip(self))]
    async fn open_remote_control(
        &self,
        target_identifier: Option<String>,
    ) -> Result<RemoteControlProxy> {
        let target = self.get_rcs_ready_target(target_identifier).await?;
        // Ensure auto-connect has at least started.
        let mut rcs = target
            .rcs()
            .ok_or_else(|| anyhow!("rcs disconnected after event fired"))
            .context("getting rcs instance")?;
        let (proxy, remote) = fidl::endpoints::create_proxy::<RemoteControlMarker>()?;
        rcs.copy_to_channel(remote.into_channel())?;
        Ok(proxy)
    }

    async fn daemon_event_queue(&self) -> events::Queue<DaemonEvent> {
        self.event_queue.clone()
    }

    async fn get_target_collection(&self) -> Result<Rc<TargetCollection>> {
        Ok(self.target_collection.clone())
    }
}

#[async_trait(?Send)]
impl EventHandler<DaemonEvent> for DaemonEventHandler {
    #[tracing::instrument(skip(self))]
    async fn on_event(&self, event: DaemonEvent) -> Result<events::Status> {
        tracing::debug!("! DaemonEvent::{:?}", event);

        match event {
            DaemonEvent::WireTraffic(traffic) => match traffic {
                WireTrafficType::Zedboot(t) => {
                    self.handle_zedboot(t).await;
                }
            },
            DaemonEvent::OvernetPeer(node_id) => {
                self.handle_overnet_peer(node_id).await;
            }
            DaemonEvent::OvernetPeerLost(node_id) => {
                self.handle_overnet_peer_lost(node_id).await;
            }
            _ => (),
        }

        // This handler is never done unless the target_collection is dropped.
        Ok(events::Status::Waiting)
    }
}

#[derive(Clone)]
/// Defines the daemon object. This is used by "ffx daemon start".
///
/// Typical usage is:
///   let mut daemon = ffx_daemon::Daemon::new(socket_path);
///   daemon.start().await
pub struct Daemon {
    // The path to the ascendd socket this daemon will bind to
    socket_path: PathBuf,
    // The event queue is a collection of subscriptions to which DaemonEvents will be published.
    event_queue: events::Queue<DaemonEvent>,
    // All the targets currently known to the daemon.
    // This may include targets the daemon has no access to.
    target_collection: Rc<TargetCollection>,
    // ascendd is the overnet daemon running on the Linux host. It manages the mesh and the
    // connections to the devices and other peers (for example, a connection to the frontend).
    // With ffx, ascendd is embedded within the ffx daemon (when ffx daemon is launched, we don’t
    // need an extra process for ascendd).
    ascendd: Rc<Cell<Option<Ascendd>>>,
    // Handles the registered FIDL protocols and associated handles. This is initialized with the
    // list of protocols defined in src/developer/ffx/daemon/protocols/BUILD.gn (the deps field in
    // ffx_protocol) using the macro generate_protocol_map in
    // src/developer/ffx/build/templates/protocols_macro.rs.jinja.
    protocol_register: ProtocolRegister,
    // All the persistent long running tasks spawned by the daemon. The tasks are standalone. That
    // means that they execute by themselves without any intervention from the daemon.
    // The purpose of this vector is to keep the reference strong count positive until the daemon is
    // dropped.
    tasks: Vec<Rc<Task<()>>>,
    // This daemon's node on the Overnet mesh.
    overnet_node: Option<Arc<overnet_core::Router>>,
}

impl Daemon {
    #[tracing::instrument]
    pub fn new(socket_path: PathBuf) -> Daemon {
        tracing::debug!("About to create Daemon, starting with Target Collection");
        let target_collection = TargetCollection::new();
        tracing::debug!("Wrapping TC in Rc");
        let target_collection = Rc::new(target_collection);
        tracing::debug!("Creating new event queue");
        let event_queue = events::Queue::new(&target_collection);
        tracing::debug!("Attaching event queue to TC");
        target_collection.set_event_queue(event_queue.clone());

        tracing::debug!("Creating Daemon structure");
        Self {
            socket_path,
            target_collection,
            event_queue,
            protocol_register: ProtocolRegister::new(create_protocol_register_map()),
            ascendd: Rc::new(Cell::new(None)),
            tasks: Vec::new(),
            overnet_node: None,
        }
    }

    #[tracing::instrument(skip(self, node))]
    pub async fn start(&mut self, node: Arc<overnet_core::Router>) -> Result<()> {
        tracing::debug!("starting daemon");
        self.overnet_node = Some(Arc::clone(&node));
        let context =
            ffx_config::global_env_context().context("Discovering ffx environment context")?;

        let ascendd = self.prime_ascendd(Arc::clone(&node)).await?;

        let (quit_tx, quit_rx) = mpsc::channel(1);
        self.log_startup_info(&context).await.context("Logging startup info")?;

        self.start_protocols().await?;
        self.start_discovery(Arc::clone(&node)).await?;
        self.start_ascendd(ascendd);
        let _socket_file_watcher =
            self.start_socket_watch(quit_tx.clone()).await.context("Starting socket watcher")?;
        self.start_signal_monitoring(quit_tx.clone());
        let should_start_expiry = context.get(DISCOVERY_EXPIRE_TARGETS).unwrap_or(true);
        if should_start_expiry == true {
            self.start_target_expiry(Duration::from_secs(1), Arc::clone(&node));
        }
        self.serve(&context, node, quit_tx, quit_rx).await.context("Serving clients")
    }

    #[tracing::instrument(skip(self))]
    async fn log_startup_info(&self, context: &EnvironmentContext) -> Result<()> {
        let pid = std::process::id();
        let buildid = context.daemon_version_string()?;
        let version_info = build_info();
        let commit_hash = version_info.commit_hash.as_deref().unwrap_or("<unknown>");
        let commit_timestamp = version_info
            .commit_timestamp
            .map(|t| t.to_string())
            .unwrap_or_else(|| "<unknown>".to_owned());
        let build_version = version_info.build_version.as_deref().unwrap_or("<unknown>");

        tracing::info!(
            "Beginning daemon startup\nBuild Version: {build_version}\nCommit Timestamp: {commit_timestamp}\nCommit Hash: {commit_hash}\nBinary Build ID: {buildid}\nPID: {pid}",
        );
        add_daemon_launch_event().await;
        Ok(())
    }

    async fn start_protocols(&mut self) -> Result<()> {
        let cx = protocols::Context::new(self.clone());
        let ((), ()) = futures::future::try_join(
            self.protocol_register
                .start(RepositoryRegistryMarker::PROTOCOL_NAME.to_string(), cx.clone()),
            self.protocol_register.start(TargetCollectionMarker::PROTOCOL_NAME.to_string(), cx),
        )
        .await?;
        Ok(())
    }

    /// Awaits a target that has RCS active.
    async fn get_rcs_ready_target(&self, target_query: Option<String>) -> Result<Rc<Target>> {
        let target = self
            .get_target(target_query)
            .await
            .map_err(|e| anyhow!("{:#?}", e))
            .context("getting default target")?;
        if matches!(target.get_connection_state(), TargetConnectionState::Fastboot(_)) {
            let nodename = target.nodename().unwrap_or_else(|| "<No Nodename>".to_string());
            bail!("Attempting to open RCS on a fastboot target: {}", nodename);
        }
        if matches!(target.get_connection_state(), TargetConnectionState::Zedboot(_)) {
            let nodename = target.nodename().unwrap_or_else(|| "<No Nodename>".to_string());
            bail!("Attempting to connect to RCS on a zedboot target: {}", nodename);
        }
        let Some(overnet_node) = self.overnet_node.as_ref() else {
            bail!("Attempting to connect to RCS when daemon is not started");
        };
        // Ensure auto-connect has at least started.
        target.run_host_pipe(overnet_node);
        target
            .events
            .wait_for(None, |e| e == TargetEvent::RcsActivated)
            .await
            .context("waiting for RCS activation")?;
        tracing::debug!("RCS activated for {}@{}", target.nodename_str(), target.id());
        Ok(target)
    }

    /// Start all discovery tasks
    async fn start_discovery(&mut self, node: Arc<overnet_core::Router>) -> Result<()> {
        let daemon_event_handler =
            DaemonEventHandler::new(Arc::clone(&node), self.target_collection.clone());
        self.event_queue.add_handler(daemon_event_handler).await;

        // TODO: these tasks could and probably should be managed by the daemon
        // instead of being detached.
        Daemon::spawn_onet_discovery(node, self.event_queue.clone());
        let discovery = zedboot_discovery(self.event_queue.clone()).await?;
        self.tasks.push(Rc::new(discovery));
        Ok(())
    }

    #[tracing::instrument(skip(self))]
    async fn prime_ascendd(
        &self,
        node: Arc<overnet_core::Router>,
    ) -> Result<impl FnOnce() -> Ascendd, errors::FfxError> {
        // Bind the ascendd socket but delay accepting connections until protocols are registered.
        tracing::debug!("Priming ascendd");

        let client_routing = false; // Don't route between ffx clients
        Ascendd::prime(
            ascendd::Opt {
                sockpath: self.socket_path.clone(),
                client_routing,
                usb: ffx_config::get(OVERNET_ENABLE_USB).unwrap_or(false),
                ..Default::default()
            },
            node,
        )
        .await
        .map_err(|e| ffx_error!("Error trying to start daemon socket: {e}"))
    }

    #[tracing::instrument(skip(self, primed_ascendd))]
    fn start_ascendd(&mut self, primed_ascendd: impl FnOnce() -> Ascendd) {
        // Start the ascendd socket only after we have registered our protocols.
        tracing::debug!("Starting ascendd");

        let ascendd = primed_ascendd();

        self.ascendd.replace(Some(ascendd));
    }

    #[tracing::instrument(skip(self))]
    async fn start_socket_watch(&self, quit_tx: mpsc::Sender<()>) -> Result<RecommendedWatcher> {
        let socket_path = self.socket_path.clone();
        let socket_dir = self.socket_path.parent().context("Getting parent directory of socket")?;
        let event_handler = move |res| {
            let mut quit_tx = quit_tx.clone();
            block_on(async {
                use notify::event::Event;
                use notify::event::EventKind::Remove;
                match res {
                    Ok(Event { kind: Remove(_), paths, .. }) if paths.contains(&socket_path) => {
                        tracing::info!("daemon socket was deleted, triggering quit message.");
                        quit_tx.send(()).await.ok();
                    }
                    Err(ref e @ notify::Error { ref kind, .. }) => {
                        match kind {
                            notify::ErrorKind::Io(ioe) => {
                                tracing::debug!("IO error. Ignoring {ioe:?}");
                            }
                            _ => {
                                // If we get a non-spurious error, treat that as something that
                                // should cause us to exit.
                                tracing::warn!("exiting due to file watcher error: {e:?}");
                                quit_tx.send(()).await.ok();
                            }
                        }
                    }
                    Ok(_) => {} // just ignore any non-delete event or for any other file.
                }
            })
        };

        #[cfg(target_os = "macos")]
        let res = RecommendedWatcher::new(
            event_handler,
            notify::Config::default().with_poll_interval(Duration::from_millis(500)),
        );
        #[cfg(not(target_os = "macos"))]
        let res = RecommendedWatcher::new(event_handler, notify::Config::default());

        let mut watcher = res.context("Creating watcher")?;

        // we have to watch the directory because watching a file does weird things and only
        // half works. This seems to be a limitation of underlying libraries.
        watcher
            .watch(&socket_dir, RecursiveMode::NonRecursive)
            .context("Setting watcher context")?;

        tracing::debug!(
            "Watching daemon socket file at {socket_path}, will gracefully exit if it's removed.",
            socket_path = self.socket_path.display()
        );

        Ok(watcher)
    }

    #[tracing::instrument(skip(self, quit_tx))]
    fn start_signal_monitoring(&self, mut quit_tx: mpsc::Sender<()>) {
        tracing::debug!("Starting monitoring for SIGHUP, SIGINT, SIGTERM");
        let mut signals = Signals::new(&[SIGHUP, SIGINT, SIGTERM]).unwrap();
        // signals.forever() is blocking, so we need to spawn a thread rather than use async.
        let _signal_handle_thread = std::thread::spawn(move || {
            if let Some(signal) = signals.forever().next() {
                match signal {
                    SIGHUP | SIGINT | SIGTERM => {
                        tracing::info!("Received signal {signal}, quitting");
                        let _ = block_on(quit_tx.send(())).ok();
                    }
                    _ => unreachable!(),
                }
            }
        });
    }

    fn start_target_expiry(
        &mut self,
        frequency: Duration,
        overnet_node: Arc<overnet_core::Router>,
    ) {
        let target_collection = Rc::downgrade(&self.target_collection);
        self.tasks.push(Rc::new(Task::local(async move {
            loop {
                Timer::new(frequency).await;
                let manual_targets = Config::default();

                match target_collection.upgrade() {
                    Some(target_collection) => {
                        let expired_addrs = target_collection.expire_targets(&overnet_node);
                        for addr in expired_addrs {
                            // If a manual target has been allowed to transition to the
                            // "disconnected" state, it should be removed from the collection.
                            match manual_targets.remove(format!("{}", addr)).await {
                                Ok(_) => {}
                                Err(e) => {
                                    tracing::error!(
                                        "Unable to persist ephemeral target removal: {}",
                                        e
                                    );
                                }
                            }
                        }
                    }
                    None => return,
                }
            }
        })))
    }

    /// get_target attempts to get the target that matches the match string if
    /// provided, otherwise the default target from the target collection.
    async fn get_target(&self, matcher: Option<String>) -> Result<Rc<Target>, DaemonError> {
        // TODO(72818): make target match timeout configurable / paramterable
        #[cfg(not(test))]
        const GET_TARGET_TIMEOUT: Duration = Duration::from_secs(8);
        #[cfg(test)]
        const GET_TARGET_TIMEOUT: Duration = Duration::from_secs(1);

        let query = matcher.into();
        let target_collection = &self.target_collection;

        // Get a previously used target first, otherwise fall back to discovery + open.
        match target_collection.query_single_enabled_target(&query) {
            Ok(Some(target)) => Ok(target),
            Ok(None) => {
                target_collection
                    // OpenTarget is called on behalf of the user, as ListTargets will (soon) not
                    // surface discovered targets by default.
                    .discover_target(&query)
                    .map_err(|_| DaemonError::TargetAmbiguous)
                    .on_timeout(GET_TARGET_TIMEOUT, || match self.target_collection.is_empty() {
                        true => Err(DaemonError::TargetCacheEmpty),
                        false => Err(DaemonError::TargetNotFound),
                    })
                    .await
                    .map(|t| target_collection.use_target(t, "OpenTarget request"))
            }
            Err(()) => Err(DaemonError::TargetAmbiguous),
        }
    }

    #[tracing::instrument(skip(self, quit_tx, stream))]
    async fn handle_requests_from_stream(
        &self,
        quit_tx: &mpsc::Sender<()>,
        stream: DaemonRequestStream,
        info: &VersionInfo,
    ) -> Result<()> {
        stream
            .map_err(|e| anyhow!("reading FIDL stream: {:#}", e))
            .try_for_each_concurrent_while_connected(None, |r| async {
                let debug_req_string = format!("{:?}", r);
                if let Err(e) = self.handle_request(quit_tx, r, info).await {
                    tracing::error!("error while handling request `{}`: {}", debug_req_string, e);
                }
                Ok(())
            })
            .await
    }

    fn spawn_onet_discovery(node: Arc<overnet_core::Router>, queue: events::Queue<DaemonEvent>) {
        fuchsia_async::Task::local(async move {
            let mut known_peers: HashSet<PeerSetElement> = Default::default();

            loop {
                let lpc = node.new_list_peers_context().await;
                loop {
                    match lpc.list_peers().await {
                        Ok(new_peers) => {
                            known_peers =
                                Self::handle_overnet_peers(&queue, known_peers, new_peers);
                        }
                        Err(err) => {
                            tracing::info!("Overnet peer discovery failed: {}, will retry", err);
                            Timer::new(Duration::from_secs(1)).await;
                            // break out of the peer discovery loop on error in
                            // order to reconnect, in case the error causes the
                            // overnet interface to go bad.
                            break;
                        }
                    };
                }
            }
        })
        .detach();
    }

    #[tracing::instrument(skip(queue))]
    fn handle_overnet_peers(
        queue: &events::Queue<DaemonEvent>,
        known_peers: HashSet<PeerSetElement>,
        peers: Vec<ListablePeer>,
    ) -> HashSet<PeerSetElement> {
        tracing::debug!("Got updated peer list {peers:?}");
        let mut new_peers: HashSet<PeerSetElement> = Default::default();
        for peer in peers {
            new_peers.insert(PeerSetElement(peer));
        }

        for peer in new_peers.difference(&known_peers) {
            let peer = &peer.0;
            let peer_has_rcs =
                peer.services.contains(&RemoteControlMarker::PROTOCOL_NAME.to_string());
            if peer_has_rcs {
                queue.push(DaemonEvent::OvernetPeer(peer.node_id.0)).unwrap_or_else(|err| {
                    tracing::warn!(
                        "Overnet discovery failed to enqueue event {:?}: {}",
                        DaemonEvent::OvernetPeer(peer.node_id.0),
                        err
                    );
                });
            }
        }

        for peer in known_peers.difference(&new_peers) {
            let peer = &peer.0;
            queue.push(DaemonEvent::OvernetPeerLost(peer.node_id.0)).unwrap_or_else(|err| {
                tracing::warn!(
                    "Overnet discovery failed to enqueue event {:?}: {}",
                    DaemonEvent::OvernetPeerLost(peer.node_id.0),
                    err
                );
            });
        }

        new_peers
    }

    #[tracing::instrument(skip(self))]
    async fn handle_request(
        &self,
        quit_tx: &mpsc::Sender<()>,
        req: DaemonRequest,
        info: &VersionInfo,
    ) -> Result<()> {
        tracing::debug!("daemon received request: {:?}", req);

        match req {
            DaemonRequest::Quit { responder } => {
                tracing::info!("Received quit request.");
                if cfg!(test) {
                    panic!("quit() should not be invoked in test code");
                }

                quit_tx.clone().send(()).await?;

                responder.send(true).context("error sending response")?;
            }
            DaemonRequest::GetVersionInfo { responder } => {
                return responder.send(info).context("sending GetVersionInfo response");
            }
            DaemonRequest::ConnectToProtocol { name, server_end, responder } => {
                let name_for_analytics = name.clone();
                match self
                    .protocol_register
                    .open(
                        name,
                        protocols::Context::new(self.clone()),
                        fidl::AsyncChannel::from_channel(server_end),
                    )
                    .await
                {
                    Ok(()) => responder.send(Ok(())).context("fidl response")?,
                    Err(e) => {
                        tracing::error!("{}", e);
                        match e {
                            ProtocolError::NoProtocolFound(_) => {
                                responder.send(Err(DaemonError::ProtocolNotFound))?
                            }
                            ProtocolError::StreamOpenError(_) => {
                                responder.send(Err(DaemonError::ProtocolOpenError))?
                            }
                            ProtocolError::BadRegisterState(_)
                            | ProtocolError::DuplicateTaskId(..) => {
                                responder.send(Err(DaemonError::BadProtocolRegisterState))?
                            }
                        }
                    }
                }
                add_daemon_metrics_event(
                    format!("connect_to_protocol: {}", &name_for_analytics).as_str(),
                )
                .await;
            }
            DaemonRequest::_UnknownMethod { ordinal, method_type, .. } => {
                tracing::warn!(ordinal, method_type = ?method_type, "Received unknown method request");
            }
        }

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    async fn serve(
        &self,
        context: &EnvironmentContext,
        node: Arc<overnet_core::Router>,
        quit_tx: mpsc::Sender<()>,
        mut quit_rx: mpsc::Receiver<()>,
    ) -> Result<()> {
        let (sender, mut stream) = futures::channel::mpsc::unbounded();

        let mut info = build_info();
        info.build_id = Some(context.daemon_version_string()?);
        tracing::debug!("Starting daemon overnet server");
        node.register_service(DaemonMarker::PROTOCOL_NAME.to_owned(), move |chan| {
            let _ = sender.unbounded_send(chan);
            Ok(())
        })
        .await?;

        tracing::debug!("Starting daemon serve loop");
        let (break_loop_tx, mut break_loop_rx) = oneshot::channel();
        let mut break_loop_tx = Some(break_loop_tx);

        loop {
            futures::select! {
                req = stream.next() => match req {
                    Some(chan) => {
                        tracing::trace!("Received protocol request for protocol");
                        let chan =
                            fidl::AsyncChannel::from_channel(chan);
                        let daemon_clone = self.clone();
                        let mut quit_tx = quit_tx.clone();
                        let version_info = info.clone();
                        Task::local(async move {
                            let ffx_version_info = VersionInfo {
                                commit_hash: version_info.commit_hash,
                                commit_timestamp: version_info.commit_timestamp,
                                build_version: version_info.build_version,
                                abi_revision: version_info.abi_revision,
                                api_level: version_info.api_level,
                                exec_path: version_info.exec_path,
                                build_id: version_info.build_id,
                                ..Default::default()
                            };
                            if let Err(err) = daemon_clone.handle_requests_from_stream(&quit_tx, DaemonRequestStream::from_channel(chan), &ffx_version_info).await {
                                tracing::error!("error handling request: {:?}", err);
                                quit_tx.send(()).await.expect("Failed to gracefully send quit message, aborting.");
                            }
                        })
                        .detach();
                    },
                    None => {
                        tracing::warn!("Service was deregistered");
                        break;
                    }
                },
                _ = quit_rx.next() => {
                    if let Some(break_loop_tx) = break_loop_tx.take() {
                        tracing::debug!("Starting graceful shutdown of daemon socket");

                        match std::fs::remove_file(self.socket_path.clone()) {
                            Ok(()) => {}
                            Err(e) => tracing::error!("failed to remove socket file: {}", e),
                        }

                        self.protocol_register
                            .shutdown(protocols::Context::new(self.clone()))
                            .await
                            .unwrap_or_else(|e| {
                                tracing::error!("shutting down protocol register: {:?}", e)
                            });

                        add_daemon_metrics_event("quit").await;

                        // It is desirable for the client to receive an ACK for the quit
                        // request. As Overnet has a potentially complicated routing
                        // path, it is tricky to implement some notion of a bounded
                        // "flush" for this response, however in practice it is only
                        // necessary here to wait long enough for the message to likely
                        // leave the local process before exiting. Enqueue a detached
                        // timer to shut down the daemon before sending the response.
                        // This is detached because once the client receives the
                        // response, the client will disconnect it's socket. If the
                        // local reactor observes this disconnection before the timer
                        // expires, an in-line timer wait would never fire, and the
                        // daemon would never exit.
                        Task::local(async move {
                            Timer::new(std::time::Duration::from_millis(20)).await;
                            break_loop_tx.send(()).expect("failed to send loop break message");
                        })
                        .detach();
                    } else {
                        tracing::trace!("Received quit message after shutdown was already initiated");
                    }
                },
                _ = break_loop_rx => {
                    tracing::debug!("Breaking main daemon socket loop");
                    break;
                }
            }
        }
        tracing::debug!("Graceful shutdown of daemon loop completed");
        ffx_config::logging::disable_stdio_logging();
        Ok(())
    }
}

// PeerSetElement wraps an overnet Peer object for inclusion in a Set
// or other collection reliant on Eq and HAsh, using the NodeId as the
// discriminator.
#[derive(Debug)]
struct PeerSetElement(ListablePeer);
impl PartialEq for PeerSetElement {
    fn eq(&self, other: &Self) -> bool {
        self.0.node_id == other.0.node_id
    }
}
impl Eq for PeerSetElement {}
impl Hash for PeerSetElement {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.node_id.hash(state);
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use addr::TargetAddr;
    use assert_matches::assert_matches;
    use chrono::Utc;
    use ffx_daemon_target::target::{TargetAddrEntry, TargetAddrStatus};
    use fidl_fuchsia_developer_ffx::DaemonProxy;
    use std::cell::RefCell;
    use std::collections::BTreeSet;
    use std::str::FromStr;
    use std::time::{Instant, SystemTime};

    fn spawn_test_daemon() -> (DaemonProxy, Daemon, Task<Result<()>>) {
        let tempdir = tempfile::tempdir().expect("Creating tempdir");
        let socket_path = tempdir.path().join("ascendd.sock");
        let d = Daemon::new(socket_path);

        let (proxy, stream) = fidl::endpoints::create_proxy_and_stream::<DaemonMarker>().unwrap();
        let (quit_tx, _quit_rx) = mpsc::channel(1);

        let d2 = d.clone();
        let task = Task::local(async move {
            let version_info = build_info();
            let ffx_version_info = VersionInfo {
                commit_hash: version_info.commit_hash,
                commit_timestamp: version_info.commit_timestamp,
                build_version: version_info.build_version,
                abi_revision: version_info.abi_revision,
                api_level: version_info.api_level,
                exec_path: version_info.exec_path,
                build_id: version_info.build_id,
                ..Default::default()
            };

            d2.handle_requests_from_stream(&quit_tx, stream, &ffx_version_info).await
        });

        (proxy, d, task)
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_open_rcs_on_fastboot_error() {
        let (_proxy, daemon, _task) = spawn_test_daemon();
        let target = Target::new_for_usb("abc");
        daemon.target_collection.merge_insert(target);
        let result = daemon.open_remote_control(None).await;
        assert!(result.is_err());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_open_rcs_on_zedboot_error() {
        let (_proxy, daemon, _task) = spawn_test_daemon();
        let target = Target::new_with_netsvc_addrs(
            Some("abc"),
            BTreeSet::from_iter(vec![TargetAddr::from_str("[fe80::1%1]:22").unwrap()].into_iter()),
        );
        daemon.target_collection.merge_insert(target);
        let result = daemon.open_remote_control(None).await;
        assert!(result.is_err());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_get_target_empty() {
        let tempdir = tempfile::tempdir().expect("Creating tempdir");
        let socket_path = tempdir.path().join("ascendd.sock");
        let d = Daemon::new(socket_path);
        let nodename = "where-is-my-hasenpfeffer";
        let t = Target::new_autoconnected(nodename);
        d.target_collection.merge_insert(t.clone());
        assert_eq!(nodename, d.get_target(None).await.unwrap().nodename().unwrap());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_get_target_query() {
        let tempdir = tempfile::tempdir().expect("Creating tempdir");
        let socket_path = tempdir.path().join("ascendd.sock");
        let d = Daemon::new(socket_path);
        let nodename = "where-is-my-hasenpfeffer";
        let t = Target::new_autoconnected(nodename);
        d.target_collection.merge_insert(t.clone());
        assert_eq!(
            nodename,
            d.get_target(Some(nodename.to_string())).await.unwrap().nodename().unwrap()
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_get_target_collection_empty_error() {
        let tempdir = tempfile::tempdir().expect("Creating tempdir");
        let socket_path = tempdir.path().join("ascendd.sock");
        let d = Daemon::new(socket_path);
        assert_eq!(DaemonError::TargetCacheEmpty, d.get_target(None).await.unwrap_err());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_get_target_ambiguous() {
        let tempdir = tempfile::tempdir().expect("Creating tempdir");
        let socket_path = tempdir.path().join("ascendd.sock");
        let d = Daemon::new(socket_path);
        let t = Target::new_autoconnected("where-is-my-hasenpfeffer");
        let t2 = Target::new_autoconnected("it-is-rabbit-season");
        d.target_collection.merge_insert(t.clone());
        d.target_collection.merge_insert(t2.clone());
        assert_eq!(DaemonError::TargetAmbiguous, d.get_target(None).await.unwrap_err());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_expiry() {
        let local_node = overnet_core::Router::new(None).unwrap();
        let tempdir = tempfile::tempdir().expect("Creating tempdir");
        let socket_path = tempdir.path().join("ascendd.sock");
        let mut daemon = Daemon::new(socket_path);
        let target = Target::new_named("goodbye-world");
        let then = Instant::now() - Duration::from_secs(10);
        target.update_connection_state(|_| TargetConnectionState::Mdns(then));
        daemon.target_collection.merge_insert(target.clone());

        assert_eq!(TargetConnectionState::Mdns(then), target.get_connection_state());

        daemon.start_target_expiry(Duration::from_millis(1), local_node);

        while target.get_connection_state() == TargetConnectionState::Mdns(then) {
            futures_lite::future::yield_now().await
        }

        assert_eq!(TargetConnectionState::Disconnected, target.get_connection_state());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_ephemeral_target_expiry() {
        let local_node = overnet_core::Router::new(None).unwrap();
        let tempdir = tempfile::tempdir().expect("Creating tempdir");
        let socket_path = tempdir.path().join("ascendd.sock");
        let mut daemon = Daemon::new(socket_path);
        let expiring_target = Target::new_with_addr_entries(
            Some("goodbye-world"),
            vec![TargetAddrEntry::new(
                TargetAddr::from_str("127.0.0.1:8088").unwrap(),
                Utc::now(),
                TargetAddrStatus::ssh().manually_added_until(SystemTime::now()),
            )]
            .into_iter(),
        );
        expiring_target.set_ssh_port(Some(8022));

        let persistent_target = Target::new_with_addr_entries(
            Some("i-will-stick-around"),
            vec![TargetAddrEntry::new(
                TargetAddr::from_str("127.0.0.1:8089").unwrap(),
                Utc::now(),
                TargetAddrStatus::ssh().manually_added(),
            )]
            .into_iter(),
        );
        persistent_target.set_ssh_port(Some(8023));

        let then = Instant::now() - Duration::from_secs(10);
        expiring_target.update_connection_state(|_| TargetConnectionState::Mdns(then));
        persistent_target.update_connection_state(|_| TargetConnectionState::Mdns(then));

        assert!(daemon.target_collection.is_empty());

        daemon.target_collection.merge_insert(expiring_target.clone());
        daemon.target_collection.merge_insert(persistent_target.clone());

        assert_eq!(TargetConnectionState::Mdns(then), expiring_target.get_connection_state());
        assert_eq!(TargetConnectionState::Mdns(then), persistent_target.get_connection_state());

        daemon.start_target_expiry(Duration::from_millis(1), local_node);

        while expiring_target.get_connection_state() == TargetConnectionState::Mdns(then) {
            futures_lite::future::yield_now().await
        }
        while persistent_target.get_connection_state() == TargetConnectionState::Mdns(then) {
            futures_lite::future::yield_now().await
        }
        assert_eq!(TargetConnectionState::Disconnected, expiring_target.get_connection_state());
        assert_matches!(
            persistent_target.get_connection_state(),
            TargetConnectionState::Manual(None)
        );
        let not_found = daemon
            .target_collection
            .query_single_enabled_target(&"goodbye-world".into())
            .expect("Query should not be ambiguous");
        assert!(not_found.is_none(), "Should've expired: {not_found:?}");
    }

    struct NullDaemonEventSynthesizer();

    #[async_trait(?Send)]
    impl events::EventSynthesizer<DaemonEvent> for NullDaemonEventSynthesizer {
        async fn synthesize_events(&self) -> Vec<DaemonEvent> {
            return Default::default();
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_handle_overnet_peers_known_peer_exclusion() {
        let queue = events::Queue::<DaemonEvent>::new(&Rc::new(NullDaemonEventSynthesizer {}));
        let mut known_peers: HashSet<PeerSetElement> = Default::default();

        let peer1 = ListablePeer { node_id: 1.into(), is_self: false, services: Vec::new() };
        let peer2 = ListablePeer { node_id: 2.into(), is_self: false, services: Vec::new() };

        let new_peers =
            Daemon::handle_overnet_peers(&queue, known_peers, vec![peer1.clone(), peer2.clone()]);
        assert!(new_peers.contains(&PeerSetElement(peer1.clone())));
        assert!(new_peers.contains(&PeerSetElement(peer2.clone())));

        known_peers = new_peers;

        let new_peers = Daemon::handle_overnet_peers(&queue, known_peers, vec![]);
        assert!(!new_peers.contains(&PeerSetElement(peer1.clone())));
        assert!(!new_peers.contains(&PeerSetElement(peer2.clone())));
    }

    struct DaemonEventRecorder {
        /// All events observed by the handler will be logged into this field.
        event_log: Rc<RefCell<Vec<DaemonEvent>>>,
    }
    #[async_trait(?Send)]
    impl EventHandler<DaemonEvent> for DaemonEventRecorder {
        async fn on_event(&self, event: DaemonEvent) -> Result<events::Status> {
            self.event_log.borrow_mut().push(event);
            Ok(events::Status::Waiting)
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_handle_overnet_peer_leave_and_return() {
        let queue = events::Queue::<DaemonEvent>::new(&Rc::new(NullDaemonEventSynthesizer {}));
        let mut known_peers: HashSet<PeerSetElement> = Default::default();

        let peer1 = ListablePeer {
            node_id: 1.into(),
            is_self: false,
            services: vec![RemoteControlMarker::PROTOCOL_NAME.to_string()],
        };
        let peer2 = ListablePeer {
            node_id: 2.into(),
            is_self: false,
            services: vec![RemoteControlMarker::PROTOCOL_NAME.to_string()],
        };

        // First the targets are discovered:
        let new_peers =
            Daemon::handle_overnet_peers(&queue, known_peers, vec![peer1.clone(), peer2.clone()]);
        assert!(new_peers.contains(&PeerSetElement(peer1.clone())));
        assert!(new_peers.contains(&PeerSetElement(peer2.clone())));

        known_peers = new_peers;

        // Make a new queue so we don't get any of the historical events.
        let queue = events::Queue::<DaemonEvent>::new(&Rc::new(NullDaemonEventSynthesizer {}));
        let event_log = Rc::new(RefCell::new(Vec::<DaemonEvent>::new()));

        // Now wire up the event handler, we want to assert that we observe OvernetPeerLost events for the leaving targets.
        queue.add_handler(DaemonEventRecorder { event_log: event_log.clone() }).await;

        // Next the targets are lost:
        let new_peers = Daemon::handle_overnet_peers(&queue, known_peers, vec![]);
        assert!(!new_peers.contains(&PeerSetElement(peer1.clone())));
        assert!(!new_peers.contains(&PeerSetElement(peer2.clone())));

        let start = Instant::now();
        while event_log.borrow().len() != 2 {
            if Instant::now().duration_since(start) > Duration::from_secs(1) {
                break;
            }
            futures_lite::future::yield_now().await;
        }

        assert_eq!(event_log.borrow().len(), 2);
        assert_matches!(event_log.borrow()[0], DaemonEvent::OvernetPeerLost(_));
        assert_matches!(event_log.borrow()[1], DaemonEvent::OvernetPeerLost(_));

        known_peers = new_peers;

        assert_eq!(known_peers.len(), 0);

        // Make a new queue so we don't get any of the historical events.
        let queue = events::Queue::<DaemonEvent>::new(&Rc::new(NullDaemonEventSynthesizer {}));
        let event_log = Rc::new(RefCell::new(Vec::<DaemonEvent>::new()));

        // Now wire up the event handler, we want to assert that we observe NewTarget events for the returning targets.
        queue.add_handler(DaemonEventRecorder { event_log: event_log.clone() }).await;

        // Now the targets return:
        let new_peers =
            Daemon::handle_overnet_peers(&queue, known_peers, vec![peer1.clone(), peer2.clone()]);
        assert!(new_peers.contains(&PeerSetElement(peer1.clone())));
        assert!(new_peers.contains(&PeerSetElement(peer2.clone())));

        let start = Instant::now();
        while event_log.borrow().len() != 2 {
            if Instant::now().duration_since(start) > Duration::from_secs(1) {
                break;
            }
            futures_lite::future::yield_now().await;
        }

        // Ensure that we observed a new target event for each target that returned.
        assert_eq!(event_log.borrow().len(), 2);
        assert_matches!(event_log.borrow()[0], DaemonEvent::OvernetPeer(_));
        assert_matches!(event_log.borrow()[1], DaemonEvent::OvernetPeer(_));
    }
}
