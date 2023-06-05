// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A module for managing RTM_ROUTE information by receiving RTM_ROUTE
//! Netlink messages and maintaining route table state from Netstack.

use std::{
    collections::HashSet,
    hash::{Hash, Hasher},
};

use anyhow::{anyhow, Context as _};
use fidl_fuchsia_net_routes_ext as fnet_routes_ext;
use futures::{pin_mut, select, StreamExt as _, TryStreamExt as _};
use net_types::ip::{Ip, IpAddress, IpVersion};
use netlink_packet_core::NetlinkMessage;
use netlink_packet_route::{
    RouteHeader, RouteMessage, RtnlMessage, AF_INET, AF_INET6, RTNLGRP_IPV4_ROUTE,
    RTNLGRP_IPV6_ROUTE, RTN_UNICAST, RTPROT_UNSPEC, RT_SCOPE_UNIVERSE, RT_TABLE_UNSPEC,
};
use netlink_packet_utils::nla::Nla;
use tracing::{error, warn};

use crate::{
    client::ClientTable,
    messaging::Sender,
    multicast_groups::ModernGroup,
    protocol_family::{route::NetlinkRoute, ProtocolFamily},
};

use crate::NETLINK_LOG_TAG;

/// Contains the asynchronous work related to RTM_ROUTE messages.
///
/// Connects to the route watcher and can respond to RTM_ROUTE
/// message requests.
pub(crate) struct EventLoop<S: Sender<<NetlinkRoute as ProtocolFamily>::Message>> {
    /// Represents the current route state as observed by Netstack, converted into
    /// Netlink messages to send when requested.
    route_messages: HashSet<NetlinkRouteMessage>,
    /// The current set of clients of NETLINK_ROUTE protocol family.
    route_clients: ClientTable<NetlinkRoute, S>,
}

/// RTM_ROUTE related event loop errors.
#[derive(Debug)]
pub(crate) enum RoutesEventLoopError {
    /// Errors at the FIDL layer.
    ///
    /// Such as: cannot connect to protocol or watcher, loaded FIDL error from stream.
    Fidl(anyhow::Error),
    /// Errors at the Netstack layer.
    ///
    /// Such as: route watcher event stream ended, unexpected event type, or struct from Netstack
    /// failed conversion.
    Netstack(anyhow::Error),
}

impl<S: Sender<<NetlinkRoute as ProtocolFamily>::Message>> EventLoop<S> {
    /// `new` returns an `EventLoop` instance.
    pub(crate) fn new(route_clients: ClientTable<NetlinkRoute, S>) -> Self {
        EventLoop { route_messages: Default::default(), route_clients }
    }

