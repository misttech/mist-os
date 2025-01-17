// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implements fuchsia.netpol.socketproxy.NetworkRegistry.

use anyhow::{Context, Error};
use fidl::endpoints::{ControlHandle, RequestStream};
use fidl_fuchsia_netpol_socketproxy::{
    self as fnp_socketproxy, FuchsiaNetworksRequest, Network, NetworkDnsServers, NetworkInfo,
    StarnixNetworksRequest,
};
use fuchsia_inspect_derive::{IValue, Inspect, Unit};
use futures::channel::mpsc;
use futures::lock::Mutex;
use futures::{SinkExt as _, StreamExt as _, TryStreamExt as _};
use log::{error, info, warn};
use std::collections::HashMap;
use std::sync::Arc;
use {fidl_fuchsia_net as fnet, fidl_fuchsia_posix_socket as fposix_socket};

/// RFC-1035§4.2 specifies port 53 (decimal) as the default port for DNS requests.
const DEFAULT_DNS_PORT: u16 = 53;

/// If there are networks registered, but no default has been set, this value
/// will be used, otherwise the mark will be OptionalUint32::Unset(Empty).
pub(crate) const DEFAULT_SOCKET_MARK: u32 = 0;

enum CommonErrors {
    MissingNetworkId,
    MissingNetworkInfo,
    MissingNetworkDnsServers,
}

trait IpAddressExt {
    fn to_dns_socket_address(self) -> fnet::SocketAddress;
}

impl<T: IpAddressExt + Copy> IpAddressExt for &T {
    fn to_dns_socket_address(self) -> fnet::SocketAddress {
        (*self).to_dns_socket_address()
    }
}

impl IpAddressExt for fnet::Ipv4Address {
    fn to_dns_socket_address(self) -> fnet::SocketAddress {
        fnet::SocketAddress::Ipv4(fnet::Ipv4SocketAddress { address: self, port: DEFAULT_DNS_PORT })
    }
}

impl IpAddressExt for fnet::Ipv6Address {
    fn to_dns_socket_address(self) -> fnet::SocketAddress {
        fnet::SocketAddress::Ipv6(fnet::Ipv6SocketAddress {
            address: self,
            port: DEFAULT_DNS_PORT,
            zone_index: 0,
        })
    }
}

trait NetworkInfoExt {
    fn mark(&self) -> Option<u32>;
}

impl NetworkInfoExt for NetworkInfo {
    fn mark(&self) -> Option<u32> {
        match self {
            NetworkInfo::Starnix(s) => s.mark,
            // Sockets express using Fuchsia's default network by setting
            // the mark to None.
            NetworkInfo::Fuchsia(_) | _ => None,
        }
    }
}

/// A copy of fnp_socketproxy::Network that ensures that all fields are present.
#[derive(Debug, Clone)]
pub(crate) struct ValidatedNetwork {
    network_id: u32,
    info: NetworkInfo,
    dns_servers: NetworkDnsServers,
}

impl ValidatedNetwork {
    fn dns_servers(&self) -> Vec<fnet::SocketAddress> {
        self.dns_servers
            .v4
            .iter()
            .flat_map(|a| a.iter().map(IpAddressExt::to_dns_socket_address))
            .chain(
                self.dns_servers
                    .v6
                    .iter()
                    .flat_map(|a| a.iter().map(IpAddressExt::to_dns_socket_address)),
            )
            .collect()
    }
}

trait ValidateNetworkExt {
    fn validate(self) -> Result<ValidatedNetwork, CommonErrors>;
}

impl ValidateNetworkExt for Network {
    fn validate(self) -> Result<ValidatedNetwork, CommonErrors> {
        match self {
            Network { network_id: None, .. } => Err(CommonErrors::MissingNetworkId),
            Network { info: None, .. } => Err(CommonErrors::MissingNetworkInfo),
            Network { dns_servers: None, .. } => Err(CommonErrors::MissingNetworkDnsServers),
            Network {
                network_id: Some(network_id),
                info: Some(info),
                dns_servers: Some(dns_servers),
                ..
            } => Ok(ValidatedNetwork { network_id, info, dns_servers }),
        }
    }
}

macro_rules! common_errors_impl {
    ($($p:ty),+) => {
        $(
            impl From<CommonErrors> for $p {
                fn from(value: CommonErrors) -> Self {
                    use CommonErrors::*;
                    match value {
                        MissingNetworkId => <$p>::MissingNetworkId,
                        MissingNetworkInfo => <$p>::MissingNetworkInfo,
                        MissingNetworkDnsServers => <$p>::MissingNetworkDnsServers,
                    }
                }
            }
        )+
    }
}

