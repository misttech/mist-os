// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::host_identifier::HostIdentifier,
    anyhow::{Context as _, Result},
    component_debug::dirs::*,
    component_debug::lifecycle::*,
    fidl::endpoints::ServerEnd,
    fidl::prelude::*,
    fidl_fuchsia_developer_remotecontrol as rcs,
    fidl_fuchsia_diagnostics::Selector,
    fidl_fuchsia_io as io,
    fidl_fuchsia_net_ext::SocketAddress as SocketAddressExt,
    fidl_fuchsia_sys2 as fsys, fuchsia_async as fasync,
    fuchsia_component::client::connect_to_protocol_at_path,
    fuchsia_zircon as zx,
    futures::future::join,
    futures::prelude::*,
    moniker::{RelativeMoniker, RelativeMonikerBase},
    selector_maps::{MappingError, SelectorMappingList},
    selectors::{StringSelector, TreeSelector},
    std::{borrow::Borrow, cell::RefCell, net::SocketAddr, rc::Rc, rc::Weak},
    tracing::*,
};

mod host_identifier;

pub struct RemoteControlService {
    ids: RefCell<Vec<Weak<RefCell<Vec<u64>>>>>,
    id_allocator: fn() -> Result<HostIdentifier>,
    connector: Box<dyn Fn(fidl::Socket)>,
    maps: SelectorMappingList,
}

struct Client {
    // Maintain reference-counts to this client's ids.
    // The ids may be shared (e.g. when Overnet maintains two
    // connections to the target -- legacy + CSO), so we can't
    // just maintain a list of RCS's ids and remove when one
    // disappars.  Instead, when these are freed due to the client
    // being dropped, the RCS Weak references will become invalid.
    allocated_ids: Rc<RefCell<Vec<u64>>>,
}

impl RemoteControlService {
    pub async fn new(connector: impl Fn(fidl::Socket) + 'static) -> Self {
        let f = match fuchsia_fs::file::open_in_namespace(
            "/config/data/selector-maps.json",
            io::OpenFlags::RIGHT_READABLE,
        ) {
            Ok(f) => f,
            Err(e) => {
                error!(%e, "failed to open selector maps json file");
                return Self::new_with_allocator_and_maps(
                    connector,
                    || HostIdentifier::new(),
                    SelectorMappingList::default(),
                );
            }
        };
        let bytes = match fuchsia_fs::file::read(&f).await {
            Ok(b) => b,
            Err(e) => {
                error!(?e, "failed to read bytes from selector maps json");
                return Self::new_with_allocator_and_maps(
                    connector,
                    || HostIdentifier::new(),
                    SelectorMappingList::default(),
                );
            }
        };
        let list: SelectorMappingList = match serde_json::from_slice(bytes.as_slice()) {
            Ok(m) => m,
            Err(e) => {
                error!(?e, "failed to parse selector map json");
                return Self::new_with_allocator_and_maps(
                    connector,
                    || HostIdentifier::new(),
                    SelectorMappingList::default(),
                );
            }
        };
        return Self::new_with_allocator_and_maps(connector, || HostIdentifier::new(), list);
    }

    pub(crate) fn new_with_allocator_and_maps(
        connector: impl Fn(fidl::Socket) + 'static,
        id_allocator: fn() -> Result<HostIdentifier>,
        maps: SelectorMappingList,
    ) -> Self {
        return Self {
            id_allocator,
            ids: Default::default(),
            connector: Box::new(connector),
            maps,
        };
    }

    // Some of the ID-lists may be gone because old clients have shut down.
    // They will have a strong_count of 0.  Drop 'em.
    fn remove_old_ids(self: &Rc<Self>) {
        self.ids.borrow_mut().retain(|wirc| wirc.strong_count() > 0);
    }