    /// Run the asynchronous work related to RTM_ROUTE messages.
    ///
    /// The event loop can track Ipv4 or Ipv6 routes, and is
    /// never expected to complete.
    /// Returns: `RoutesEventLoopError` that cannot be resolved from within the event loop.
    pub(crate) async fn run<I: Ip + fnet_routes_ext::FidlRouteIpExt>(
        &mut self,
    ) -> RoutesEventLoopError {
        use fuchsia_component::client::connect_to_protocol;

        let route_event_stream = {
            let state_res = connect_to_protocol::<I::StateMarker>().context(format!(
                "failed to connect to fuchsia.net.routes/StateV{}",
                I::VERSION.version_number(),
            ));
            let state = match state_res {
                Ok(state) => state,
                Err(e) => return RoutesEventLoopError::Fidl(e),
            };

            let stream_res = fnet_routes_ext::event_stream_from_state::<I>(&state).context(
                format!("failed to initialize a `WatcherV{}` client", I::VERSION.version_number()),
            );
            match stream_res {
                Ok(stream) => stream.fuse(),
                Err(e) => return RoutesEventLoopError::Fidl(e),
            }
        };
        pin_mut!(route_event_stream);

        let routes_res = fnet_routes_ext::collect_routes_until_idle::<_, HashSet<_>>(
            route_event_stream.by_ref(),
        )
        .await;

        let mut ctx =
            format!("while collecting existing IPv{} routes", I::VERSION.version_number());
        let routes = match routes_res {
            Ok(routes) => routes,
            Err(fnet_routes_ext::CollectRoutesUntilIdleError::ErrorInStream(e)) => {
                return RoutesEventLoopError::Fidl(anyhow!("stream error {}: {:?}", ctx, e));
            }
            Err(fnet_routes_ext::CollectRoutesUntilIdleError::StreamEnded) => {
                return RoutesEventLoopError::Fidl(anyhow!("stream ended {}", ctx))
            }
            Err(fnet_routes_ext::CollectRoutesUntilIdleError::UnexpectedEvent(event)) => {
                return RoutesEventLoopError::Netstack(anyhow!(
                    "unexpected event {}: {:?}",
                    ctx,
                    event
                ))
            }
        };

        let EventLoop { route_messages, route_clients } = self;

        *route_messages = new_set_with_existing_routes(routes);

        ctx = format!("in IPv{} route event stream", I::VERSION.version_number());
        loop {
            select! {
                stream_res = route_event_stream.try_next() => {
                    let event_opt = match stream_res {
                        Ok(event_res) => event_res,
                        Err(fnet_routes_ext::WatchError::Fidl(e)) => {
                            return RoutesEventLoopError::Fidl(anyhow!("fidl error {}: {:?}", ctx, e))
                        }
                        // Recoverable errors that should not crash the event loop.
                        Err(fnet_routes_ext::WatchError::EmptyEventBatch) => {
                            error!(
                                tag = NETLINK_LOG_TAG,
                                "{:?}",
                                RoutesEventLoopError::Fidl(anyhow!("empty batch error {}", ctx))
                            );
                            continue;
                        }
                        Err(fnet_routes_ext::WatchError::Conversion(e)) => {
                            error!(
                                tag = NETLINK_LOG_TAG,
                                "{:?}",
                                RoutesEventLoopError::Netstack(anyhow!(
                                    "type conversion error {}: {:?}",
                                    ctx,
                                    e
                                ))
                            );
                            continue;
                        }
                    };

                    let event = match event_opt {
                        Some(event) => event,
                        None => return RoutesEventLoopError::Fidl(anyhow!("route event stream ended")),
                    };

                    match handle_route_watcher_event(
                        route_messages,
                        route_clients,
                        event
                    ) {
                        Ok(()) => {}
                        // Recoverable errors that do not affect the processing of further events.
                        Err(RouteEventHandlerError::AlreadyExistingRouteAddition(route))
                        | Err(RouteEventHandlerError::NonExistentRouteDeletion(route)) => {
                            error!(
                                tag = NETLINK_LOG_TAG,
                                "observed no-op route modification: {:?}",
                                route
                            );
                        }
                        Err(RouteEventHandlerError::NonAddOrRemoveEventReceived(event)) => {
                            error!(
                                tag = NETLINK_LOG_TAG,
                                "observed no-op route event: {:?}",
                                event
                            );
                        }
                    }
                }
            }
        }
    }
}

// Errors related to handling route events.
#[derive(Debug, PartialEq)]
enum RouteEventHandlerError<I: Ip> {
    // Route watcher event handler attempted to add a route that already existed.
    AlreadyExistingRouteAddition(fnet_routes_ext::InstalledRoute<I>),
    // Route watcher event handler attempted to remove a route that does not exist.
    NonExistentRouteDeletion(fnet_routes_ext::InstalledRoute<I>),
    // Route watcher event handler attempted to process a route event that was not add or remove.
    NonAddOrRemoveEventReceived(fnet_routes_ext::Event<I>),
}