common_errors_impl!(
    fnp_socketproxy::NetworkRegistryAddError,
    fnp_socketproxy::NetworkRegistryUpdateError
);

/// NetworkRegistry tracks the networks that have been registered.
#[derive(Inspect, Debug, Default)]
struct NetworkRegistry {
    networks: IValue<RegisteredNetworks>,

    inspect_node: fuchsia_inspect::Node,
}

impl NetworkRegistry {
    /// Returns a collated list of DnsServerList objects.
    pub(crate) fn dns_servers(&self) -> Vec<fnp_socketproxy::DnsServerList> {
        self.networks.dns_servers()
    }

    /// Returns whether the network registry has a default network set.
    pub(crate) fn has_default_network(&self) -> bool {
        self.networks.default_network_id.is_some()
    }
}

#[derive(Unit, Debug, Default)]
struct MethodInspect {
    successes: u32,
    errors: u32,
}

#[derive(Unit, Default, Debug)]
struct RegisteredNetworks {
    default_network_id: Option<u32>,

    #[inspect(skip)]
    /// A mapping from network id to ValidatedNetwork for each registered network.
    networks: HashMap<u32, ValidatedNetwork>,

    adds: MethodInspect,
    removes: MethodInspect,
    set_defaults: MethodInspect,
    updates: MethodInspect,
}

impl RegisteredNetworks {
    fn add_network(&mut self, network: Network) -> fnp_socketproxy::NetworkRegistryAddResult {
        let network = network.validate()?;
        #[allow(clippy::map_entry, reason = "mass allow for https://fxbug.dev/381896734")]
        if self.networks.contains_key(&network.network_id) {
            self.adds.errors += 1;
            Err(fnp_socketproxy::NetworkRegistryAddError::DuplicateNetworkId)
        } else {
            let _: Option<_> = self.networks.insert(network.network_id, network);
            self.adds.successes += 1;
            Ok(())
        }
    }

    /// Empties the registered networks.
    pub(crate) fn clear(&mut self) {
        self.networks.clear();
    }

    fn update_network(&mut self, network: Network) -> fnp_socketproxy::NetworkRegistryUpdateResult {
        let network = network.validate()?;
        let network_id = network.network_id;
        *self
            .networks
            .get_mut(&network_id)
            .ok_or(fnp_socketproxy::NetworkRegistryUpdateError::NotFound)
            .inspect(|_| self.updates.successes += 1)
            .inspect_err(|_| self.updates.errors += 1)? = network;
        Ok(())
    }

    fn remove_network(&mut self, network_id: u32) -> fnp_socketproxy::NetworkRegistryRemoveResult {
        if self.default_network_id == Some(network_id) {
            self.removes.errors += 1;
            return Err(fnp_socketproxy::NetworkRegistryRemoveError::CannotRemoveDefaultNetwork);
        }
        match self.networks.remove(&network_id) {
            Some(_) => {
                self.removes.successes += 1;
                Ok(())
            }
            None => {
                self.removes.errors += 1;
                Err(fnp_socketproxy::NetworkRegistryRemoveError::NotFound)
            }
        }
    }

    /// Update the currently set default network id.
    ///
    /// If `network_id` is None, the default network id will be unset.
    fn set_default_network(
        &mut self,
        network_id: Option<u32>,
    ) -> fnp_socketproxy::NetworkRegistrySetDefaultResult {
        if let Some(network_id) = network_id {
            if !self.networks.contains_key(&network_id) {
                self.set_defaults.errors += 1;
                return Err(fnp_socketproxy::NetworkRegistrySetDefaultError::NotFound);
            }
        }
        self.set_defaults.successes += 1;
        self.default_network_id = network_id;

        Ok(())
    }

    /// Returns a collated list of DnsServerList objects.
    pub(crate) fn dns_servers(&self) -> Vec<fnp_socketproxy::DnsServerList> {
        self.networks
            .iter()
            .map(|(id, network)| fnp_socketproxy::DnsServerList {
                source_network_id: Some(*id),
                addresses: Some(network.dns_servers()),
                ..Default::default()
            })
            .collect()
    }

    fn current_mark(&self) -> fposix_socket::OptionalUint32 {
        use fposix_socket::OptionalUint32::*;
        match (self.default_network_id, self.networks.is_empty()) {
            (None, false) => Value(DEFAULT_SOCKET_MARK),
            (id, _) => match id.and_then(|id| self.networks[&id].info.mark()) {
                Some(value) => Value(value),
                None => Unset(fposix_socket::Empty),
            },
        }
    }

    fn len(&self) -> usize {
        self.networks.len()
    }
}

#[derive(Inspect, Clone, Debug, Default)]
pub struct NetworkRegistries {
    starnix: Arc<Mutex<NetworkRegistry>>,
    fuchsia: Arc<Mutex<NetworkRegistry>>,
}