    async fn handle(
        self: &Rc<Self>,
        client: &Client,
        request: rcs::RemoteControlRequest,
    ) -> Result<()> {
        match request {
            rcs::RemoteControlRequest::EchoString { value, responder } => {
                info!("Received echo string {}", value);
                responder.send(&value)?;
                Ok(())
            }
            rcs::RemoteControlRequest::AddId { id, responder } => {
                client.allocated_ids.borrow_mut().push(id);
                responder.send()?;
                Ok(())
            }
            rcs::RemoteControlRequest::AddOvernetLink { id, socket, responder } => {
                (self.connector)(socket);
                client.allocated_ids.borrow_mut().push(id);
                responder.send()?;
                Ok(())
            }
            rcs::RemoteControlRequest::IdentifyHost { responder } => {
                self.clone().identify_host(responder).await?;
                Ok(())
            }
            rcs::RemoteControlRequest::ConnectCapability {
                moniker,
                capability_name,
                server_chan,
                flags,
                responder,
            } => {
                responder.send(
                    &mut self
                        .clone()
                        .connect_capability(moniker, capability_name, flags, server_chan)
                        .await,
                )?;
                Ok(())
            }
            rcs::RemoteControlRequest::Connect { selector, service_chan, responder } => {
                responder
                    .send(&mut self.clone().connect_to_service(selector, service_chan).await)?;
                Ok(())
            }
            rcs::RemoteControlRequest::RootRealmExplorer { server, responder } => {
                responder.send(
                    &mut fdio::service_connect(
                        &format!(
                            "/svc/{}.root",
                            fidl_fuchsia_sys2::RealmExplorerMarker::PROTOCOL_NAME
                        ),
                        server.into_channel(),
                    )
                    .map_err(|i| i.into_raw()),
                )?;
                Ok(())
            }
            rcs::RemoteControlRequest::RootRealmQuery { server, responder } => {
                responder.send(
                    &mut fdio::service_connect(
                        &format!(
                            "/svc/{}.root",
                            fidl_fuchsia_sys2::RealmQueryMarker::PROTOCOL_NAME
                        ),
                        server.into_channel(),
                    )
                    .map_err(|i| i.into_raw()),
                )?;
                Ok(())
            }
            rcs::RemoteControlRequest::RootLifecycleController { server, responder } => {
                responder.send(
                    &mut fdio::service_connect(
                        &format!(
                            "/svc/{}.root",
                            fidl_fuchsia_sys2::LifecycleControllerMarker::PROTOCOL_NAME
                        ),
                        server.into_channel(),
                    )
                    .map_err(|i| i.into_raw()),
                )?;
                Ok(())
            }
            rcs::RemoteControlRequest::RootRouteValidator { server, responder } => {
                responder.send(
                    &mut fdio::service_connect(
                        &format!(
                            "/svc/{}.root",
                            fidl_fuchsia_sys2::RouteValidatorMarker::PROTOCOL_NAME
                        ),
                        server.into_channel(),
                    )
                    .map_err(|i| i.into_raw()),
                )?;
                Ok(())
            }
            rcs::RemoteControlRequest::KernelStats { server, responder } => {
                responder.send(
                    &mut fdio::service_connect(
                        &format!("/svc/{}", fidl_fuchsia_kernel::StatsMarker::PROTOCOL_NAME),
                        server.into_channel(),
                    )
                    .map_err(|i| i.into_raw()),
                )?;
                Ok(())
            }
            rcs::RemoteControlRequest::BootArguments { server, responder } => {
                responder.send(
                    &mut fdio::service_connect(
                        &format!("/svc/{}", fidl_fuchsia_boot::ArgumentsMarker::PROTOCOL_NAME),
                        server.into_channel(),
                    )
                    .map_err(|i| i.into_raw()),
                )?;
                Ok(())
            }
            rcs::RemoteControlRequest::ForwardTcp { addr, socket, responder } => {
                let addr: SocketAddressExt = addr.into();
                let addr = addr.0;
                let mut result = match fasync::Socket::from_socket(socket) {
                    Ok(socket) => match self.connect_forwarded_port(addr, socket).await {
                        Ok(()) => Ok(()),
                        Err(e) => {
                            error!("Port forward connection failed: {:?}", e);
                            Err(rcs::TunnelError::ConnectFailed)
                        }
                    },
                    Err(e) => {
                        error!("Could not use socket asynchronously: {:?}", e);
                        Err(rcs::TunnelError::SocketFailed)
                    }
                };
                responder.send(&mut result)?;
                Ok(())
            }
            rcs::RemoteControlRequest::ReverseTcp { addr, client, responder } => {
                let addr: SocketAddressExt = addr.into();
                let addr = addr.0;
                let client = match client.into_proxy() {
                    Ok(proxy) => proxy,
                    Err(e) => {
                        error!("Could not communicate with callback: {:?}", e);
                        responder.send(&mut Err(rcs::TunnelError::CallbackError))?;
                        return Ok(());
                    }
                };
                let mut result = match self.listen_reversed_port(addr, client).await {
                    Ok(()) => Ok(()),
                    Err(e) => {
                        error!("Port forward connection failed: {:?}", e);
                        Err(rcs::TunnelError::ConnectFailed)
                    }
                };
                responder.send(&mut result)?;
                Ok(())
            }
            rcs::RemoteControlRequest::GetTime { responder } => {
                responder.send(fuchsia_zircon::Time::get_monotonic().into_nanos())?;
                Ok(())
            }
        }
    }

    pub async fn serve_stream(self: Rc<Self>, stream: rcs::RemoteControlRequestStream) {
        // When the stream ends, the client (and its ids) will drop
        let allocated_ids = Rc::new(RefCell::new(vec![]));
        self.ids.borrow_mut().push(Rc::downgrade(&allocated_ids));
        let client = Client { allocated_ids };
        stream
            .for_each_concurrent(None, |request| async {
                match request {
                    Ok(request) => {
                        let _ = self
                            .handle(&client, request)
                            .await
                            .map_err(|e| warn!("stream request handling error: {:?}", e));
                    }
                    Err(e) => warn!("stream error: {:?}", e),
                }
            })
            .await;
    }

