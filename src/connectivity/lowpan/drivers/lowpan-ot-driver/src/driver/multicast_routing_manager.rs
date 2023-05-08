// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{fasync, stream::TryStreamExt as _};
use fidl::Error::ClientChannelClosed;
use fidl_fuchsia_net_ext::FromExt as _;
use fidl_fuchsia_net_multicast_admin as fnet_mcast;
use fuchsia_component::client::connect_to_protocol;
use fuchsia_zircon_status::Status as ZxStatus;
use std::collections::HashMap;
use std::fmt::{Debug, Formatter};
use std::sync::Arc;
use tracing::{error, info, warn};

// min_ttl value to be used for all routes added.
const MIN_TTL: u8 = 1;

// This wrapper struct helps to pretty print the `Ipv6UnicastSourceAndMulticastDestination` type.
struct PiiWrap<'a>(&'a fnet_mcast::Ipv6UnicastSourceAndMulticastDestination);
impl<'a> Debug for PiiWrap<'a> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let Self(fnet_mcast::Ipv6UnicastSourceAndMulticastDestination {
            unicast_source,
            multicast_destination,
        }) = self;

        f.debug_struct("Ipv6UnicastSourceAndMulticastDestination")
            .field("unicast_source", &net_types::ip::Ipv6Addr::from_ext(*unicast_source))
            .field(
                "multicast_destination",
                &net_types::ip::Ipv6Addr::from_ext(*multicast_destination),
            )
            .finish()
    }
}

#[derive(Debug)]
pub struct MulticastRoutingManager {
    spawned_task: Option<fuchsia_async::Task<()>>,
    // A map of multicast-destination and a list of source addresses for which
    // routes have been added for inbound packets from backbone interface to thread
    // interface. The list of source addresses will be needed for deleting routes
    // from multicast admin through `del_route` fidl.
    multicast_forwarding_cache: Arc<
        futures::lock::Mutex<
            HashMap<fidl_fuchsia_net::Ipv6Address, Vec<fidl_fuchsia_net::Ipv6Address>>,
        >,
    >,
}

impl MulticastRoutingManager {
    pub fn new() -> Self {
        let forwarding_cache = HashMap::new();
        let mutex_entry = futures::lock::Mutex::new(forwarding_cache);
        MulticastRoutingManager {
            spawned_task: None,
            multicast_forwarding_cache: std::sync::Arc::new(mutex_entry),
        }
    }

    pub fn start(&mut self, backbone_if_id: u64, net_if_id: u64) {
        let multicast_routing_client_end =
            connect_to_protocol::<fnet_mcast::Ipv6RoutingTableControllerMarker>()
                .expect("Failed to connect to Ipv6RoutingTableController service");

        self.spawned_task = Some(fasync::Task::spawn(Self::multicast_routing_manager_main_loop(
            Arc::clone(&self.multicast_forwarding_cache),
            backbone_if_id,
            net_if_id,
            multicast_routing_client_end,
        )));
    }