/// Handles events observed by the route watchers by adding/removing routes
/// from the underlying `NetlinkRouteMessage` set.
///
/// Returns a `RoutesEventLoopError` when unexpected events or HashSet issues occur.
fn handle_route_watcher_event<I: Ip, S: Sender<<NetlinkRoute as ProtocolFamily>::Message>>(
    route_messages: &mut HashSet<NetlinkRouteMessage>,
    route_clients: &ClientTable<NetlinkRoute, S>,
    event: fnet_routes_ext::Event<I>,
) -> Result<(), RouteEventHandlerError<I>> {
    let message_for_clients = match event {
        fnet_routes_ext::Event::Added(route) => {
            if let Some(route_message) = NetlinkRouteMessage::optionally_from(route) {
                if !route_messages.insert(route_message.clone()) {
                    return Err(RouteEventHandlerError::AlreadyExistingRouteAddition(route));
                }
                Some(route_message.into_rtnl_new_route())
            } else {
                None
            }
        }
        fnet_routes_ext::Event::Removed(route) => {
            if let Some(route_message) = NetlinkRouteMessage::optionally_from(route) {
                if !route_messages.remove(&route_message) {
                    return Err(RouteEventHandlerError::NonExistentRouteDeletion(route));
                }
                Some(route_message.into_rtnl_del_route())
            } else {
                None
            }
        }
        // We don't expect to observe any existing events, because the route watchers were drained
        // of existing events prior to starting the event loop.
        fnet_routes_ext::Event::Existing(_)
        | fnet_routes_ext::Event::Idle
        | fnet_routes_ext::Event::Unknown => {
            return Err(RouteEventHandlerError::NonAddOrRemoveEventReceived(event));
        }
    };
    if let Some(message_for_clients) = message_for_clients {
        let route_group = match I::VERSION {
            IpVersion::V4 => ModernGroup(RTNLGRP_IPV4_ROUTE),
            IpVersion::V6 => ModernGroup(RTNLGRP_IPV6_ROUTE),
        };
        route_clients.send_message_to_group(message_for_clients, route_group);
    }

    Ok(())
}

/// A wrapper type for the netlink_packet_route `RouteMessage` to enable conversions
/// from [`fnet_routes_ext::InstalledRoute`] and implement hashing.
#[derive(Clone, Debug, Eq, PartialEq)]
struct NetlinkRouteMessage(RouteMessage);

// Constructs a new set of `NetlinkRouteMessage` from an
// `InstalledRoute` HashSet.
fn new_set_with_existing_routes<I: Ip>(
    routes: HashSet<fnet_routes_ext::InstalledRoute<I>>,
) -> HashSet<NetlinkRouteMessage> {
    return routes
        .iter()
        .filter_map(|route| NetlinkRouteMessage::optionally_from(*route))
        .collect::<HashSet<_>>();
}

impl NetlinkRouteMessage {
    /// Implement optional conversions from `InstalledRoute` to `NetlinkRouteMessage`.
    /// `Ok` becomes `Some`, while `Err` is logged and becomes `None`.
    fn optionally_from<I: Ip>(
        route: fnet_routes_ext::InstalledRoute<I>,
    ) -> Option<NetlinkRouteMessage> {
        match route.try_into() {
            Ok(route) => Some(route),
            Err(NetlinkRouteMessageConversionError::RouteActionNotForwarding) => {
                warn!(
                    tag = NETLINK_LOG_TAG,
                    "Unexpected non-forwarding route in routing table: {:?}", route
                );
                None
            }
            Err(NetlinkRouteMessageConversionError::InvalidInterfaceId(id)) => {
                warn!(
                    tag = NETLINK_LOG_TAG,
                    "Invalid interface id found in routing table route: {:?}", id
                );
                None
            }
        }
    }

    /// Wrap the inner [`RouteMessage`] in an [`RtnlMessage::NewRoute`].
    fn into_rtnl_new_route(self) -> NetlinkMessage<RtnlMessage> {
        let NetlinkRouteMessage(message) = self;
        let mut msg: NetlinkMessage<RtnlMessage> = RtnlMessage::NewRoute(message).into();
        msg.finalize();
        msg
    }