    async fn listen_reversed_port(
        &self,
        listen_addr: SocketAddr,
        client: rcs::ForwardCallbackProxy,
    ) -> Result<(), std::io::Error> {
        let mut listener = fasync::net::TcpListener::bind(&listen_addr)?.accept_stream();

        fasync::Task::local(async move {
            let mut client_closed = client.on_closed().fuse();

            loop {
                // Listen for a connection, or exit if the client has gone away.
                let (stream, addr) = futures::select! {
                    result = listener.next().fuse() => {
                        match result {
                            Some(Ok(x)) => x,
                            Some(Err(e)) => {
                                warn!("Error accepting connection: {:?}", e);
                                continue;
                            }
                            None => {
                                warn!("reverse tunnel to {:?} listener socket closed", listen_addr);
                                break;
                            }
                        }
                    }
                    _ = client_closed => {
                        info!("reverse tunnel {:?} client has closed", listen_addr);
                        break;
                    }
                };

                info!("reverse tunnel connection from {:?} to {:?}", addr, listen_addr);

                let (local, remote) = zx::Socket::create_stream();

                let local = match fasync::Socket::from_socket(local) {
                    Ok(x) => x,
                    Err(e) => {
                        warn!("Error converting socket to async: {:?}", e);
                        continue;
                    }
                };

                spawn_forward_traffic(stream, local);

                // Send the socket to the client.
                if let Err(e) = client.forward(remote, &mut SocketAddressExt(addr).into()) {
                    // The client has gone away, so stop the task.
                    if let fidl::Error::ClientChannelClosed { .. } = e {
                        warn!("tunnel client channel closed while forwarding socket");
                        break;
                    }

                    warn!("Could not return forwarded socket to client: {:?}", e);
                }
            }
        })
        .detach();

        Ok(())
    }

    async fn connect_forwarded_port(
        &self,
        addr: SocketAddr,
        socket: fasync::Socket,
    ) -> Result<(), std::io::Error> {
        let tcp_conn = fasync::net::TcpStream::connect(addr)?.await?;

        spawn_forward_traffic(tcp_conn, socket);

        Ok(())
    }

    pub(crate) fn map_selector(
        self: &Rc<Self>,
        selector: Selector,
    ) -> Result<Selector, rcs::ConnectError> {
        self.maps.map_selector(selector.clone()).map_err(|e| {
            match e {
                MappingError::BadSelector(selector_str, err) => {
                    error!(?selector, ?selector_str, %err, "got invalid selector mapping");
                }
                MappingError::BadInputSelector(err) => {
                    error!(%err, "input selector invalid");
                }
                MappingError::Unbounded => {
                    error!(?selector, %e, "got a cycle in mapping selector");
                }
            }
            rcs::ConnectError::ServiceRerouteFailed
        })
    }

    pub async fn identify_host(
        self: &Rc<Self>,
        responder: rcs::RemoteControlIdentifyHostResponder,
    ) -> Result<()> {
        let identifier = match (self.id_allocator)() {
            Ok(i) => i,
            Err(e) => {
                error!(%e, "Allocating host identifier");
                return responder
                    .send(&mut Err(rcs::IdentifyHostError::ProxyConnectionFailed))
                    .context("responding to client");
            }
        };

        // We need to clean up the ids at some point. Let's do
        // it when those IDs are asked for.
        self.remove_old_ids();
        // Now the only vecs should be ones which are still held with a strong
        // Rc reference. Extract those.
        let ids: Vec<u64> = self
            .ids
            .borrow()
            .iter()
            .flat_map(|w| -> Vec<u64> {
                // This is all sadmac's fault. Grr. (Because he suggested, correctly, that
                // we use a Rc<Vec<_>> instead of Vec<Rc<_>>)
                <Rc<RefCell<Vec<u64>>> as Borrow<RefCell<Vec<u64>>>>::borrow(
                    &w.upgrade().expect("Didn't we just clear out refs with expired values??"),
                )
                .borrow()
                .clone()
            })
            .collect();
        let mut target_identity = identifier.identify().await.map(move |mut i| {
            i.ids = Some(ids);
            i
        });
        responder.send(&mut target_identity).context("responding to client")?;
        Ok(())
    }

    /// Connects to an exposed capability identified by the given moniker and capability name.
    async fn connect_capability(
        self: &Rc<Self>,
        moniker: String,
        capability_name: String,
        flags: io::OpenFlags,
        server_end: zx::Channel,
    ) -> Result<(), rcs::ConnectCapabilityError> {
        // Connect to the root LifecycleController protocol
        let lifecycle = connect_to_protocol_at_path::<fsys::LifecycleControllerMarker>(
            "/svc/fuchsia.sys2.LifecycleController.root",
        )
        .map_err(|err| {
            error!(%err, "could not connect to lifecycle controller");
            rcs::ConnectCapabilityError::CapabilityConnectFailed
        })?;

        // Connect to the root RealmQuery protocol
        let query = connect_to_protocol_at_path::<fsys::RealmQueryMarker>(
            "/svc/fuchsia.sys2.RealmQuery.root",
        )
        .map_err(|err| {
            error!(%err, "could not connect to realm query");
            rcs::ConnectCapabilityError::CapabilityConnectFailed
        })?;

        let moniker =
            relative_moniker(moniker).ok_or(rcs::ConnectCapabilityError::InvalidMoniker)?;
        connect_to_exposed_capability(moniker, capability_name, server_end, flags, lifecycle, query)
            .await
    }