    async fn multicast_routing_manager_main_loop(
        multicast_forwarding_cache: Arc<
            futures::lock::Mutex<
                HashMap<fidl_fuchsia_net::Ipv6Address, Vec<fidl_fuchsia_net::Ipv6Address>>,
            >,
        >,
        backbone_if_id: u64,
        net_if_id: u64,
        multicast_routing_client_end: fnet_mcast::Ipv6RoutingTableControllerProxy,
    ) {
        loop {
            match multicast_routing_client_end.watch_routing_events().await {
                Ok((dropped_events, mut addresses, input_interface, event)) => {
                    if dropped_events != 0 {
                        warn!("Dropped {dropped_events:?} events before getting the event");
                    }

                    info!(
                        tag = "mcast_routing",
                        "Got routing event {:?} for addresses: {:?} on interface: {:?}",
                        event,
                        PiiWrap(&addresses),
                        input_interface
                    );

                    let mut multicast_forwarding_cache = multicast_forwarding_cache.lock().await;

                    // If there is a missing route event for a packet on backbone_if
                    // with destination address which is part of addresses we are monitoring,
                    // (ie which was added to multicast_forwarding_cache)
                    // we proceed to add route. Otherwise continue to wait for the next event.
                    let source_list_matching_dest_address = match event {
                        fnet_mcast::RoutingEvent::MissingRoute(fnet_mcast::Empty {}) => {
                            // TODO(https://fxbug.dev/116533) - investigate posix for packets going
                            // from thread interface to backbone interface and why routes aren't
                            // added there. Route need to be added for any outbound multicast
                            // packet (ie packets going from this thread network to multicast
                            // destination through backbone).
                            if input_interface != backbone_if_id {
                                continue;
                            }

                            match multicast_forwarding_cache
                                .get_mut(&addresses.multicast_destination)
                            {
                                None => continue,
                                Some(sources) => sources,
                            }
                        }

                        fnet_mcast::RoutingEvent::WrongInputInterface(interface) => {
                            warn!(
                                tag = "mcast_routing",
                                "Got routing event WrongInputInterface({:?})", interface
                            );
                            continue;
                        }
                    };

                    let route = fnet_mcast::Route {
                        expected_input_interface: Some(input_interface),
                        action: Some(fnet_mcast::Action::OutgoingInterfaces(vec![
                            fnet_mcast::OutgoingInterfaces { id: net_if_id, min_ttl: MIN_TTL },
                        ])),
                        ..Default::default()
                    };

                    match multicast_routing_client_end
                        .add_route(&mut addresses, route.clone())
                        .await
                    {
                        Err(err) => {
                            error!(
                                tag="mcast_routing", "Got FIDL error {:?} when trying to add route {:?} for address {:?}",
                                err, route, PiiWrap(&addresses)
                            );

                            // Error other than channel closed is unexpected:
                            assert!(err.is_closed());
                        }
                        Ok(Err(err)) => {
                            // Panic! for any error other than InterfaceNotFound as those indicate
                            // programmer error.
                            match err {
                                fnet_mcast::Ipv6RoutingTableControllerAddRouteError::InterfaceNotFound
                                    => error!(tag="mcast_routing", "Got error `{err:?}` when trying to add route {route:?} for address {:?}",
                                              PiiWrap(&addresses)) ,

                                fnet_mcast::Ipv6RoutingTableControllerAddRouteError::InvalidAddress |
                                fnet_mcast::Ipv6RoutingTableControllerAddRouteError::RequiredRouteFieldsMissing |
                                fnet_mcast::Ipv6RoutingTableControllerAddRouteError::InputCannotBeOutput
                                    => panic!("Unexpected error `{err:?}` trying to add {route:?} for address {:?}",
                                              PiiWrap(&addresses)),
                            }
                        }
                        Ok(Ok(())) => {
                            // Since route has been successfully added, let's add the source
                            // address in the list of addresses in local cache.
                            // This is used when deleting routes as both source and destination
                            // addresses are required.
                            source_list_matching_dest_address.push(addresses.unicast_source);
                            info!(
                                tag = "mcast_routing",
                                "Route added successfully and also updated hash table {:?}",
                                multicast_forwarding_cache
                            );
                        }
                    }
                }
                Err(err) => {
                    warn!(
                        tag = "mcast_routing",
                        "Got error in waiting for routing events: {:?}", err
                    );

                    if let ClientChannelClosed { status: ZxStatus::PEER_CLOSED, .. } = err {
                        let fidl_fuchsia_net_multicast_admin::Ipv6RoutingTableControllerEvent::OnClose{ error: reason }
                        = multicast_routing_client_end.take_event_stream().try_next().await
                            .expect("failed to read controller event")
                            .expect("event stream ended unexpectedly");
                        panic!("Fidl channel closed with error from OnClose {reason:?}");
                    }
                }
            }
        }
    }

    pub async fn add_forwarding_route(&self, group_dst_addr: &std::net::Ipv6Addr) {
        let mut multicast_forwarding_cache = self.multicast_forwarding_cache.lock().await;
        let insert_address = fidl_fuchsia_net::Ipv6Address { addr: group_dst_addr.octets() };
        multicast_forwarding_cache.insert(insert_address, Vec::new());
    }

    // TODO(https://fxbug.dev/116533) - add function remove_forwarding_route
    // which does clean-up of addresses similar to:
    // https://github.com/openthread/openthread/blob/main/src/posix/platform/multicast_routing.cpp#L589
}

#[cfg(test)]
mod test {
    use crate::driver::multicast_routing_manager::MIN_TTL;
    use crate::driver::MulticastRoutingManager;
    use crate::fasync;
    use anyhow::Context;
    use assert_matches::assert_matches;
    use fidl_fuchsia_net_multicast_admin as fnet_mcast;
    use futures::TryStreamExt as _;
    use std::sync::Arc;

    const BACKBONE_IF_ID: u64 = 1;
    const NET_IF_ID: u64 = 2;

    const GROUP_DEST_ADDR1: std::net::Ipv6Addr = net_declare::std_ip_v6!("ff04::1234:777a:1");
    const GROUP_DEST_ADDR2: std::net::Ipv6Addr = net_declare::std_ip_v6!("ff04::1234:777a:2");

    const UNICAST_SOURCE_ADDR_FIDL: fidl_fuchsia_net::Ipv6Address =
        net_declare::fidl_ip_v6!("910b::81:d7ff:fef8:b2e5");
    const MULTICAST_DEST_ADDR1_FIDL: fidl_fuchsia_net::Ipv6Address =
        fidl_fuchsia_net::Ipv6Address { addr: GROUP_DEST_ADDR1.octets() };
    const MULTICAST_DEST_ADDR2_FIDL: fidl_fuchsia_net::Ipv6Address =
        fidl_fuchsia_net::Ipv6Address { addr: GROUP_DEST_ADDR2.octets() };