    /// Wrap the inner [`RouteMessage`] in an [`RtnlMessage::DelRoute`].
    fn into_rtnl_del_route(self) -> NetlinkMessage<RtnlMessage> {
        let NetlinkRouteMessage(message) = self;
        let mut msg: NetlinkMessage<RtnlMessage> = RtnlMessage::DelRoute(message).into();
        msg.finalize();
        msg
    }
}

impl Hash for NetlinkRouteMessage {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let NetlinkRouteMessage(message) = self;
        message.header.hash(state);
        message.nlas.iter().for_each(|nla| {
            let mut buffer = vec![0u8; nla.value_len() as usize];
            nla.emit_value(&mut buffer);
            buffer.hash(state);
        });
    }
}

// NetlinkRouteMessage conversion related errors.
#[derive(Debug, PartialEq)]
enum NetlinkRouteMessageConversionError {
    // Route with non-forward action received from Netstack.
    RouteActionNotForwarding,
    // Interface id could not be downcasted to fit into the expected u32.
    InvalidInterfaceId(u64),
}

// Implement conversions from `InstalledRoute` to `NetlinkRouteMessage`
// which is fallible iff, the route's action is not `Forward`.
impl<I: Ip> TryFrom<fnet_routes_ext::InstalledRoute<I>> for NetlinkRouteMessage {
    type Error = NetlinkRouteMessageConversionError;
    fn try_from(
        fnet_routes_ext::InstalledRoute {
            route: fnet_routes_ext::Route { destination, action, properties: _ },
            effective_properties: fnet_routes_ext::EffectiveRouteProperties { metric },
        }: fnet_routes_ext::InstalledRoute<I>,
    ) -> Result<Self, Self::Error> {
        let fnet_routes_ext::RouteTarget { outbound_interface, next_hop } = match action {
            fnet_routes_ext::RouteAction::Unknown => {
                return Err(NetlinkRouteMessageConversionError::RouteActionNotForwarding)
            }
            fnet_routes_ext::RouteAction::Forward(target) => target,
        };

        let mut route_header = RouteHeader::default();
        // Both possible constants are in the range of u8-accepted values, so they can be
        // safely casted to a u8.
        route_header.address_family = match I::VERSION {
            IpVersion::V4 => AF_INET,
            IpVersion::V6 => AF_INET6,
        } as u8;
        route_header.destination_prefix_length = destination.prefix();

        // The following fields are used in the header, but they do not have any
        // corresponding values in `InstalledRoute`. The fields explicitly
        // defined below  are expected to be needed at some point, but the
        // information is not currently provided by the watcher.
        //
        // length of source prefix
        // tos filter (type of service)
        route_header.table = RT_TABLE_UNSPEC;
        route_header.protocol = RTPROT_UNSPEC;
        // Universe for routes with next_hop. Valid as long as route action
        // is forwarding.
        route_header.scope = RT_SCOPE_UNIVERSE;
        route_header.kind = RTN_UNICAST;

        // The NLA order follows the list that attributes are listed on the
        // rtnetlink man page.
        // The following fields are used in the options in the NLA, but they
        // do not have any corresponding values in `InstalledRoute`.
        //
        // RTA_SRC (route source address)
        // RTA_IIF (input interface index)
        // RTA_PREFSRC (preferred source address)
        // RTA_METRICS (route statistics)
        // RTA_MULTIPATH
        // RTA_FLOW
        // RTA_CACHEINFO
        // RTA_MARK
        // RTA_MFC_STATS
        // RTA_VIA
        // RTA_NEWDST
        // RTA_PREF
        // RTA_ENCAP_TYPE
        // RTA_ENCAP
        // RTA_EXPIRES (can set to 'forever' if it is required)
        let mut nlas = vec![];

        // A prefix length of 0 indicates it is the default route. Specifying
        // destination NLA does not provide useful information.
        if route_header.destination_prefix_length > 0 {
            let destination_nla = netlink_packet_route::route::Nla::Destination(
                destination.network().bytes().to_vec(),
            );
            nlas.push(destination_nla);
        }

        // We expect interface ids to safely fit in the range of u32 values.
        let outbound_id: u32 = match outbound_interface.try_into() {
            Err(_) => {
                return Err(NetlinkRouteMessageConversionError::InvalidInterfaceId(
                    outbound_interface,
                ))
            }
            Ok(id) => id,
        };
        let oif_nla = netlink_packet_route::route::Nla::Oif(outbound_id);
        nlas.push(oif_nla);

        if let Some(next_hop) = next_hop {
            let bytes = next_hop.bytes().iter().cloned().collect();
            let gateway_nla = netlink_packet_route::route::Nla::Gateway(bytes);
            nlas.push(gateway_nla);
        }

        let priority_nla = netlink_packet_route::route::Nla::Priority(metric);
        nlas.push(priority_nla);

        let mut route_message = RouteMessage::default();
        route_message.header = route_header;
        route_message.nlas = nlas;
        Ok(NetlinkRouteMessage(route_message))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use assert_matches::assert_matches;
    use fidl_fuchsia_net_routes as fnet_routes;
    use net_declare::{net_ip_v4, net_ip_v6, net_subnet_v4, net_subnet_v6};
    use net_types::{
        ip::{Ipv4, Ipv4Addr, Ipv6, Ipv6Addr, Subnet},
        SpecifiedAddr,
    };
    use test_case::test_case;

    use crate::messaging::testutil::FakeSender;

    fn create_installed_route<I: Ip>(
        subnet: Subnet<I::Addr>,
        next_hop: I::Addr,
        interface_id: u64,
        metric: u32,
    ) -> fnet_routes_ext::InstalledRoute<I> {
        fnet_routes_ext::InstalledRoute::<I> {
            route: fnet_routes_ext::Route {
                destination: subnet,
                action: fnet_routes_ext::RouteAction::Forward(fnet_routes_ext::RouteTarget::<I> {
                    outbound_interface: interface_id,
                    next_hop: SpecifiedAddr::new(next_hop),
                }),
                properties: fnet_routes_ext::RouteProperties {
                    specified_properties: fnet_routes_ext::SpecifiedRouteProperties {
                        metric: fnet_routes::SpecifiedMetric::ExplicitMetric(metric),
                    },
                },
            },
            effective_properties: fnet_routes_ext::EffectiveRouteProperties { metric },
        }
    }

    fn create_netlink_route_message(
        address_family: u8,
        destination_prefix_length: u8,
        nlas: Vec<netlink_packet_route::route::Nla>,
    ) -> NetlinkRouteMessage {
        let mut route_header = RouteHeader::default();
        route_header.address_family = address_family;
        route_header.destination_prefix_length = destination_prefix_length;
        route_header.kind = RTN_UNICAST;

        let mut route_message = RouteMessage::default();
        route_message.header = route_header;
        route_message.nlas = nlas;

        NetlinkRouteMessage(route_message)
    }

    fn create_nlas<I: Ip>(
        destination: Option<Subnet<I::Addr>>,
        next_hop: Option<I::Addr>,
        outgoing_interface_id: u32,
        metric: u32,
    ) -> Vec<netlink_packet_route::route::Nla> {
        let mut nlas = vec![];

        if let Some(destination) = destination {
            let destination_nla = netlink_packet_route::route::Nla::Destination(
                destination.network().bytes().to_vec(),
            );
            nlas.push(destination_nla);
        }

        let oif_nla = netlink_packet_route::route::Nla::Oif(outgoing_interface_id);
        nlas.push(oif_nla);

        if let Some(next_hop) = next_hop {
            let bytes = next_hop.bytes().iter().cloned().collect();
            let gateway_nla = netlink_packet_route::route::Nla::Gateway(bytes);
            nlas.push(gateway_nla);
        }

        let priority_nla = netlink_packet_route::route::Nla::Priority(metric);
        nlas.push(priority_nla);
        nlas
    }

    #[fuchsia::test]
    fn test_handle_route_watcher_event_v4() {
        handle_route_watcher_event_helper::<Ipv4>(
            net_subnet_v4!("192.0.2.0/24"),
            net_ip_v4!("192.0.2.1"),
        );
    }

    #[fuchsia::test]
    fn test_handle_route_watcher_event_v6() {
        handle_route_watcher_event_helper::<Ipv6>(
            net_subnet_v6!("2001:db8::0/32"),
            net_ip_v6!("2001:db8::1"),
        );
    }

    fn handle_route_watcher_event_helper<I: Ip>(subnet: Subnet<I::Addr>, next_hop: I::Addr) {
        let interface_id = 1u64;
        let metric: u32 = Default::default();
        let installed_route1: fnet_routes_ext::InstalledRoute<I> =
            create_installed_route(subnet, next_hop, interface_id, metric);
        let installed_route2: fnet_routes_ext::InstalledRoute<I> =
            create_installed_route(subnet, next_hop, interface_id, metric + 1);

        let add_event1 = fnet_routes_ext::Event::Added(installed_route1);
        let add_event2 = fnet_routes_ext::Event::Added(installed_route2);
        let remove_event = fnet_routes_ext::Event::Removed(installed_route1);
        let unknown_event: fnet_routes_ext::Event<I> = fnet_routes_ext::Event::Unknown;

        let mut route_messages: HashSet<NetlinkRouteMessage> = HashSet::new();
        let expected_route_message1: NetlinkRouteMessage = installed_route1.try_into().unwrap();
        let expected_route_message2: NetlinkRouteMessage = installed_route2.try_into().unwrap();

        // Setup two fake clients: one is a member of the route multicast group.
        let (right_group, wrong_group) = match I::VERSION {
            IpVersion::V4 => (ModernGroup(RTNLGRP_IPV4_ROUTE), ModernGroup(RTNLGRP_IPV6_ROUTE)),
            IpVersion::V6 => (ModernGroup(RTNLGRP_IPV6_ROUTE), ModernGroup(RTNLGRP_IPV4_ROUTE)),
        };
        let (mut right_sink, right_client) = crate::client::testutil::new_fake_client::<NetlinkRoute>(
            crate::client::testutil::CLIENT_ID_1,
            &[right_group],
        );
        let (mut wrong_sink, wrong_client) = crate::client::testutil::new_fake_client::<NetlinkRoute>(
            crate::client::testutil::CLIENT_ID_2,
            &[wrong_group],
        );
        let route_clients: ClientTable<NetlinkRoute, FakeSender<_>> = ClientTable::default();
        route_clients.add_client(right_client);
        route_clients.add_client(wrong_client);

        // An event that is not an add or remove should result in an error.
        assert_matches!(
            handle_route_watcher_event(&mut route_messages, &route_clients, unknown_event),
            Err(RouteEventHandlerError::NonAddOrRemoveEventReceived(_))
        );
        assert_eq!(route_messages.len(), 0);
        assert_eq!(&right_sink.take_messages()[..], &[]);
        assert_eq!(&wrong_sink.take_messages()[..], &[]);

        assert_eq!(
            handle_route_watcher_event(&mut route_messages, &route_clients, add_event1),
            Ok(())
        );
        assert_eq!(route_messages, HashSet::from_iter([expected_route_message1.clone()]));
        assert_eq!(
            &right_sink.take_messages()[..],
            &[expected_route_message1.clone().into_rtnl_new_route()]
        );
        assert_eq!(&wrong_sink.take_messages()[..], &[]);

        // Adding the same route again should result in an error.
        assert_matches!(
            handle_route_watcher_event(&mut route_messages, &route_clients, add_event1),
            Err(RouteEventHandlerError::AlreadyExistingRouteAddition(_))
        );
        assert_eq!(route_messages, HashSet::from_iter([expected_route_message1.clone()]));
        assert_eq!(&right_sink.take_messages()[..], &[]);
        assert_eq!(&wrong_sink.take_messages()[..], &[]);

        // Adding a different route should result in an addition.
        assert_eq!(
            handle_route_watcher_event(&mut route_messages, &route_clients, add_event2),
            Ok(())
        );
        assert_eq!(
            route_messages,
            HashSet::from_iter([expected_route_message1.clone(), expected_route_message2.clone()])
        );
        assert_eq!(
            &right_sink.take_messages()[..],
            &[expected_route_message2.clone().into_rtnl_new_route()]
        );
        assert_eq!(&wrong_sink.take_messages()[..], &[]);

        assert_eq!(
            handle_route_watcher_event(&mut route_messages, &route_clients, remove_event),
            Ok(())
        );
        assert_eq!(route_messages, HashSet::from_iter([expected_route_message2.clone()]));
        assert_eq!(
            &right_sink.take_messages()[..],
            &[expected_route_message1.clone().into_rtnl_del_route()]
        );
        assert_eq!(&wrong_sink.take_messages()[..], &[]);

        // Removing a route that doesn't exist should result in an error.
        assert_matches!(
            handle_route_watcher_event(&mut route_messages, &route_clients, remove_event),
            Err(RouteEventHandlerError::NonExistentRouteDeletion(_))
        );
        assert_eq!(route_messages, HashSet::from_iter([expected_route_message2.clone()]));
        assert_eq!(&right_sink.take_messages()[..], &[]);
        assert_eq!(&wrong_sink.take_messages()[..], &[]);
    }

    #[test_case(net_subnet_v4!("192.0.2.0/24"), net_ip_v4!("192.0.2.1"))]
    // InstalledRoute with a default subnet should not have a Destination NLA.
    #[test_case(net_subnet_v4!("0.0.0.0/0"), net_ip_v4!("0.0.0.1"))]
    fn test_netlink_route_message_try_from_installed_route_v4(
        subnet: Subnet<Ipv4Addr>,
        next_hop: Ipv4Addr,
    ) {
        netlink_route_message_conversion_helper::<Ipv4>(subnet, next_hop);
    }

    #[test_case(net_subnet_v6!("2001:db8::0/32"), net_ip_v6!("2001:db8::1"))]
    // InstalledRoute with a default subnet should not have a Destination NLA.
    #[test_case(net_subnet_v6!("::/0"), net_ip_v6!("::1"))]
    fn test_netlink_route_message_try_from_installed_route_v6(
        subnet: Subnet<Ipv6Addr>,
        next_hop: Ipv6Addr,
    ) {
        netlink_route_message_conversion_helper::<Ipv6>(subnet, next_hop);
    }

    fn netlink_route_message_conversion_helper<I: Ip>(subnet: Subnet<I::Addr>, next_hop: I::Addr) {
        let interface_id = 1u32;
        let metric: u32 = Default::default();

        let installed_route =
            create_installed_route::<I>(subnet, next_hop, interface_id.into(), metric);
        let prefix_length = subnet.prefix();
        let subnet = if prefix_length > 0 { Some(subnet) } else { None };
        let nlas = create_nlas::<I>(subnet, Some(next_hop), interface_id, metric);
        let address_family = match I::VERSION {
            IpVersion::V4 => AF_INET,
            IpVersion::V6 => AF_INET6,
        } as u8;
        let expected = create_netlink_route_message(address_family, prefix_length, nlas);

        let actual: NetlinkRouteMessage = installed_route.try_into().unwrap();
        assert_eq!(actual, expected);
    }

    #[fuchsia::test]
    fn test_non_forward_route_conversion() {
        let installed_route = fnet_routes_ext::InstalledRoute::<Ipv4> {
            route: fnet_routes_ext::Route {
                destination: net_subnet_v4!("192.0.2.0/24"),
                action: fnet_routes_ext::RouteAction::Unknown,
                properties: fnet_routes_ext::RouteProperties {
                    specified_properties: fnet_routes_ext::SpecifiedRouteProperties {
                        metric: fnet_routes::SpecifiedMetric::ExplicitMetric(Default::default()),
                    },
                },
            },
            effective_properties: fnet_routes_ext::EffectiveRouteProperties {
                metric: Default::default(),
            },
        };

        let actual: Result<NetlinkRouteMessage, NetlinkRouteMessageConversionError> =
            installed_route.try_into();
        assert_eq!(actual, Err(NetlinkRouteMessageConversionError::RouteActionNotForwarding));
    }

    #[fuchsia::test]
    fn test_oversized_interface_id_route_conversion() {
        let invalid_interface_id = (u32::MAX as u64) + 1;
        let installed_route: fnet_routes_ext::InstalledRoute<Ipv4> = create_installed_route(
            net_subnet_v4!("192.0.2.0/24"),
            net_ip_v4!("192.0.2.1"),
            invalid_interface_id,
            Default::default(),
        );

        let actual: Result<NetlinkRouteMessage, NetlinkRouteMessageConversionError> =
            installed_route.try_into();
        assert_eq!(
            actual,
            Err(NetlinkRouteMessageConversionError::InvalidInterfaceId(invalid_interface_id))
        );
    }

    #[test]
    fn test_into_rtnl_new_route_is_serializable() {
        let route = create_netlink_route_message(0, 0, vec![]);
        let new_route_message = route.into_rtnl_new_route();
        let mut buf = vec![0; new_route_message.buffer_len()];
        // Serialize will panic if `new_route_message` is malformed.
        new_route_message.serialize(&mut buf);
    }

    #[test]
    fn test_into_rtnl_del_route_is_serializable() {
        let route = create_netlink_route_message(0, 0, vec![]);
        let del_route_message = route.into_rtnl_del_route();
        let mut buf = vec![0; del_route_message.buffer_len()];
        // Serialize will panic if `del_route_message` is malformed.
        del_route_message.serialize(&mut buf);
    }

    #[fuchsia::test]
    fn test_new_set_with_existing_routes_v4() {
        new_set_with_existing_routes_helper::<Ipv4>(
            net_subnet_v4!("192.0.2.0/24"),
            net_ip_v4!("192.0.2.1"),
        );
    }

    #[fuchsia::test]
    fn test_new_set_with_existing_routes_v6() {
        new_set_with_existing_routes_helper::<Ipv6>(
            net_subnet_v6!("2001:db8::0/32"),
            net_ip_v6!("2001:db8::1"),
        );
    }

    fn new_set_with_existing_routes_helper<I: Ip>(subnet: Subnet<I::Addr>, next_hop: I::Addr) {
        let interface_id = u32::MAX;
        let metric: u32 = Default::default();

        let installed_route1: fnet_routes_ext::InstalledRoute<I> =
            create_installed_route(subnet, next_hop, interface_id as u64, metric);
        let installed_route2: fnet_routes_ext::InstalledRoute<I> =
            create_installed_route(subnet, next_hop, (interface_id as u64) + 1, metric);
        let routes: HashSet<fnet_routes_ext::InstalledRoute<I>> =
            vec![installed_route1, installed_route2].into_iter().collect::<_>();

        // One `InstalledRoute` has an invalid interface id, so it should be removed in
        // the conversion to the `NetlinkRouteMessage` HashSet.
        let actual = new_set_with_existing_routes::<I>(routes);
        assert_eq!(actual.len(), 1);

        let nlas = create_nlas::<I>(Some(subnet), Some(next_hop), interface_id, metric);
        let address_family = match I::VERSION {
            IpVersion::V4 => AF_INET,
            IpVersion::V6 => AF_INET6,
        } as u8;
        let netlink_route_message =
            create_netlink_route_message(address_family, subnet.prefix(), nlas);
        let expected: HashSet<NetlinkRouteMessage> =
            vec![netlink_route_message].into_iter().collect::<_>();
        assert_eq!(actual, expected);
    }
}