    /// Connects to an exposed capability identified by the given selector.
    async fn connect_to_service(
        self: &Rc<Self>,
        selector: Selector,
        server_end: zx::Channel,
    ) -> Result<rcs::ServiceMatch, rcs::ConnectError> {
        // If applicable, get a remapped selector
        let selector = self.map_selector(selector)?;

        // Connect to the root LifecycleController protocol
        let lifecycle = connect_to_protocol_at_path::<fsys::LifecycleControllerMarker>(
            "/svc/fuchsia.sys2.LifecycleController.root",
        )
        .map_err(|err| {
            error!(%err, "could not connect to lifecycle controller");
            rcs::ConnectError::ServiceConnectFailed
        })?;

        // Connect to the root RealmQuery protocol
        let query = connect_to_protocol_at_path::<fsys::RealmQueryMarker>(
            "/svc/fuchsia.sys2.RealmQuery.root",
        )
        .map_err(|err| {
            error!(%err, "could not connect to realm query");
            rcs::ConnectError::ServiceConnectFailed
        })?;

        let (moniker, protocol_name) = extract_moniker_and_protocol_from_selector(selector)
            .ok_or(rcs::ConnectError::ServiceConnectFailed)?;
        let moniker_parts = moniker.path().iter().map(|part| part.to_string()).collect();

        connect_to_exposed_capability(
            moniker,
            protocol_name.clone(),
            server_end,
            io::OpenFlags::RIGHT_READABLE,
            lifecycle,
            query,
        )
        .await
        .map_err(|err| match err {
            rcs::ConnectCapabilityError::NoMatchingComponent => {
                rcs::ConnectError::NoMatchingServices
            }
            rcs::ConnectCapabilityError::CapabilityConnectFailed => {
                rcs::ConnectError::ServiceConnectFailed
            }
            rcs::ConnectCapabilityError::NoMatchingCapabilities => {
                rcs::ConnectError::NoMatchingServices
            }
            _ => unreachable!("we only emit the errors above"),
        })?;

        Ok(rcs::ServiceMatch {
            moniker: moniker_parts,
            subdir: "expose".to_string(),
            service: protocol_name,
        })
    }
}

fn relative_moniker(moniker: String) -> Option<RelativeMoniker> {
    // If we have an absolute moniker, make it relative, otherwise attempt to parse as a relative
    // moniker.
    let moniker = if moniker.starts_with("/") { format!(".{moniker}") } else { moniker };
    RelativeMoniker::try_from(moniker.as_str()).ok()
}

async fn connect_to_exposed_capability(
    moniker: RelativeMoniker,
    capability_name: String,
    server_end: zx::Channel,
    flags: io::OpenFlags,
    lifecycle: fsys::LifecycleControllerProxy,
    query: fsys::RealmQueryProxy,
) -> Result<(), rcs::ConnectCapabilityError> {
    // This is a no-op if already resolved.
    resolve_instance(&lifecycle, &moniker)
        .map_err(|err| match err {
            ResolveError::ActionError(ActionError::InstanceNotFound) => {
                rcs::ConnectCapabilityError::NoMatchingComponent
            }
            err => {
                error!(?err, "error resolving component");
                rcs::ConnectCapabilityError::CapabilityConnectFailed
            }
        })
        .await?;

    let exposed_dir = open_instance_dir_root_readable(&moniker, OpenDirType::Exposed, &query)
        .map_err(|err| {
            error!(?err, "error opening exposed dir");
            rcs::ConnectCapabilityError::CapabilityConnectFailed
        })
        .await?;

    connect_to_capability_in_exposed_dir(&exposed_dir, &capability_name, server_end, flags).await?;
    Ok(())
}

fn extract_moniker_and_protocol_from_selector(
    selector: Selector,
) -> Option<(RelativeMoniker, String)> {
    // Construct the moniker from the selector
    let moniker_segments = selector.component_selector?.moniker_segments?;
    let mut children = vec![];
    for segment in moniker_segments {
        let child = segment.exact_match()?.to_string();
        children.push(child);
    }
    let moniker = format!("./{}", children.join("/"));

    let tree_selector = selector.tree_selector?;

    // Namespace must be `expose`. Nothing else is supported.
    let namespace = tree_selector.node_path()?.get(0)?.exact_match()?;
    if namespace != "expose" {
        return None;
    }

    // Get the protocol name
    let property_selector = tree_selector.property()?;
    let protocol_name = property_selector.exact_match()?.to_string();

    // Make sure the moniker is valid
    let moniker = RelativeMoniker::try_from(moniker.as_str())
        .map_err(|err| {
            error!(%err, "moniker invalid");
            err
        })
        .ok()?;

    Some((moniker, protocol_name))
}

async fn connect_to_capability_in_exposed_dir(
    exposed_dir: &io::DirectoryProxy,
    capability_name: &str,
    server_end: zx::Channel,
    flags: io::OpenFlags,
) -> Result<(), rcs::ConnectCapabilityError> {
    // Check if capability exists in exposed dir.
    let entries = fuchsia_fs::directory::readdir(exposed_dir)
        .await
        .map_err(|_| rcs::ConnectCapabilityError::CapabilityConnectFailed)?;
    let is_capability_exposed = entries.iter().any(|e| &e.name == &capability_name);
    if !is_capability_exposed {
        return Err(rcs::ConnectCapabilityError::NoMatchingCapabilities);
    }

    // Connect to the capability
    exposed_dir
        .open(flags, io::ModeType::empty(), capability_name, ServerEnd::new(server_end))
        .map_err(|err| {
            error!(%err, "error opening capability from exposed dir");
            rcs::ConnectCapabilityError::CapabilityConnectFailed
        })
}

#[derive(Debug)]
enum ForwardError {
    TcpToZx(anyhow::Error),
    ZxToTcp(anyhow::Error),
    Both { tcp_to_zx: anyhow::Error, zx_to_tcp: anyhow::Error },
}