impl NetworkRegistries {
    // When Fuchsia has a default network, then prefer its mark
    // over any existing mark. When it is unset, then fallback
    // to the mark provided by Starnix.
    async fn current_mark(&self) -> fposix_socket::OptionalUint32 {
        if self.fuchsia.lock().await.has_default_network() {
            info!("FuchsiaNetworks has a default network, preferring Fuchsia mark.");
            return self.fuchsia.lock().await.networks.current_mark();
        }

        return self.starnix.lock().await.networks.current_mark();
    }

    // When Fuchsia has a default network, then prefer its DNS
    // over any existing DNS servers. When it is unset, then
    // fallback to the DNS servers provided by Starnix.
    async fn current_dns_servers(&self) -> Vec<fnp_socketproxy::DnsServerList> {
        if self.fuchsia.lock().await.has_default_network() {
            info!("FuchsiaNetworks has a default network, preferring Fuchsia DNS.");
            return self.fuchsia.lock().await.dns_servers();
        }

        return self.starnix.lock().await.dns_servers();
    }
}

#[derive(Inspect, Clone, Debug)]
pub struct Registry {
    #[inspect(forward)]
    networks: NetworkRegistries,
    // Reflects the marks that are set on the sockets vended
    // by this component.
    marks: Arc<Mutex<crate::SocketMarks>>,
    dns_tx: mpsc::Sender<Vec<fnp_socketproxy::DnsServerList>>,

    starnix_occupant: Arc<Mutex<()>>,
    fuchsia_occupant: Arc<Mutex<()>>,
}

macro_rules! handle_registry_request {
    ($request_type:ident, $request:expr, $network_registry:expr, $name:expr) => {{
        let mut networks = $network_registry.networks.as_mut();
        let (op, send, did_state_change): (
            _,
            Box<dyn FnOnce() -> Result<(), _> + Send + Sync + 'static>,
            bool,
        ) = match $request {
            $request_type::SetDefault { network_id, responder } => {
                let result = networks.set_default_network(match network_id {
                    fposix_socket::OptionalUint32::Value(value) => Some(value),
                    fposix_socket::OptionalUint32::Unset(_) => None,
                });
                ("set default", Box::new(move || responder.send(result)), true)
            }
            $request_type::Add { network, responder } => {
                let result = networks.add_network(network);
                ("add", Box::new(move || responder.send(result)), true)
            }
            $request_type::Update { network, responder } => {
                let result = networks.update_network(network);
                ("update", Box::new(move || responder.send(result)), true)
            }
            $request_type::Remove { network_id, responder } => {
                let result = networks.remove_network(network_id);
                ("remove", Box::new(move || responder.send(result)), true)
            }
            $request_type::CheckPresence { responder } => {
                ("check_presence", Box::new(move || responder.send()), false)
            }
        };
        if did_state_change {
            let new_mark = networks.current_mark();
            info!(
                "{} registry {op}. mark: {new_mark:?}, networks count: {}",
                $name,
                networks.len()
            );
        }
        std::mem::drop(networks);
        (send, did_state_change)
    }};
}

impl Registry {
    pub(crate) fn new(
        marks: Arc<Mutex<crate::SocketMarks>>,
        dns_tx: mpsc::Sender<Vec<fnp_socketproxy::DnsServerList>>,
    ) -> Self {
        Self {
            networks: Default::default(),
            marks,
            dns_tx,
            starnix_occupant: Default::default(),
            fuchsia_occupant: Default::default(),
        }
    }

    pub(crate) async fn run_starnix(
        &self,
        stream: fnp_socketproxy::StarnixNetworksRequestStream,
    ) -> Result<(), Error> {
        let _occupant = match self.starnix_occupant.try_lock() {
            Some(o) => o,
            None => {
                warn!("Only one connection to StarnixNetworks is allowed at a time");
                stream.control_handle().shutdown_with_epitaph(fidl::Status::ACCESS_DENIED);
                return Ok(());
            }
        };

        info!("Starting fuchsia.netpol.socketproxy.StarnixNetworks server");
        self.networks.starnix.lock().await.networks.as_mut().clear();
        stream
            .map(|result| result.context("failed request"))
            .try_for_each(|request| async {
                let mut network_registry = self.networks.starnix.lock().await;
                let (send, did_state_change): (
                    Box<dyn FnOnce() -> Result<(), _> + Send + Sync + 'static>,
                    bool,
                ) = handle_registry_request!(
                    StarnixNetworksRequest,
                    request,
                    network_registry,
                    "starnix"
                );
                std::mem::drop(network_registry);

                if did_state_change {
                    // We do feed here instead of send so that we don't wait for a flush
                    // in the event that the DnsServerWatcher is not running.
                    self.dns_tx
                        .clone()
                        .feed(self.networks.current_dns_servers().await)
                        .await
                        .unwrap_or_else(|e| {
                            if !e.is_disconnected() {
                                // Log if the feed fails for reasons other than disconnection.
                                error!("Unable to feed DNS update: {e:?}")
                            }
                        });

                    // Ensure the mark is updated prior to sending out the response
                    // and dropping the registry.
                    self.marks.lock().await.mark_1 = self.networks.current_mark().await;
                    send().context("error sending response")?;
                } else {
                    // Not a request involving a state change,
                    // so ignore any errors.
                    let _: Result<(), fidl::Error> = send();
                }
                Ok(())
            })
            .await
    }

