// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::InterfaceId;
use anyhow::Context as _;
use async_utils::stream::{Tagged, WithTag as _};
use dns_server_watcher::DnsServers;
use fidl::endpoints::{ControlHandle as _, Responder as _};
use fidl::HandleBased as _;
use log::{error, info, warn};
use std::collections::HashMap;

mod token_map;

use {
    fidl_fuchsia_net as fnet, fidl_fuchsia_net_name as fnet_name,
    fidl_fuchsia_net_policy_properties as fnp_properties,
};

pub trait NetworkTokenExt: Sized {
    fn duplicate(&self) -> Result<Self, zx::Status>;
}

impl NetworkTokenExt for fnp_properties::NetworkToken {
    fn duplicate(&self) -> Result<fnp_properties::NetworkToken, zx::Status> {
        Ok(fnp_properties::NetworkToken {
            value: Some(
                self.value
                    .as_ref()
                    .ok_or(zx::Status::NOT_FOUND)?
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)?,
            ),
            ..Default::default()
        })
    }
}

pub(crate) struct NetworkTokenContents {
    connection_id: ConnectionId,
    interface_id: InterfaceId,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ConnectionId(usize);

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct UpdateGeneration {
    /// The current generation for `fuchsia.net.policy.properties.WatchDefault`.
    /// Incremented each time the default network changes.
    default_network: usize,

    /// The current generation for `fuchsia.net.policy.properties.WatchProperties`.
    /// Incremented each time a network property changes.
    properties: usize,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct UpdateGenerations(HashMap<ConnectionId, UpdateGeneration>);

impl UpdateGenerations {
    fn default_network(&self, id: &ConnectionId) -> Option<usize> {
        self.0.get(id).map(|g| g.default_network)
    }

    fn set_default_network(&mut self, id: ConnectionId, generation: UpdateGeneration) {
        self.0.entry(id).or_default().default_network = generation.default_network;
    }

    fn properties(&self, id: &ConnectionId) -> Option<usize> {
        self.0.get(id).map(|g| g.properties)
    }

    fn set_properties(&mut self, id: ConnectionId, generation: UpdateGeneration) {
        self.0.entry(id).or_default().properties = generation.properties;
    }

    fn remove(&mut self, id: &ConnectionId) -> Option<UpdateGeneration> {
        self.0.remove(id)
    }
}

#[derive(Debug)]
pub(crate) struct NetworkPropertyResponder {
    token: fidl::EventPair,
    watched_properties: Vec<fnp_properties::Property>,
    responder: fnp_properties::NetworksWatchPropertiesResponder,
}

#[derive(Default)]
pub(crate) struct NetpolNetworksService {
    // The current generation
    current_generation: UpdateGeneration,
    // The last generation sent per connection
    generations_by_connection: UpdateGenerations,
    // Default Network Watchers
    default_network_responders:
        HashMap<ConnectionId, fnp_properties::NetworksWatchDefaultResponder>,
    tokens: token_map::TokenMap<NetworkTokenContents>,
    // NetworkProperty Watchers
    property_responders: HashMap<ConnectionId, NetworkPropertyResponder>,
    // Network properties
    network_properties: NetworkProperties,
}

#[derive(Default, Clone)]
struct NetworkProperties {
    // Current default network
    default_network: Option<InterfaceId>,
    // Network marks
    socket_marks: HashMap<InterfaceId, fnet::Marks>,
    // DNS Servers
    dns_servers: Vec<fnet_name::DnsServer_>,
}

impl NetworkProperties {
    fn apply(&mut self, update: PropertyUpdate) -> UpdatesApplied {
        let mut updates = UpdatesApplied::default();

        if let Some(new_default_network) = update.default_network {
            updates.default_network = self.handle_default_network_update(new_default_network);
        }

        if let Some((netid, marks)) = update.socket_marks {
            updates.socket_marks_network = self.handle_socket_marks_update(netid, marks);
        }

        if let Some(dns) = update.dns {
            updates.dns_changed = self.dns_servers != dns;
            self.dns_servers = dns;
        }

        updates
    }

    // Handle the `default_network` argument in a `PropertyUpdate`, determining
    // whether the network changed as a result of the update.
    //
    // Returns the update to set for the `default_network` argument
    // of UpdatesApplied.
    fn handle_default_network_update(
        &mut self,
        new_default_network: Option<InterfaceId>,
    ) -> Option<Option<InterfaceId>> {
        // We do not need to send an update applied if the network stayed the same.
        if new_default_network == self.default_network {
            return None;
        }

        let old_default_network = self.default_network;
        if let (None, Some(old_default_network_id)) = (new_default_network, old_default_network) {
            let _: Option<_> = self.socket_marks.remove(&old_default_network_id);
        }
        self.default_network = new_default_network;
        return Some(old_default_network);
    }

    // Handle the `socket_marks` argument in a `PropertyUpdate`, determining
    // whether the socket marks changed as a result of the update.
    //
    // Returns the update to set for the `socket_marks_network` argument
    // of UpdatesApplied.
    fn handle_socket_marks_update(
        &mut self,
        netid: InterfaceId,
        marks: fnet::Marks,
    ) -> Option<InterfaceId> {
        // We do not need to send an update applied if the marks for the
        // provided netid stay the same.
        if self.socket_marks.contains_key(&netid)
            && self
                .socket_marks
                .get(&netid)
                .and_then(|old_marks| Some(*old_marks == marks))
                .unwrap_or_default()
        {
            return None;
        }

        let _: Option<_> = self.socket_marks.insert(netid, marks);
        return Some(netid);
    }

    fn maybe_respond(
        &self,
        network: &NetworkTokenContents,
        responder: NetworkPropertyResponder,
    ) -> Option<NetworkPropertyResponder> {
        let mut updates = Vec::new();
        updates.add_socket_marks(self, network, &responder);
        updates.add_dns(self, network, &responder);

        if updates.is_empty() {
            Some(responder)
        } else {
            if let Err(e) = responder.responder.send(Ok(&updates)) {
                warn!("Could not send to responder: {e}");
            }
            None
        }
    }
}

trait PropertyUpdates {
    fn add_socket_marks(
        &mut self,
        properties: &NetworkProperties,
        network: &NetworkTokenContents,
        responder: &NetworkPropertyResponder,
    );
    fn add_dns(
        &mut self,
        properties: &NetworkProperties,
        network: &NetworkTokenContents,
        responder: &NetworkPropertyResponder,
    );
}

impl PropertyUpdates for Vec<fnp_properties::PropertyUpdate> {
    fn add_socket_marks(
        &mut self,
        properties: &NetworkProperties,
        network: &NetworkTokenContents,
        responder: &NetworkPropertyResponder,
    ) {
        if !responder.watched_properties.contains(&fnp_properties::Property::SocketMarks) {
            return;
        }
        match properties.socket_marks.get(&network.interface_id) {
            Some(marks) => self.push(fnp_properties::PropertyUpdate::SocketMarks(marks.clone())),
            None => {}
        }
    }

    fn add_dns(
        &mut self,
        properties: &NetworkProperties,
        network: &NetworkTokenContents,
        responder: &NetworkPropertyResponder,
    ) {
        if !responder.watched_properties.contains(&fnp_properties::Property::DnsConfiguration) {
            return;
        }

        let interface_id = network.interface_id;
        self.push(fnp_properties::PropertyUpdate::DnsConfiguration(
            fnp_properties::DnsConfiguration {
                servers: Some(
                    properties
                        .dns_servers
                        .iter()
                        .filter(|d| {
                            match &d.source {
                                Some(source) => match source {
                                    fnet_name::DnsServerSource::StaticSource(_) => true,
                                    fnet_name::DnsServerSource::SocketProxy(
                                        fnet_name::SocketProxyDnsServerSource {
                                            source_interface,
                                            ..
                                        },
                                    )
                                    | fnet_name::DnsServerSource::Dhcp(
                                        fnet_name::DhcpDnsServerSource { source_interface, .. },
                                    )
                                    | fnet_name::DnsServerSource::Ndp(
                                        fnet_name::NdpDnsServerSource { source_interface, .. },
                                    )
                                    | fnet_name::DnsServerSource::Dhcpv6(
                                        fnet_name::Dhcpv6DnsServerSource {
                                            source_interface, ..
                                        },
                                    ) => match (interface_id, source_interface) {
                                        (_, None) => true,
                                        (id1, Some(id2)) => id1.get() == *id2,
                                    },

                                    _ => {
                                        error!("unhandled DnsServerSource: {source:?}");
                                        false
                                    }
                                },

                                // No source, assume static source, so include it.
                                None => true,
                            }
                        })
                        .cloned()
                        .collect::<Vec<_>>(),
                ),
                ..Default::default()
            },
        ));
    }
}

#[derive(Default)]
pub(crate) struct PropertyUpdate {
    default_network: Option<Option<InterfaceId>>,
    socket_marks: Option<(InterfaceId, fnet::Marks)>,
    dns: Option<Vec<fnet_name::DnsServer_>>,
}

#[derive(Default, Debug)]
struct UpdatesApplied {
    // If Some, contains old network id
    default_network: Option<Option<InterfaceId>>,

    // If Some, contains the network ID for which the mark was set
    socket_marks_network: Option<InterfaceId>,

    // Was the DNS changed
    dns_changed: bool,
}

impl PropertyUpdate {
    pub fn default_network<N: TryInto<InterfaceId>>(
        mut self,
        network_id: N,
    ) -> Result<Self, N::Error> {
        self.default_network = Some(Some(network_id.try_into()?));
        Ok(self)
    }

    pub fn default_network_lost(mut self) -> Self {
        self.default_network = Some(None);
        self
    }

    pub fn socket_marks<N: TryInto<InterfaceId>, Marks: Into<fnet::Marks>>(
        mut self,
        network_id: N,
        marks: Marks,
    ) -> Result<Self, N::Error> {
        self.socket_marks = Some((network_id.try_into()?, marks.into()));
        Ok(self)
    }

    pub fn dns(mut self, dns_servers: &DnsServers) -> Self {
        self.dns = Some(dns_servers.consolidated_dns_servers());
        self
    }
}

impl NetpolNetworksService {
    pub(crate) async fn handle_network_attributes_request(
        &mut self,
        id: ConnectionId,
        req: Result<fnp_properties::NetworksRequest, fidl::Error>,
    ) -> Result<(), anyhow::Error> {
        let req = req.context("network attributes request")?;
        match req {
            fnp_properties::NetworksRequest::WatchDefault { responder } => {
                match self.default_network_responders.entry(id) {
                    std::collections::hash_map::Entry::Occupied(_) => {
                        warn!(
                            "Only one call to fuchsia.net.policy.properties/Networks.WatchDefault \
                             may be active per connection"
                        );
                        responder
                            .control_handle()
                            .shutdown_with_epitaph(zx::Status::CONNECTION_ABORTED)
                    }
                    std::collections::hash_map::Entry::Vacant(vacant_entry) => {
                        let interface_id = if self
                            .generations_by_connection
                            .default_network(&id)
                            .unwrap_or_default()
                            < self.current_generation.default_network
                        {
                            match self.network_properties.default_network {
                                Some(interface_id) => Some(interface_id),
                                None => None,
                            }
                        } else {
                            None
                        };
                        if let Some(interface_id) = interface_id {
                            self.generations_by_connection
                                .set_default_network(id, self.current_generation);
                            let token = self
                                .tokens
                                .insert_data(NetworkTokenContents {
                                    connection_id: id,
                                    interface_id,
                                })
                                .await;
                            responder.send(
                                fnp_properties::NetworksWatchDefaultResponse::Network(
                                    fnp_properties::NetworkToken {
                                        value: Some(token),
                                        ..Default::default()
                                    },
                                ),
                            )?;

                            if let Some(responder) = self.property_responders.remove(&id) {
                                let _ = self.generations_by_connection.remove(&id);
                                let _ = responder
                                    .responder
                                    .send(Err(fnp_properties::WatchError::DefaultNetworkChanged));
                            }
                        } else {
                            let _ = vacant_entry.insert(responder);
                        }
                    }
                }
            }
            fnp_properties::NetworksRequest::WatchProperties {
                payload: fnp_properties::NetworksWatchPropertiesRequest { network, properties, .. },
                responder,
            } => match (network, properties) {
                (None, _) | (_, None) => {
                    responder.send(Err(fnp_properties::WatchError::MissingRequiredArgument))?
                }
                (Some(network), Some(properties)) => {
                    if properties.is_empty() {
                        responder.send(Err(fnp_properties::WatchError::NoProperties))?;
                    } else if let Some(token) = network.value {
                        match self.property_responders.entry(id) {
                            std::collections::hash_map::Entry::Occupied(_) => {
                                warn!(
                            "Only one call to fuchsia.net.policy.properties/Networks.WatchProperties \
                             may be active per connection"
                        );
                                responder
                                    .control_handle()
                                    .shutdown_with_epitaph(zx::Status::CONNECTION_ABORTED)
                            }
                            std::collections::hash_map::Entry::Vacant(vacant_entry) => {
                                match self.tokens.get(&token).await {
                                    None => {
                                        warn!("Unknown network token. ({token:?}");
                                        responder.send(Err(
                                            fnp_properties::WatchError::InvalidNetworkToken,
                                        ))?;
                                    }
                                    Some(network_contents) => {
                                        if network_contents.connection_id != id {
                                            warn!(
                                            "Cannot watch a NetworkToken that was not created by \
                                             this connection."
                                        );
                                            responder.send(Err(
                                                fnp_properties::WatchError::InvalidNetworkToken,
                                            ))?;
                                        } else {
                                            let responder = NetworkPropertyResponder {
                                                token,
                                                watched_properties: properties,
                                                responder,
                                            };
                                            if self
                                                .generations_by_connection
                                                .properties(&id)
                                                .unwrap_or_default()
                                                < self.current_generation.properties
                                            {
                                                self.generations_by_connection
                                                    .set_properties(id, self.current_generation);
                                                if let Some(responder) = self
                                                    .network_properties
                                                    .maybe_respond(&network_contents, responder)
                                                {
                                                    let _: &mut NetworkPropertyResponder =
                                                        vacant_entry.insert(responder);
                                                }
                                            } else {
                                                let _: &mut NetworkPropertyResponder =
                                                    vacant_entry.insert(responder);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    } else {
                        responder.send(Err(fnp_properties::WatchError::InvalidNetworkToken))?;
                    }
                }
            },
            _ => {
                warn!("Received unexpected request {req:?}");
            }
        }

        Ok(())
    }

    async fn changed_default_network(
        error: fnp_properties::WatchError,
        responders: &mut HashMap<ConnectionId, NetworkPropertyResponder>,
        generations: &mut UpdateGenerations,
    ) {
        let mut r = HashMap::new();
        std::mem::swap(&mut r, responders);
        r = r
            .into_iter()
            .filter_map(|(id, responder)| {
                // NB: Currently all responders are for the default network.
                let _ = generations.remove(&id);
                let _ = responder.responder.send(Err(error));
                None
            })
            .collect::<HashMap<_, _>>();
        std::mem::swap(&mut r, responders);
    }

    pub(crate) async fn remove_network<ID: Into<InterfaceId>>(&mut self, interface_id: ID) {
        let interface_id = interface_id.into();
        info!("Removing interface {interface_id}. Reporting NETWORK_GONE to all clients.");
        let mut responders = HashMap::new();
        std::mem::swap(&mut self.property_responders, &mut responders);
        for (id, responder) in responders {
            let network = match self.tokens.get(&responder.token).await {
                Some(network) => network,
                None => {
                    warn!("Could not fetch network data for responder");
                    continue;
                }
            };
            if network.interface_id == interface_id {
                // Report that this interface was removed
                if let Err(e) =
                    responder.responder.send(Err(fnp_properties::WatchError::NetworkGone))
                {
                    warn!("Could not send to responder: {e}");
                }
            } else {
                if self.property_responders.insert(id, responder).is_some() {
                    error!("Re-inserted in an existing responder slot. This should be impossible.");
                }
            }
        }
    }

    pub(crate) async fn update(&mut self, update: PropertyUpdate) {
        self.current_generation.properties += 1;
        let updates_applied = self.network_properties.apply(update);
        let mut property_responders = HashMap::new();
        std::mem::swap(&mut self.property_responders, &mut property_responders);

        if updates_applied.default_network.is_some() {
            if let Some(default_network) = self.network_properties.default_network {
                self.current_generation.default_network += 1;
                let mut responders = HashMap::new();
                std::mem::swap(&mut self.default_network_responders, &mut responders);
                for (id, responder) in responders {
                    self.generations_by_connection.set_default_network(id, self.current_generation);

                    let token = self
                        .tokens
                        .insert_data(NetworkTokenContents {
                            connection_id: id,
                            interface_id: default_network,
                        })
                        .await;

                    if let Err(e) =
                        responder.send(fnp_properties::NetworksWatchDefaultResponse::Network(
                            fnp_properties::NetworkToken {
                                value: Some(token),
                                ..Default::default()
                            },
                        ))
                    {
                        warn!("Could not send to responder: {e}");
                    }
                }

                NetpolNetworksService::changed_default_network(
                    fnp_properties::WatchError::DefaultNetworkChanged,
                    &mut property_responders,
                    &mut self.generations_by_connection,
                )
                .await;
            } else {
                // The default network has been lost.
                self.current_generation.default_network += 1;
                let mut responders = HashMap::new();
                std::mem::swap(&mut self.default_network_responders, &mut responders);
                for (id, responder) in responders {
                    self.generations_by_connection.set_default_network(id, self.current_generation);
                    if let Err(e) = responder.send(
                        fnp_properties::NetworksWatchDefaultResponse::NoDefaultNetwork(
                            fnp_properties::Empty,
                        ),
                    ) {
                        warn!("Could not send to responder: {e}");
                    }
                }

                NetpolNetworksService::changed_default_network(
                    fnp_properties::WatchError::DefaultNetworkLost,
                    &mut property_responders,
                    &mut self.generations_by_connection,
                )
                .await;
            }
        }

        for (id, responder) in property_responders {
            let mut updates = Vec::new();
            let network = match self.tokens.get(&responder.token).await {
                Some(network) => network,
                None => {
                    warn!("Could not fetch network data for responder");
                    continue;
                }
            };

            if let Some(network_id) = updates_applied.socket_marks_network {
                if network.interface_id == network_id {
                    updates.add_socket_marks(&self.network_properties, &network, &responder);
                }
            }
            if updates_applied.dns_changed {
                updates.add_dns(&self.network_properties, &network, &responder);
            }

            self.generations_by_connection.set_properties(id, self.current_generation);
            if updates.is_empty() {
                if self.property_responders.insert(id, responder).is_some() {
                    warn!("Re-inserted in an existing responder slot. This should be impossible.");
                }
            } else {
                if let Err(e) = responder.responder.send(Ok(&updates)) {
                    warn!("Could not send to responder: {e}");
                }
            }
        }
    }
}

#[derive(Default)]
pub(crate) struct NetworksRequestStreams {
    next_id: ConnectionId,
    request_streams:
        futures::stream::SelectAll<Tagged<ConnectionId, fnp_properties::NetworksRequestStream>>,
}

impl NetworksRequestStreams {
    pub fn push(&mut self, stream: fnp_properties::NetworksRequestStream) {
        self.request_streams.push(stream.tagged(self.next_id));
        self.next_id.0 += 1;
    }
}

impl futures::Stream for NetworksRequestStreams {
    type Item = (ConnectionId, Result<fnp_properties::NetworksRequest, fidl::Error>);

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::pin::Pin::new(&mut self.request_streams).poll_next(cx)
    }
}

impl futures::stream::FusedStream for NetworksRequestStreams {
    fn is_terminated(&self) -> bool {
        self.request_streams.is_terminated()
    }
}