fn spawn_forward_traffic(tcp_side: fasync::net::TcpStream, zx_side: fasync::Socket) {
    fasync::Task::local(async move {
        match forward_traffic(tcp_side, zx_side).await {
            Ok(()) => {}
            Err(ForwardError::TcpToZx(err)) => {
                error!("error forwarding from tcp to zx socket: {:#}", err);
            }
            Err(ForwardError::ZxToTcp(err)) => {
                error!("error forwarding from zx to tcp socket: {:#}", err);
            }
            Err(ForwardError::Both { tcp_to_zx, zx_to_tcp }) => {
                error!("error forwarding from zx to tcp socket:\n{:#}\n{:#}", tcp_to_zx, zx_to_tcp);
            }
        }
    })
    .detach()
}

async fn forward_traffic(
    tcp_side: fasync::net::TcpStream,
    zx_side: fasync::Socket,
) -> Result<(), ForwardError> {
    // We will forward traffic with two sub-tasks. One to stream bytes from the
    // tcp socket to the zircon socket, and vice versa. Since we have two tasks,
    // we need to handle how we exit the loops, otherwise we risk leaking
    // resource.
    //
    // To handle this, we'll create two promises that will resolve upon the
    // stream closing. For the zircon socket, we can use a native signal, but
    // unfortunately fasync::net::TcpStream doesn't support listening for
    // closure, so we'll just use a oneshot channel to signal to the other task
    // when the tcp stream closes.
    let (tcp_closed_tx, mut tcp_closed_rx) = futures::channel::oneshot::channel::<()>();
    let mut zx_closed = fasync::OnSignals::new(&zx_side, zx::Signals::SOCKET_PEER_CLOSED).fuse();
    let zx_side = &zx_side;

    let (mut tcp_read, mut tcp_write) = tcp_side.split();
    let (mut zx_read, mut zx_write) = zx_side.split();

    let tcp_to_zx = async move {
        let res = async move {
            // TODO(84188): Use a buffer pool once we have them.
            let mut buf = [0; 4096];
            loop {
                futures::select! {
                    res = tcp_read.read(&mut buf).fuse() => {
                        let num_bytes = res.context("read tcp socket")?;
                        if num_bytes == 0 {
                            return Ok(());
                        }

                        zx_write.write_all(&mut buf[..num_bytes]).await.context("write zx socket")?;
                        zx_write.flush().await.context("flush zx socket")?;
                    }
                    _ = zx_closed => {
                        return Ok(());
                    }
                }
            }
        }
        .await;

        // Let the other task know the tcp stream has shut down. If the other
        // task finished before this one, this send could fail. That's okay, so
        // just ignore the result.
        let _ = tcp_closed_tx.send(());

        res
    };

    let zx_to_tcp = async move {
        // TODO(84188): Use a buffer pool once we have them.
        let mut buf = [0; 4096];
        loop {
            futures::select! {
                res = zx_read.read(&mut buf).fuse() => {
                    let num_bytes = res.context("read zx socket")?;
                    if num_bytes == 0 {
                        return Ok(());
                    }
                    tcp_write.write_all(&mut buf[..num_bytes]).await.context("write tcp socket")?;
                    tcp_write.flush().await.context("flush tcp socket")?;
                }
                _ = tcp_closed_rx => {
                    break Ok(());
                }
            }
        }
    };

    match join(tcp_to_zx, zx_to_tcp).await {
        (Ok(()), Ok(())) => Ok(()),
        (Err(tcp_to_zx), Err(zx_to_tcp)) => Err(ForwardError::Both { tcp_to_zx, zx_to_tcp }),
        (Err(tcp_to_zx), Ok(())) => Err(ForwardError::TcpToZx(tcp_to_zx)),
        (Ok(()), Err(zx_to_tcp)) => Err(ForwardError::ZxToTcp(zx_to_tcp)),
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        assert_matches::assert_matches,
        fidl_fuchsia_buildinfo as buildinfo, fidl_fuchsia_developer_remotecontrol as rcs,
        fidl_fuchsia_device as fdevice, fidl_fuchsia_hwinfo as hwinfo, fidl_fuchsia_io as fio,
        fidl_fuchsia_net as fnet, fidl_fuchsia_net_interfaces as fnet_interfaces,
        fuchsia_component::server::ServiceFs,
        fuchsia_zircon as zx,
        selectors::{parse_selector, VerboseError},
        std::net::Ipv4Addr,
    };

    const NODENAME: &'static str = "thumb-set-human-shred";
    const BOOT_TIME: u64 = 123456789000000000;
    const SERIAL: &'static str = "test_serial";
    const BOARD_CONFIG: &'static str = "test_board_name";
    const PRODUCT_CONFIG: &'static str = "core";
    const FAKE_SERVICE_SELECTOR: &'static str = "my/component:expose:some.fake.Service";
    const MAPPED_SERVICE_SELECTOR: &'static str = "my/other/component:out:some.fake.mapped.Service";

    const IPV4_ADDR: [u8; 4] = [127, 0, 0, 1];
    const IPV6_ADDR: [u8; 16] = [127, 1, 2, 3, 4, 5, 6, 7, 8, 9, 1, 2, 3, 4, 5, 6];

    fn setup_fake_device_service() -> hwinfo::DeviceProxy {
        let (proxy, mut stream) =
            fidl::endpoints::create_proxy_and_stream::<hwinfo::DeviceMarker>().unwrap();
        fasync::Task::spawn(async move {
            while let Ok(Some(req)) = stream.try_next().await {
                match req {
                    hwinfo::DeviceRequest::GetInfo { responder } => {
                        let _ = responder.send(&hwinfo::DeviceInfo {
                            serial_number: Some(String::from(SERIAL)),
                            ..Default::default()
                        });
                    }
                }
            }
        })
        .detach();

        proxy
    }

    fn setup_fake_build_info_service() -> buildinfo::ProviderProxy {
        let (proxy, mut stream) =
            fidl::endpoints::create_proxy_and_stream::<buildinfo::ProviderMarker>().unwrap();
        fasync::Task::spawn(async move {
            while let Ok(Some(req)) = stream.try_next().await {
                match req {
                    buildinfo::ProviderRequest::GetBuildInfo { responder } => {
                        let _ = responder.send(&buildinfo::BuildInfo {
                            board_config: Some(String::from(BOARD_CONFIG)),
                            product_config: Some(String::from(PRODUCT_CONFIG)),
                            ..Default::default()
                        });
                    }
                }
            }
        })
        .detach();

        proxy
    }

    fn setup_fake_name_provider_service() -> fdevice::NameProviderProxy {
        let (proxy, mut stream) =
            fidl::endpoints::create_proxy_and_stream::<fdevice::NameProviderMarker>().unwrap();

        fasync::Task::spawn(async move {
            while let Ok(Some(req)) = stream.try_next().await {
                match req {
                    fdevice::NameProviderRequest::GetDeviceName { responder } => {
                        let _ = responder.send(&mut Ok(String::from(NODENAME)));
                    }
                }
            }
        })
        .detach();

        proxy
    }

    fn setup_fake_interface_state_service() -> fnet_interfaces::StateProxy {
        let (proxy, mut stream) =
            fidl::endpoints::create_proxy_and_stream::<fnet_interfaces::StateMarker>().unwrap();

        fasync::Task::spawn(async move {
            while let Ok(Some(req)) = stream.try_next().await {
                match req {
                    fnet_interfaces::StateRequest::GetWatcher {
                        options: _,
                        watcher,
                        control_handle: _,
                    } => {
                        let mut stream = watcher.into_stream().unwrap();
                        let mut first = true;
                        while let Ok(Some(req)) = stream.try_next().await {
                            match req {
                                fnet_interfaces::WatcherRequest::Watch { responder } => {
                                    let mut event = if first {
                                        first = false;
                                        fnet_interfaces::Event::Existing(
                                            fnet_interfaces::Properties {
                                                id: Some(1),
                                                addresses: Some(
                                                    IntoIterator::into_iter([
                                                        fnet::Subnet {
                                                            addr: fnet::IpAddress::Ipv4(
                                                                fnet::Ipv4Address {
                                                                    addr: IPV4_ADDR,
                                                                },
                                                            ),
                                                            prefix_len: 4,
                                                        },
                                                        fnet::Subnet {
                                                            addr: fnet::IpAddress::Ipv6(
                                                                fnet::Ipv6Address {
                                                                    addr: IPV6_ADDR,
                                                                },
                                                            ),
                                                            prefix_len: 110,
                                                        },
                                                    ])
                                                    .map(Some)
                                                    .map(|addr| fnet_interfaces::Address {
                                                        addr,
                                                        valid_until: Some(1),
                                                        ..Default::default()
                                                    })
                                                    .collect(),
                                                ),
                                                online: Some(true),
                                                device_class: Some(
                                                    fnet_interfaces::DeviceClass::Loopback(
                                                        fnet_interfaces::Empty {},
                                                    ),
                                                ),
                                                has_default_ipv4_route: Some(false),
                                                has_default_ipv6_route: Some(false),
                                                name: Some(String::from("eth0")),
                                                ..Default::default()
                                            },
                                        )
                                    } else {
                                        fnet_interfaces::Event::Idle(fnet_interfaces::Empty {})
                                    };
                                    let () = responder.send(&mut event).unwrap();
                                }
                            }
                        }
                    }
                }
            }
        })
        .detach();

        proxy
    }

    fn make_rcs() -> Rc<RemoteControlService> {
        make_rcs_with_maps(vec![])
    }

    fn make_rcs_with_maps(maps: Vec<(&str, &str)>) -> Rc<RemoteControlService> {
        Rc::new(RemoteControlService::new_with_allocator_and_maps(
            |_| (),
            || {
                Ok(HostIdentifier {
                    interface_state_proxy: setup_fake_interface_state_service(),
                    name_provider_proxy: setup_fake_name_provider_service(),
                    device_info_proxy: setup_fake_device_service(),
                    build_info_proxy: setup_fake_build_info_service(),
                    boot_timestamp_nanos: BOOT_TIME,
                })
            },
            SelectorMappingList::new(
                maps.iter().map(|s| (s.0.to_string(), s.1.to_string())).collect(),
            ),
        ))
    }

    fn setup_rcs_proxy() -> rcs::RemoteControlProxy {
        let service = make_rcs();

        let (rcs_proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<rcs::RemoteControlMarker>().unwrap();
        fasync::Task::local(async move {
            service.serve_stream(stream).await;
        })
        .detach();

        return rcs_proxy;
    }

    fn setup_fake_lifecycle_controller() -> fsys::LifecycleControllerProxy {
        fidl::endpoints::spawn_stream_handler(
            move |request: fsys::LifecycleControllerRequest| async move {
                match request {
                    fsys::LifecycleControllerRequest::ResolveInstance { moniker, responder } => {
                        assert_eq!(moniker, "./core/my_component");
                        responder.send(&mut Ok(())).unwrap()
                    }
                    _ => panic!("unexpected request: {:?}", request),
                }
            },
        )
        .unwrap()
    }

    fn setup_exposed_dir(server: ServerEnd<fio::DirectoryMarker>) {
        let mut fs = ServiceFs::new();
        fs.add_fidl_service(move |_: hwinfo::BoardRequestStream| {});
        fs.serve_connection(server).unwrap();
        fasync::Task::spawn(fs.collect::<()>()).detach();
    }

    fn setup_fake_realm_query() -> fsys::RealmQueryProxy {
        fidl::endpoints::spawn_stream_handler(move |request: fsys::RealmQueryRequest| async move {
            match request {
                fsys::RealmQueryRequest::Open {
                    moniker,
                    dir_type,
                    flags,
                    mode,
                    path,
                    object,
                    responder,
                } => {
                    assert_eq!(moniker, "./core/my_component");
                    assert_eq!(dir_type, fsys::OpenDirType::ExposedDir);
                    assert_eq!(flags, fio::OpenFlags::RIGHT_READABLE);
                    assert_eq!(mode, fio::ModeType::empty());
                    assert_eq!(path, ".");

                    setup_exposed_dir(object.into_channel().into());

                    responder.send(&mut Ok(())).unwrap()
                }
                _ => panic!("unexpected request: {:?}", request),
            }
        })
        .unwrap()
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_extract_moniker_protocol() -> Result<()> {
        let selector =
            parse_selector::<VerboseError>("core/my_component:expose:fuchsia.foo.bar").unwrap();
        let (moniker, protocol) = extract_moniker_and_protocol_from_selector(selector).unwrap();

        assert_eq!(moniker, RelativeMoniker::try_from("./core/my_component").unwrap());
        assert_eq!(protocol, "fuchsia.foo.bar");

        for selector in [
            "*:*:*",
            "core/my_component:expose",
            "core/my_component:expose:*",
            "*:expose:fuchsia.foo.bar",
            "core/my_component:*:fuchsia.foo.bar",
            "core/my_component:out:fuchsia.foo.bar",
            "core/my_component:in:fuchsia.foo.bar",
        ] {
            let selector = parse_selector::<VerboseError>(selector).unwrap();
            assert!(extract_moniker_and_protocol_from_selector(selector).is_none());
        }

        Ok(())
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_relative_moniker() {
        assert!(relative_moniker("/core/foo".to_string()).is_some());
        assert!(relative_moniker("./core/foo".to_string()).is_some());
        assert!(relative_moniker("core/foo".to_string()).is_none());
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_connect_to_exposed_capability() -> Result<()> {
        let (_client, server) = zx::Channel::create();
        let lifecycle = setup_fake_lifecycle_controller();
        let query = setup_fake_realm_query();
        connect_to_exposed_capability(
            RelativeMoniker::try_from("./core/my_component").unwrap(),
            "fuchsia.hwinfo.Board".to_string(),
            server,
            io::OpenFlags::RIGHT_READABLE,
            lifecycle,
            query,
        )
        .await
        .unwrap();
        Ok(())
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_connect_to_capability_not_exposed() -> Result<()> {
        let (_client, server) = zx::Channel::create();
        let lifecycle = setup_fake_lifecycle_controller();
        let query = setup_fake_realm_query();
        let error = connect_to_exposed_capability(
            RelativeMoniker::try_from("./core/my_component").unwrap(),
            "fuchsia.not.exposed".to_string(),
            server,
            io::OpenFlags::RIGHT_READABLE,
            lifecycle,
            query,
        )
        .await
        .unwrap_err();
        assert_eq!(error, rcs::ConnectCapabilityError::NoMatchingCapabilities);
        Ok(())
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_identify_host() -> Result<()> {
        let rcs_proxy = setup_rcs_proxy();

        let resp = rcs_proxy.identify_host().await.unwrap().unwrap();

        assert_eq!(resp.serial_number.unwrap(), SERIAL);
        assert_eq!(resp.board_config.unwrap(), BOARD_CONFIG);
        assert_eq!(resp.product_config.unwrap(), PRODUCT_CONFIG);
        assert_eq!(resp.nodename.unwrap(), NODENAME);

        let addrs = resp.addresses.unwrap();
        assert_eq!(
            addrs[..],
            [
                fnet::Subnet {
                    addr: fnet::IpAddress::Ipv4(fnet::Ipv4Address { addr: IPV4_ADDR }),
                    prefix_len: 4,
                },
                fnet::Subnet {
                    addr: fnet::IpAddress::Ipv6(fnet::Ipv6Address { addr: IPV6_ADDR }),
                    prefix_len: 110,
                }
            ]
        );

        assert_eq!(resp.boot_timestamp_nanos.unwrap(), BOOT_TIME);

        Ok(())
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_ids_in_host_identify() -> Result<()> {
        let rcs_proxy = setup_rcs_proxy();

        let ident = rcs_proxy.identify_host().await.unwrap().unwrap();
        assert_eq!(ident.ids, Some(vec![]));

        rcs_proxy.add_id(1234).await.unwrap();
        rcs_proxy.add_id(4567).await.unwrap();

        let ident = rcs_proxy.identify_host().await.unwrap().unwrap();
        let ids = ident.ids.unwrap();
        assert_eq!(ids.len(), 2);
        assert_eq!(1234u64, ids[0]);
        assert_eq!(4567u64, ids[1]);

        Ok(())
    }

    fn service_selector() -> Selector {
        parse_selector::<VerboseError>(FAKE_SERVICE_SELECTOR).unwrap()
    }

    fn mapped_service_selector() -> Selector {
        parse_selector::<VerboseError>(MAPPED_SERVICE_SELECTOR).unwrap()
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_map_selector() -> Result<()> {
        let service = make_rcs_with_maps(vec![(FAKE_SERVICE_SELECTOR, MAPPED_SERVICE_SELECTOR)]);

        assert_eq!(service.map_selector(service_selector()).unwrap(), mapped_service_selector());
        Ok(())
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_map_selector_broken_mapping() -> Result<()> {
        let service = make_rcs_with_maps(vec![(FAKE_SERVICE_SELECTOR, "not_a_selector:::::")]);

        assert_matches!(
            service.map_selector(service_selector()).unwrap_err(),
            rcs::ConnectError::ServiceRerouteFailed
        );
        Ok(())
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_map_selector_unbounded_mapping() -> Result<()> {
        let service = make_rcs_with_maps(vec![
            (FAKE_SERVICE_SELECTOR, MAPPED_SERVICE_SELECTOR),
            (MAPPED_SERVICE_SELECTOR, FAKE_SERVICE_SELECTOR),
        ]);

        assert_matches!(
            service.map_selector(service_selector()).unwrap_err(),
            rcs::ConnectError::ServiceRerouteFailed
        );
        Ok(())
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_map_selector_no_matches() -> Result<()> {
        let service =
            make_rcs_with_maps(vec![("not/a/match:out:some.Service", MAPPED_SERVICE_SELECTOR)]);

        assert_eq!(service.map_selector(service_selector()).unwrap(), service_selector());
        Ok(())
    }

    async fn create_forward_tunnel(
    ) -> (fasync::net::TcpStream, fasync::Socket, fasync::Task<Result<(), ForwardError>>) {
        let addr = (Ipv4Addr::LOCALHOST, 0).into();
        let listener = fasync::net::TcpListener::bind(&addr).unwrap();
        let listen_addr = listener.local_addr().unwrap();
        let mut listener_stream = listener.accept_stream();

        let (remote_tx, remote_rx) = futures::channel::oneshot::channel();

        // Run the listener in a background task so it can forward traffic in
        // parallel with the test.
        let forward_task = fasync::Task::local(async move {
            let (stream, _) = listener_stream.next().await.unwrap().unwrap();

            let (local, remote) = zx::Socket::create_stream();
            let local = fasync::Socket::from_socket(local).unwrap();
            let remote = fasync::Socket::from_socket(remote).unwrap();

            remote_tx.send(remote).unwrap();

            forward_traffic(stream, local).await
        });

        // We should connect to the TCP socket, which should set us up a zircon socket.
        let tcp_stream = fasync::net::TcpStream::connect(listen_addr).unwrap().await.unwrap();
        let zx_socket = remote_rx.await.unwrap();

        (tcp_stream, zx_socket, forward_task)
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_forward_traffic_tcp_closes_first() {
        let (mut tcp_stream, mut zx_socket, forward_task) = create_forward_tunnel().await;

        // Now any traffic that is sent to the tcp stream should come out of the zx socket.
        let msg = b"ping";
        tcp_stream.write_all(msg).await.unwrap();

        let mut buf = [0; 4096];
        zx_socket.read_exact(&mut buf[..msg.len()]).await.unwrap();
        assert_eq!(&buf[..msg.len()], msg);

        // Send a reply from the zx socket to the tcp stream.
        let msg = b"pong";
        zx_socket.write_all(msg).await.unwrap();

        tcp_stream.read_exact(&mut buf[..msg.len()]).await.unwrap();
        assert_eq!(&buf[..msg.len()], msg);

        // Now, close the tcp stream, this should cause the zx socket to close as well.
        std::mem::drop(tcp_stream);

        let mut buf = vec![];
        zx_socket.read_to_end(&mut buf).await.unwrap();
        assert_eq!(&buf, &Vec::<u8>::default());

        // Make sure the forward task shuts down as well.
        assert_matches!(forward_task.await, Ok(()));
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_forward_traffic_zx_socket_closes_first() {
        let (mut tcp_stream, mut zx_socket, forward_task) = create_forward_tunnel().await;

        // Check that the zx socket can send the first data.
        let msg = b"ping";
        zx_socket.write_all(msg).await.unwrap();

        let mut buf = [0; 4096];
        tcp_stream.read_exact(&mut buf[..msg.len()]).await.unwrap();
        assert_eq!(&buf[..msg.len()], msg);

        let msg = b"pong";
        tcp_stream.write_all(msg).await.unwrap();

        zx_socket.read_exact(&mut buf[..msg.len()]).await.unwrap();
        assert_eq!(&buf[..msg.len()], msg);

        // Now, close the zx socket, this should cause the tcp stream to close as well.
        std::mem::drop(zx_socket);

        let mut buf = vec![];
        tcp_stream.read_to_end(&mut buf).await.unwrap();
        assert_eq!(&buf, &Vec::<u8>::default());

        // Make sure the forward task shuts down as well.
        assert_matches!(forward_task.await, Ok(()));
    }
}