    pub(crate) async fn run_fuchsia(
        &self,
        stream: fnp_socketproxy::FuchsiaNetworksRequestStream,
    ) -> Result<(), Error> {
        let _occupant = match self.fuchsia_occupant.try_lock() {
            Some(o) => o,
            None => {
                warn!("Only one connection to FuchsiaNetworks is allowed at a time");
                stream.control_handle().shutdown_with_epitaph(fidl::Status::ACCESS_DENIED);
                return Ok(());
            }
        };

        info!("Starting fuchsia.netpol.socketproxy.FuchsiaNetworks server");
        self.networks.fuchsia.lock().await.networks.as_mut().clear();
        stream
            .map(|result| result.context("failed request"))
            .try_for_each(|request| async {
                let mut network_registry = self.networks.fuchsia.lock().await;
                let (send, did_state_change): (
                    Box<dyn FnOnce() -> Result<(), _> + Send + Sync + 'static>,
                    bool,
                ) = handle_registry_request!(
                    FuchsiaNetworksRequest,
                    request,
                    network_registry,
                    "fuchsia"
                );
                std::mem::drop(network_registry);

                if did_state_change {
                    // We do feed here instead of send so that we don't wait for a flush
                    // in the event that the DnsServerWatcher is not running.
                    self.dns_tx
                        .clone()
                        .feed(self.networks.current_dns_servers().await)
                        .await
                        .unwrap_or_else(|e| {
                            if !e.is_disconnected() {
                                // Log if the feed fails for reasons other than disconnection.
                                error!("Unable to feed DNS update: {e:?}")
                            }
                        });

                    // Ensure the mark is updated prior to sending out the response
                    // and dropping the registry.
                    self.marks.lock().await.mark_1 = self.networks.current_mark().await;
                    send().context("error sending response")?;
                } else {
                    // Not a request involving a state change,
                    // so ignore any errors.
                    let _: Result<(), fidl::Error> = send();
                }
                Ok(())
            })
            .await
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use fuchsia_component::server::ServiceFs;
    use fuchsia_component_test::{
        Capability, ChildOptions, LocalComponentHandles, RealmBuilder, RealmInstance, Ref, Route,
    };
    use futures::channel::mpsc::Receiver;
    use net_declare::{fidl_ip, fidl_socket_addr};
    use pretty_assertions::assert_eq;
    use socket_proxy_testing::{RegistryType, ToDnsServerList as _, ToNetwork};
    use test_case::test_case;

    #[derive(Clone, Debug)]
    enum Op<N: ToNetwork> {
        SetDefault {
            network_id: Option<u32>,
            result: Result<(), fnp_socketproxy::NetworkRegistrySetDefaultError>,
        },
        Add {
            network: N,
            result: Result<(), fnp_socketproxy::NetworkRegistryAddError>,
        },
        Update {
            network: N,
            result: Result<(), fnp_socketproxy::NetworkRegistryUpdateError>,
        },
        Remove {
            network_id: u32,
            result: Result<(), fnp_socketproxy::NetworkRegistryRemoveError>,
        },
    }

    macro_rules! execute {
        ($self:ident, $proxy:ident, $registry:expr) => {{
            match $self {
                Op::SetDefault { network_id, result } => {
                    assert_eq!(
                        $proxy
                            .set_default(&match network_id {
                                Some(value) => fposix_socket::OptionalUint32::Value(*value),
                                None => fposix_socket::OptionalUint32::Unset(fposix_socket::Empty),
                            })
                            .await?,
                        *result
                    )
                }
                Op::Add { network, result } => {
                    assert_eq!($proxy.add(&network.to_network($registry)).await?, *result)
                }
                Op::Update { network, result } => {
                    assert_eq!($proxy.update(&network.to_network($registry)).await?, *result)
                }
                Op::Remove { network_id, result } => {
                    assert_eq!($proxy.remove(*network_id).await?, *result)
                }
            }
            Ok(())
        }};
    }

    impl<N: ToNetwork + Clone> Op<N> {
        async fn execute_starnix(
            &self,
            starnix: &fnp_socketproxy::StarnixNetworksProxy,
        ) -> Result<(), Error> {
            execute!(self, starnix, RegistryType::Starnix)
        }

        async fn execute_fuchsia(
            &self,
            fuchsia: &fnp_socketproxy::FuchsiaNetworksProxy,
        ) -> Result<(), Error> {
            execute!(self, fuchsia, RegistryType::Fuchsia)
        }

        fn is_err(&self) -> bool {
            match &self {
                Op::SetDefault { network_id: _, result } => result.is_err(),
                Op::Add { network: _, result } => result.is_err(),
                Op::Update { network: _, result } => result.is_err(),
                Op::Remove { network_id: _, result } => result.is_err(),
            }
        }
    }

    enum IncomingService {
        StarnixNetworks(fnp_socketproxy::StarnixNetworksRequestStream),
        FuchsiaNetworks(fnp_socketproxy::FuchsiaNetworksRequestStream),
    }

    async fn run_registry(
        handles: LocalComponentHandles,
        starnix_networks: Arc<Mutex<NetworkRegistry>>,
        fuchsia_networks: Arc<Mutex<NetworkRegistry>>,
        marks: Arc<Mutex<crate::SocketMarks>>,
        dns_tx: mpsc::Sender<Vec<fnp_socketproxy::DnsServerList>>,
    ) -> Result<(), Error> {
        let mut fs = ServiceFs::new();
        let _ = fs
            .dir("svc")
            .add_fidl_service(IncomingService::StarnixNetworks)
            .add_fidl_service(IncomingService::FuchsiaNetworks);
        let _ = fs.serve_connection(handles.outgoing_dir)?;

        let registry = Registry {
            networks: NetworkRegistries { starnix: starnix_networks, fuchsia: fuchsia_networks },
            marks,
            dns_tx,
            starnix_occupant: Default::default(),
            fuchsia_occupant: Default::default(),
        };

        fs.for_each_concurrent(0, |service| async {
            match service {
                IncomingService::StarnixNetworks(stream) => registry.run_starnix(stream).await,
                IncomingService::FuchsiaNetworks(stream) => registry.run_fuchsia(stream).await,
            }
            .unwrap_or_else(|e| error!("{e:?}"))
        })
        .await;

        Ok(())
    }

    async fn setup_test(
    ) -> Result<(RealmInstance, Receiver<Vec<fnp_socketproxy::DnsServerList>>), Error> {
        let builder = RealmBuilder::new().await?;
        let starnix_networks = Arc::new(Mutex::new(Default::default()));
        let fuchsia_networks = Arc::new(Mutex::new(Default::default()));
        let (dns_tx, dns_rx) = mpsc::channel(1);
        let marks = Arc::new(Mutex::new(crate::SocketMarks::default()));
        let registry = builder
            .add_local_child(
                "registry",
                {
                    let starnix_networks = starnix_networks.clone();
                    let fuchsia_networks = fuchsia_networks.clone();
                    let marks = marks.clone();
                    let dns_tx = dns_tx.clone();
                    move |handles: LocalComponentHandles| {
                        Box::pin(run_registry(
                            handles,
                            starnix_networks.clone(),
                            fuchsia_networks.clone(),
                            marks.clone(),
                            dns_tx.clone(),
                        ))
                    }
                },
                ChildOptions::new(),
            )
            .await?;

        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol::<fnp_socketproxy::StarnixNetworksMarker>())
                    .from(&registry)
                    .to(Ref::parent()),
            )
            .await?;

        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol::<fnp_socketproxy::FuchsiaNetworksMarker>())
                    .from(&registry)
                    .to(Ref::parent()),
            )
            .await?;

        let realm = builder.build().await?;

        Ok((realm, dns_rx))
    }

    #[test_case(&[
        Op::Add { network: 1, result: Ok(()) },
        Op::Update { network: 1, result: Ok(()) },
        Op::Remove { network_id: 1, result: Ok(()) },
    ]; "normal operation")]
    #[test_case(&[
        Op::Add { network: 1, result: Ok(()) },
        Op::Add { network: 1, result: Err(fnp_socketproxy::NetworkRegistryAddError::DuplicateNetworkId) },
    ]; "duplicate add")]
    #[test_case(&[
        Op::Update { network: 1, result: Err(fnp_socketproxy::NetworkRegistryUpdateError::NotFound) },
    ]; "update missing")]
    #[test_case(&[
        Op::<u32>::Remove { network_id: 1, result: Err(fnp_socketproxy::NetworkRegistryRemoveError::NotFound) },
    ]; "remove missing")]
    #[test_case(&[
        Op::<u32>::SetDefault { network_id: Some(1), result: Err(fnp_socketproxy::NetworkRegistrySetDefaultError::NotFound) },
    ]; "set default missing")]
    #[test_case(&[
        Op::Add { network: 1, result: Ok(()) },
        Op::SetDefault { network_id: Some(1), result: Ok(()) },
        Op::Remove { network_id: 1, result: Err(fnp_socketproxy::NetworkRegistryRemoveError::CannotRemoveDefaultNetwork)},
    ]; "remove default network")]
    #[test_case(&[
        Op::Add { network: 1, result: Ok(()) },
        Op::SetDefault { network_id: Some(1), result: Ok(()) },
        Op::Remove { network_id: 1, result: Err(fnp_socketproxy::NetworkRegistryRemoveError::CannotRemoveDefaultNetwork)},
        Op::Add { network: 2, result: Ok(()) },
        Op::SetDefault { network_id: Some(2), result: Ok(()) },
        Op::Remove { network_id: 1, result: Ok(()) },
    ]; "remove formerly default network")]
    #[test_case(&[
        Op::Add { network: 1, result: Ok(()) },
        Op::SetDefault { network_id: Some(1), result: Ok(()) },
        Op::Remove { network_id: 1, result: Err(fnp_socketproxy::NetworkRegistryRemoveError::CannotRemoveDefaultNetwork)},
        Op::SetDefault { network_id: None, result: Ok(()) },
        Op::Remove { network_id: 1, result: Ok(()) },
    ]; "remove last network")]
    #[test_case(&[
        Op::Add { network: 1, result: Ok(()) },
        Op::Update { network: 1, result: Ok(()) },
        Op::Add { network: 2, result: Ok(()) },
        Op::Add { network: 3, result: Ok(()) },
        Op::Add { network: 4, result: Ok(()) },
        Op::Update { network: 4, result: Ok(()) },
        Op::Update { network: 2, result: Ok(()) },
        Op::Update { network: 3, result: Ok(()) },
        Op::Add { network: 5, result: Ok(()) },
        Op::Update { network: 5, result: Ok(()) },
        Op::Add { network: 6, result: Ok(()) },
        Op::Add { network: 7, result: Ok(()) },
        Op::Add { network: 8, result: Ok(()) },
        Op::Update { network: 8, result: Ok(()) },
        Op::Update { network: 6, result: Ok(()) },
        Op::Add { network: 9, result: Ok(()) },
        Op::Update { network: 9, result: Ok(()) },
        Op::Update { network: 7, result: Ok(()) },
        Op::Add { network: 10, result: Ok(()) },
        Op::Update { network: 10, result: Ok(()) },
    ]; "many updates")]
    #[fuchsia::test]
    async fn test_operations<N: ToNetwork + Clone>(operations: &[Op<N>]) -> Result<(), Error> {
        let (realm, _) = setup_test().await?;
        let starnix_networks = realm
            .root
            .connect_to_protocol_at_exposed_dir::<fnp_socketproxy::StarnixNetworksMarker>()
            .context("While connecting to StarnixNetworks")?;
        let fuchsia_networks = realm
            .root
            .connect_to_protocol_at_exposed_dir::<fnp_socketproxy::FuchsiaNetworksMarker>()
            .context("While connecting to FuchsiaNetworks")?;

        for op in operations {
            // Demonstrate that the same operations can be applied
            // independently in both registries.
            op.execute_starnix(&starnix_networks).await?;
            op.execute_fuchsia(&fuchsia_networks).await?;
        }

        Ok(())
    }

    #[test_case(&[
        Op::Add { network: (1, vec![fidl_ip!("192.0.2.0")]), result: Ok(()) },
    ], vec![(1, vec![fidl_socket_addr!("192.0.2.0:53")]).to_dns_server_list()]
    ; "normal operation (v4)")]
    #[test_case(&[
        Op::Add { network: (1, vec![fidl_ip!("192.0.2.0")]), result: Ok(()) },
        Op::Update { network: (1, vec![fidl_ip!("192.0.2.1")]), result: Ok(()) },
    ], vec![(1, vec![fidl_socket_addr!("192.0.2.1:53")]).to_dns_server_list()]
    ; "update server list (v4)")]
    #[test_case(&[
        Op::Add { network: (1, vec![fidl_ip!("2001:db8::1")]), result: Ok(()) },
    ], vec![(1, vec![fidl_socket_addr!("[2001:db8::1]:53")]).to_dns_server_list()]
    ; "normal operation (v6)")]
    #[test_case(&[
        Op::Add { network: (1, vec![fidl_ip!("2001:db8::1")]), result: Ok(()) },
        Op::Update { network: (1, vec![fidl_ip!("2001:db8::2")]), result: Ok(()) },
    ], vec![(1, vec![fidl_socket_addr!("[2001:db8::2]:53")]).to_dns_server_list()]
    ; "update server list (v6)")]
    #[test_case(&[
        Op::Add { network: (1, vec![fidl_ip!("192.0.2.0"), fidl_ip!("2001:db8::1")]), result: Ok(()) },
    ], vec![(1, vec![fidl_socket_addr!("192.0.2.0:53"), fidl_socket_addr!("[2001:db8::1]:53")]).to_dns_server_list()]
    ; "normal operation (mixed)")]
    #[test_case(&[
        Op::Add { network: (1, vec![fidl_ip!("192.0.2.0"), fidl_ip!("2001:db8::1")]), result: Ok(()) },
        Op::Update { network: (1, vec![fidl_ip!("192.0.2.1"), fidl_ip!("2001:db8::2")]), result: Ok(()) },
    ], vec![(1, vec![fidl_socket_addr!("192.0.2.1:53"), fidl_socket_addr!("[2001:db8::2]:53")]).to_dns_server_list()]
    ; "update server list (mixed)")]
    #[test_case(&[
        Op::Add { network: (1, vec![fidl_ip!("192.0.2.0"), fidl_ip!("2001:db8::1")]), result: Ok(()) },
        Op::Add { network: (2, vec![fidl_ip!("192.0.2.1"), fidl_ip!("2001:db8::2")]), result: Ok(()) },
        Op::Add { network: (3, vec![fidl_ip!("192.0.2.2"), fidl_ip!("2001:db8::3")]), result: Ok(()) },
    ], vec![
        (1, vec![fidl_socket_addr!("192.0.2.0:53"), fidl_socket_addr!("[2001:db8::1]:53")]).to_dns_server_list(),
        (2, vec![fidl_socket_addr!("192.0.2.1:53"), fidl_socket_addr!("[2001:db8::2]:53")]).to_dns_server_list(),
        (3, vec![fidl_socket_addr!("192.0.2.2:53"), fidl_socket_addr!("[2001:db8::3]:53")]).to_dns_server_list(),
    ]; "multiple networks")]
    #[fuchsia::test]
    async fn test_dns_tracking<N: ToNetwork + Clone>(
        operations: &[Op<N>],
        dns_servers: Vec<fnp_socketproxy::DnsServerList>,
    ) -> Result<(), Error> {
        let (realm, mut dns_rx) = setup_test().await?;
        let starnix_networks = realm
            .root
            .connect_to_protocol_at_exposed_dir::<fnp_socketproxy::StarnixNetworksMarker>()
            .context("While connecting to StarnixNetworks")?;

        let mut last_dns = None;
        for op in operations {
            // Starnix and Fuchsia registries have the same handling logic, so
            // use the Starnix registry to confirm this behavior.
            op.execute_starnix(&starnix_networks).await?;
            last_dns = Some(dns_rx.next().await.expect("dns update expected after each operation"));
        }

        let mut last_dns = last_dns.expect("there should be at least one dns update");
        last_dns.sort_by_key(|a| a.source_network_id);
        assert_eq!(last_dns, dns_servers);

        Ok(())
    }

    #[test_case(&[
        (RegistryType::Fuchsia, Op::Add { network: (1, vec![fidl_ip!("192.0.2.0")]), result: Ok(()) }),
        (RegistryType::Fuchsia, Op::SetDefault { network_id: Some(1), result: Ok(()) }),
    ], vec![(1, vec![fidl_socket_addr!("192.0.2.0:53")]).to_dns_server_list()]
    ; "normal operation Fuchsia (v4)")]
    #[test_case(&[
        (RegistryType::Fuchsia, Op::Add { network: (1, vec![fidl_ip!("2001:db8::1")]), result: Ok(()) }),
        (RegistryType::Fuchsia, Op::SetDefault { network_id: Some(1), result: Ok(()) }),
    ], vec![(1, vec![fidl_socket_addr!("[2001:db8::1]:53")]).to_dns_server_list()]
    ; "normal operation Fuchsia (v6)")]
    #[test_case(&[
        (RegistryType::Starnix, Op::Add { network: (1, vec![fidl_ip!("192.0.2.0")]), result: Ok(()) }),
        (RegistryType::Fuchsia, Op::Remove { network_id: 1, result: Err(fnp_socketproxy::NetworkRegistryRemoveError::NotFound) }),
    ], vec![(1, vec![fidl_socket_addr!("192.0.2.0:53")]).to_dns_server_list()]
    ; "attempt remove in wrong registry")]
    #[test_case(&[
        (RegistryType::Starnix, Op::Add { network: (1, vec![fidl_ip!("192.0.2.0"), fidl_ip!("2001:db8::1")]), result: Ok(()) }),
        (RegistryType::Fuchsia, Op::Add { network: (2, vec![fidl_ip!("192.0.2.1"), fidl_ip!("2001:db8::2")]), result: Ok(()) }),
    ], vec![
        (1, vec![fidl_socket_addr!("192.0.2.0:53"), fidl_socket_addr!("[2001:db8::1]:53")]).to_dns_server_list(),
    ]; "Fuchsia default absent, use Starnix")]
    #[test_case(&[
        (RegistryType::Starnix, Op::Add { network: (1, vec![fidl_ip!("192.0.2.0"), fidl_ip!("2001:db8::1")]), result: Ok(()) }),
        (RegistryType::Fuchsia, Op::Add { network: (2, vec![fidl_ip!("192.0.2.1"), fidl_ip!("2001:db8::2")]), result: Ok(()) }),
        (RegistryType::Fuchsia, Op::SetDefault { network_id: Some(2), result: Ok(()) }),
        ], vec![
        (2, vec![fidl_socket_addr!("192.0.2.1:53"), fidl_socket_addr!("[2001:db8::2]:53")]).to_dns_server_list(),
    ]; "Fuchsia default present, use Fuchsia")]
    #[test_case(&[
        (RegistryType::Starnix, Op::Add { network: (1, vec![fidl_ip!("192.0.2.0"), fidl_ip!("2001:db8::1")]), result: Ok(()) }),
        (RegistryType::Fuchsia, Op::Add { network: (2, vec![fidl_ip!("192.0.2.1"), fidl_ip!("2001:db8::2")]), result: Ok(()) }),
        (RegistryType::Fuchsia, Op::SetDefault { network_id: Some(2), result: Ok(()) }),
        (RegistryType::Fuchsia, Op::SetDefault { network_id: None, result: Ok(()) }),
        ], vec![
        (1, vec![fidl_socket_addr!("192.0.2.0:53"), fidl_socket_addr!("[2001:db8::1]:53")]).to_dns_server_list(),
    ]; "Fallback to Starnix network")]
    #[test_case(&[
        (RegistryType::Starnix, Op::Add { network: (1, vec![fidl_ip!("192.0.2.0"), fidl_ip!("2001:db8::1")]), result: Ok(()) }),
        (RegistryType::Fuchsia, Op::Add { network: (2, vec![fidl_ip!("192.0.2.1"), fidl_ip!("2001:db8::2")]), result: Ok(()) }),
        (RegistryType::Fuchsia, Op::SetDefault { network_id: Some(2), result: Ok(()) }),
        (RegistryType::Fuchsia, Op::Update { network: (2, vec![fidl_ip!("192.0.2.2"), fidl_ip!("2001:db8::3")]), result: Ok(()) }),
        ], vec![
        (2, vec![fidl_socket_addr!("192.0.2.2:53"), fidl_socket_addr!("[2001:db8::3]:53")]).to_dns_server_list(),
    ]; "Fuchsia default present then updated")]
    #[fuchsia::test]
    async fn test_dns_tracking_across_registries<N: ToNetwork + Clone>(
        operations: &[(RegistryType, Op<N>)],
        dns_servers: Vec<fnp_socketproxy::DnsServerList>,
    ) -> Result<(), Error> {
        let (realm, mut dns_rx) = setup_test().await?;
        let starnix_networks = realm
            .root
            .connect_to_protocol_at_exposed_dir::<fnp_socketproxy::StarnixNetworksMarker>()
            .context("While connecting to StarnixNetworks")?;
        let fuchsia_networks = realm
            .root
            .connect_to_protocol_at_exposed_dir::<fnp_socketproxy::FuchsiaNetworksMarker>()
            .context("While connecting to FuchsiaNetworks")?;

        let mut last_dns = None;
        for (registry, op) in operations {
            match registry {
                RegistryType::Starnix => {
                    op.execute_starnix(&starnix_networks).await?;
                }
                RegistryType::Fuchsia => {
                    op.execute_fuchsia(&fuchsia_networks).await?;
                }
            }
            // When the operation results in an error, we don't expect that to
            // result in an additional DNS update.
            if !op.is_err() {
                last_dns =
                    Some(dns_rx.next().await.expect("dns update expected after each operation"));
            }
        }

        let mut last_dns = last_dns.expect("there should be at least one dns update");
        last_dns.sort_by_key(|a| a.source_network_id);
        assert_eq!(last_dns, dns_servers);

        Ok(())
    }
}