    #[fasync::run_singlethreaded(test)]
    async fn test_multicast_add_route_for_matching_dest() {
        let (multicast_routing_client_end, multicast_routing_server_end) =
            fidl::endpoints::create_proxy::<fnet_mcast::Ipv6RoutingTableControllerMarker>()
                .expect("create Ipv6RoutingTableController endpoints");

        let mut routing_manager = MulticastRoutingManager::new();

        routing_manager.spawned_task = Some(fasync::Task::spawn(
            MulticastRoutingManager::multicast_routing_manager_main_loop(
                Arc::clone(&routing_manager.multicast_forwarding_cache),
                BACKBONE_IF_ID,
                NET_IF_ID,
                multicast_routing_client_end,
            ),
        ));

        routing_manager.add_forwarding_route(&GROUP_DEST_ADDR1).await;

        let mut req_stream =
            multicast_routing_server_end.into_stream().expect("Convert to request stream");

        let responder = assert_matches!(
            req_stream.try_next().await.context("error running multicast_routing_server"),
            Ok(Some(fnet_mcast::Ipv6RoutingTableControllerRequest::WatchRoutingEvents{responder})) => responder
        );

        let dropped_events = 0;
        let mut addresses = fnet_mcast::Ipv6UnicastSourceAndMulticastDestination {
            unicast_source: UNICAST_SOURCE_ADDR_FIDL,
            multicast_destination: MULTICAST_DEST_ADDR1_FIDL,
        };
        let event = fnet_mcast::RoutingEvent::MissingRoute(fnet_mcast::Empty {});

        // Send the missing route event.
        responder
            .send(dropped_events, &mut addresses, BACKBONE_IF_ID, &event)
            .expect("Responding should succeed");

        // Now ensure that 'add_route' request has been successfully made.
        let (add_route_addresses, route, add_route_responder) = assert_matches!(
            req_stream.try_next().await.context("error getting request from client"),
            Ok(Some(fnet_mcast::Ipv6RoutingTableControllerRequest::AddRoute{
                addresses, route, responder
            })) => (addresses, route, responder)
        );

        assert_eq!(addresses, add_route_addresses);

        // Check respective fields of route - expected input interface
        // and first member of outgoing interfaces should be BACKBONE_IF_ID and NET_IF_ID
        // respectively.
        assert_eq!(route.expected_input_interface.unwrap(), BACKBONE_IF_ID);
        let vec_outgoing_interfaces = assert_matches!(
            route.action.unwrap(),
            fnet_mcast::Action::OutgoingInterfaces(value) => value
        );
        assert_eq!(
            vec_outgoing_interfaces,
            vec![fnet_mcast::OutgoingInterfaces { id: NET_IF_ID, min_ttl: MIN_TTL }]
        );

        add_route_responder.send(&mut Ok(())).expect("Responding to add_route");

        // Test is done, do a final check to ensure that multicast routing service is
        // waiting for next routing event.
        assert_matches!(
            req_stream.try_next().await.context("error running multicast_routing_server"),
            Ok(Some(fnet_mcast::Ipv6RoutingTableControllerRequest::WatchRoutingEvents { .. }))
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_multicast_no_add_route_for_different_dest() {
        let (multicast_routing_client_end, multicast_routing_server_end) =
            fidl::endpoints::create_proxy::<fnet_mcast::Ipv6RoutingTableControllerMarker>()
                .expect("create Ipv6RoutingTableController endpoints");

        let mut routing_manager = MulticastRoutingManager::new();

        routing_manager.spawned_task = Some(fasync::Task::spawn(
            MulticastRoutingManager::multicast_routing_manager_main_loop(
                Arc::clone(&routing_manager.multicast_forwarding_cache),
                BACKBONE_IF_ID,
                NET_IF_ID,
                multicast_routing_client_end,
            ),
        ));

        // We monitor address 1, while we get event for address 2, this should not result in
        // add_route
        routing_manager.add_forwarding_route(&GROUP_DEST_ADDR1).await;

        let mut req_stream =
            multicast_routing_server_end.into_stream().expect("Convert to request stream");

        let responder = assert_matches!(
            req_stream.try_next().await.context("error running multicast_routing_server"),
            Ok(Some(fnet_mcast::Ipv6RoutingTableControllerRequest::WatchRoutingEvents{responder})) => responder
        );

        let dropped_events = 0;
        let mut addresses = fnet_mcast::Ipv6UnicastSourceAndMulticastDestination {
            unicast_source: UNICAST_SOURCE_ADDR_FIDL,
            multicast_destination: MULTICAST_DEST_ADDR2_FIDL,
        };
        let event = fnet_mcast::RoutingEvent::MissingRoute(fnet_mcast::Empty {});

        // Send the missing route event.
        responder
            .send(dropped_events, &mut addresses, BACKBONE_IF_ID, &event)
            .expect("Responding should succeed");

        // We should not see any AddRoute or any other request, and should directly see another
        // WatchRoutingEvents call.
        assert_matches!(
            req_stream.try_next().await.context("error running multicast_routing_server"),
            Ok(Some(fnet_mcast::Ipv6RoutingTableControllerRequest::WatchRoutingEvents { .. }))
        );
    }
}
