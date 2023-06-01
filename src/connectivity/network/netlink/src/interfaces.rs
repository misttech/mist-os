// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A module for managing RTM_LINK and RTM_ADDR information by generating
//! RTM_LINK and RTM_ADDR Netlink messages based on events received from
//! Netstack's interface watcher.

use std::{
    collections::{HashMap, HashSet},
    hash::{Hash, Hasher},
};

use anyhow::anyhow;
use fidl_fuchsia_hardware_network as fhwnet;
use fidl_fuchsia_net as fnet;
use fidl_fuchsia_net_interfaces as fnet_interfaces;
use fidl_fuchsia_net_interfaces_ext::{self as fnet_interfaces_ext, Update as _};
use futures::{pin_mut, StreamExt as _, TryStreamExt as _};
use netlink_packet_core::NetlinkMessage;
use netlink_packet_route::{
    AddressHeader, AddressMessage, LinkHeader, LinkMessage, RtnlMessage, AF_INET, AF_INET6,
    AF_UNSPEC, ARPHRD_ETHER, ARPHRD_LOOPBACK, ARPHRD_PPP, ARPHRD_VOID, IFA_F_PERMANENT,
    IFF_LOOPBACK, IFF_RUNNING, IFF_UP, RTNLGRP_LINK,
};
use netlink_packet_utils::nla::Nla;
use tracing::{debug, warn};

use crate::{
    client::ClientTable,
    messaging::Sender,
    multicast_groups::ModernGroup,
    protocol_family::{route::NetlinkRoute, ProtocolFamily},
    NETLINK_LOG_TAG,
};

/// Contains the asynchronous work related to RTM_LINK and RTM_ADDR messages.
///
/// Connects to the interfaces watcher and can respond to RTM_LINK and RTM_ADDR
/// message requests.
pub(crate) struct EventLoop<S: Sender<<NetlinkRoute as ProtocolFamily>::Message>> {
    /// A `StateProxy` to connect to the interfaces watcher.
    state_proxy: fnet_interfaces::StateProxy,
    /// The current set of clients of NETLINK_ROUTE protocol family.
    route_clients: ClientTable<NetlinkRoute, S>,
}

/// RTM_LINK and RTM_ADDR related event loop errors.
#[derive(Debug)]
pub(crate) enum InterfacesEventLoopError {
    /// Errors at the FIDL layer.
    ///
    /// Such as: cannot connect to protocol or watcher, loaded FIDL error
    /// from stream.
    Fidl(FidlError),
    /// Errors at the Netstack layer.
    ///
    /// Such as: interface watcher event stream ended, or a struct from
    /// Netstack failed conversion.
    Netstack(NetstackError),
}

#[derive(Debug)]
pub(crate) enum FidlError {
    /// Error in the FIDL event stream.
    EventStream(fidl::Error),
    /// Error that could not cleanly be wrapped with an error type.
    Unspecified(anyhow::Error),
    /// Error in getting event stream from state.
    WatcherCreation(fnet_interfaces_ext::WatcherCreationError),
}

#[derive(Debug)]
pub(crate) enum NetstackError {
    /// Event stream ended unexpectedly.
    EventStreamEnded,
    /// An existing event was received while handling interface events.
    ExistingEventReceived(fnet_interfaces_ext::Properties),
    /// Error that could not cleanly be wrapped with an error type.
    Unspecified(anyhow::Error),
    /// Inconsistent state between Netstack and interface properties.
    Update(fnet_interfaces_ext::UpdateError),
}

impl<S: Sender<<NetlinkRoute as ProtocolFamily>::Message>> EventLoop<S> {
    /// `new` returns a `Result<EventLoop, InterfacesEventLoopError>` instance.
    /// This is fallible iff it is not possible to obtain the `StateProxy`.
    pub(crate) fn new(
        route_clients: ClientTable<NetlinkRoute, S>,
    ) -> Result<Self, InterfacesEventLoopError> {
        use fuchsia_component::client::connect_to_protocol;
        let state_proxy = connect_to_protocol::<fnet_interfaces::StateMarker>()
            .map_err(|e| InterfacesEventLoopError::Fidl(FidlError::Unspecified(e)))?;

        Ok(EventLoop { state_proxy, route_clients })
    }

    /// Run the asynchronous work related to RTM_LINK and RTM_ADDR messages.
    ///
    /// The event loop can track interface properties, and is never
    /// expected to complete.
    /// Returns: `InterfacesEventLoopError` that requires restarting the
    /// event loop task, for example, if the watcher stream ends or if
    /// the FIDL protocol cannot be connected.
    pub(crate) async fn run(self) -> InterfacesEventLoopError {
        let EventLoop { state_proxy, route_clients } = self;
        let if_event_stream = {
            let stream_res = fnet_interfaces_ext::event_stream_from_state(&state_proxy);

            match stream_res {
                Ok(stream) => stream.fuse(),

                Err(e) => {
                    return InterfacesEventLoopError::Fidl(FidlError::WatcherCreation(e));
                }
            }
        };

        pin_mut!(if_event_stream);

        let interfaces_res =
            fnet_interfaces_ext::existing(if_event_stream.by_ref(), HashMap::new()).await;
        let mut interface_properties = match interfaces_res {
            Ok(interfaces) => interfaces,
            Err(e) => {
                return InterfacesEventLoopError::Netstack(NetstackError::Unspecified(anyhow!(
                    "could not fetch existing interface events: {:?}",
                    e
                )));
            }
        };

        loop {
            let stream_res = if_event_stream.try_next().await;
            let event = match stream_res {
                Ok(Some(event)) => event,
                Ok(None) => {
                    return InterfacesEventLoopError::Netstack(NetstackError::EventStreamEnded);
                }
                Err(e) => {
                    return InterfacesEventLoopError::Fidl(FidlError::EventStream(e));
                }
            };

            match handle_interface_watcher_event(&mut interface_properties, &route_clients, event) {
                Ok(()) => {}
                Err(InterfaceEventHandlerError::ExistingEventReceived(properties)) => {
                    // This error indicates there is an inconsistent interface state shared
                    // between Netlink and Netstack.
                    return InterfacesEventLoopError::Netstack(
                        NetstackError::ExistingEventReceived(properties),
                    );
                }
                Err(InterfaceEventHandlerError::Update(e)) => {
                    // This error is severe enough to indicate a larger problem in Netstack.
                    return InterfacesEventLoopError::Netstack(NetstackError::Update(e));
                }
            }
        }
    }
}

// Errors related to handling interface events.
#[derive(Debug)]
enum InterfaceEventHandlerError {
    // Interface event handler updated the HashMap with an event, but received an
    // unexpected response.
    Update(fnet_interfaces_ext::UpdateError),
    // Interface event handler attempted to process an event for an interface that already existed.
    ExistingEventReceived(fnet_interfaces_ext::Properties),
}

/// Handles events observed from the interface watcher by updating interfaces
/// from the underlying interface properties HashMap.
///
/// Returns an `InterfaceEventLoopError` when unexpected events occur, or an
/// `UpdateError` when updates are not consistent with the current state.
fn handle_interface_watcher_event<S: Sender<<NetlinkRoute as ProtocolFamily>::Message>>(
    interface_properties: &mut HashMap<u64, fnet_interfaces_ext::Properties>,
    route_clients: &ClientTable<NetlinkRoute, S>,
    event: fnet_interfaces::Event,
) -> Result<(), InterfaceEventHandlerError> {
    let update = match interface_properties.update(event) {
        Ok(update) => update,
        Err(e) => return Err(InterfaceEventHandlerError::Update(e.into())),
    };

    match update {
        fnet_interfaces_ext::UpdateResult::Added(properties) => {
            if let Some(message) = NetlinkLinkMessage::optionally_from(properties) {
                route_clients
                    .send_message_to_group(message.into_rtnl_new_link(), ModernGroup(RTNLGRP_LINK))
            }

            // TODO(issuetracker.google.com/283826112): Send update to RTMGRP_IPV{4,6}_ADDR group
            debug!(tag = NETLINK_LOG_TAG, "processed add/existing event for id {}", properties.id);
        }
        fnet_interfaces_ext::UpdateResult::Changed {
            previous:
                fnet_interfaces::Properties {
                    online,
                    addresses,
                    id: _,
                    name: _,
                    device_class: _,
                    has_default_ipv4_route: _,
                    has_default_ipv6_route: _,
                    ..
                },
            current:
                current @ fnet_interfaces_ext::Properties {
                    addresses: current_addresses,
                    id,
                    name: _,
                    device_class: _,
                    online: _,
                    has_default_ipv4_route: _,
                    has_default_ipv6_route: _,
                },
        } => {
            if online.is_some() {
                if let Some(message) = NetlinkLinkMessage::optionally_from(current) {
                    route_clients.send_message_to_group(
                        message.into_rtnl_new_link(),
                        ModernGroup(RTNLGRP_LINK),
                    )
                }

                debug!(
                    tag = NETLINK_LOG_TAG,
                    "processed interface link change event for id {}", id
                );
            };
            if addresses.map_or(false, |addresses| {
                let previous = addresses
                    .iter()
                    .filter_map(|fnet_interfaces::Address { addr, .. }| addr.as_ref())
                    .collect::<HashSet<_>>();
                let current = current_addresses
                    .iter()
                    .map(|fnet_interfaces_ext::Address { addr, valid_until: _ }| addr)
                    .collect::<HashSet<_>>();
                previous.ne(&current)
            }) {
                // TODO: Send update to RTM_GRP_IPV{4,6}_ADDR group
                debug!(
                    tag = NETLINK_LOG_TAG,
                    "processed interface address change event for id {}", id
                );
            };
        }
        fnet_interfaces_ext::UpdateResult::Removed(properties) => {
            // TODO: Send deletion to RTMGRP_IPV{4,6}_ADDR group

            if let Some(message) = NetlinkLinkMessage::optionally_from(&properties) {
                route_clients
                    .send_message_to_group(message.into_rtnl_del_link(), ModernGroup(RTNLGRP_LINK))
            }

            debug!(
                tag = NETLINK_LOG_TAG,
                "processed interface remove event for id {}", properties.id
            );
        }
        fnet_interfaces_ext::UpdateResult::Existing(properties) => {
            return Err(InterfaceEventHandlerError::ExistingEventReceived(properties.clone()));
        }
        fnet_interfaces_ext::UpdateResult::NoChange => {}
    }
    Ok(())
}

/// A wrapper type for the netlink_packet_route `LinkMessage` to enable conversions
/// from [`fnet_interfaces_ext::Properties`]. The addresses component of this
/// struct will be handled separately.
#[derive(Clone, Debug, Eq, PartialEq)]
struct NetlinkLinkMessage(LinkMessage);

impl NetlinkLinkMessage {
    fn optionally_from(properties: &fnet_interfaces_ext::Properties) -> Option<Self> {
        match properties.clone().try_into() {
            Ok(o) => Some(o),
            Err(NetlinkLinkMessageConversionError::InvalidInterfaceId(id)) => {
                warn!(tag = NETLINK_LOG_TAG, "Invalid interface id: {:?}", id);
                None
            }
        }
    }

    fn into_rtnl_new_link(self) -> NetlinkMessage<RtnlMessage> {
        let Self(message) = self;
        RtnlMessage::NewLink(message).into()
    }

    fn into_rtnl_del_link(self) -> NetlinkMessage<RtnlMessage> {
        let Self(message) = self;
        RtnlMessage::DelLink(message).into()
    }
}

// NetlinkLinkMessage conversion related errors.
#[derive(Debug, PartialEq)]
enum NetlinkLinkMessageConversionError {
    // Interface id could not be downcasted to fit into the expected u32.
    InvalidInterfaceId(u64),
}

fn device_class_to_link_type(device_class: fnet_interfaces::DeviceClass) -> u16 {
    match device_class {
        fnet_interfaces::DeviceClass::Loopback(fnet_interfaces::Empty {}) => ARPHRD_LOOPBACK,
        fnet_interfaces::DeviceClass::Device(device_class) => match device_class {
            fhwnet::DeviceClass::Ethernet
            | fhwnet::DeviceClass::Wlan
            | fhwnet::DeviceClass::WlanAp => ARPHRD_ETHER,
            fhwnet::DeviceClass::Ppp => ARPHRD_PPP,
            // TODO(https://issuetracker.google.com/284962255): Find a better mapping for
            // Bridge and Virtual device class
            fhwnet::DeviceClass::Bridge | fhwnet::DeviceClass::Virtual => ARPHRD_VOID,
        },
    }
}

// Implement conversions from `Properties` to `NetlinkLinkMessage`
// which is fallible iff, the interface has an id greater than u32.
impl TryFrom<fnet_interfaces_ext::Properties> for NetlinkLinkMessage {
    type Error = NetlinkLinkMessageConversionError;
    fn try_from(
        fnet_interfaces_ext::Properties {
            id,
            name,
            device_class,
            online,
            addresses: _,
            has_default_ipv4_route: _,
            has_default_ipv6_route: _,
        }: fnet_interfaces_ext::Properties,
    ) -> Result<Self, Self::Error> {
        let mut link_header = LinkHeader::default();

        // This constant is in the range of u8-accepted values, so it can be safely casted to a u8.
        link_header.interface_family = AF_UNSPEC.try_into().expect("should fit into u8");

        // We expect interface ids to safely fit in the range of u32 values.
        let id: u32 = match id.get().try_into() {
            Err(std::num::TryFromIntError { .. }) => {
                return Err(NetlinkLinkMessageConversionError::InvalidInterfaceId(id.into()))
            }
            Ok(id) => id,
        };
        link_header.index = id;

        let link_layer_type = device_class_to_link_type(device_class);
        link_header.link_layer_type = link_layer_type;

        let mut flags = 0;
        if online {
            // Netstack only reports 'online' when the 'admin status' is 'enabled' and the 'link
            // state' is UP. IFF_RUNNING represents only `link state` UP, so it is likely that
            // there will be cases where a flag should be set to IFF_RUNNING but we can not make
            // the determination with the information provided.
            flags |= IFF_UP | IFF_RUNNING;
        };
        if link_header.link_layer_type == ARPHRD_LOOPBACK {
            flags |= IFF_LOOPBACK;
        };
        link_header.flags = flags;

        // As per netlink_package_route and rtnetlink documentation, this should be set to
        // `0xffff_ffff` and reserved for future use.
        link_header.change_mask = u32::MAX;

        // The NLA order follows the list that attributes are listed on the
        // rtnetlink man page.
        // The following fields are used in the options in the NLA, but they do
        // not have any corresponding values in `fnet_interfaces_ext::Properties`.
        //
        // IFLA_ADDRESS
        // IFLA_BROADCAST
        // IFLA_MTU
        // IFLA_LINK
        // IFLA_QDISC
        // IFLA_STATS
        //
        // There are other NLAs observed via the netlink_packet_route crate, and do
        // not have corresponding values in `fnet_interfaces_ext::Properties`.
        // This list is documented within issuetracker.google.com/283137644.
        let nlas = vec![
            netlink_packet_route::link::nlas::Nla::IfName(name),
            netlink_packet_route::link::nlas::Nla::Link(link_layer_type.into()),
            // Netstack only exposes enough state to determine between `Up` and `Down`
            // operating state.
            netlink_packet_route::link::nlas::Nla::OperState(if online {
                netlink_packet_route::nlas::link::State::Up
            } else {
                netlink_packet_route::nlas::link::State::Down
            }),
        ];

        let mut link_message = LinkMessage::default();
        link_message.header = link_header;
        link_message.nlas = nlas;

        return Ok(NetlinkLinkMessage(link_message));
    }
}

/// A wrapper type for the netlink_packet_route `AddressMessage` to enable conversions
/// from [`fnet_interfaces_ext::Properties`] and implement hashing.
#[derive(Clone, Debug, Eq, PartialEq)]
struct NetlinkAddressMessage(AddressMessage);

impl Hash for NetlinkAddressMessage {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let NetlinkAddressMessage(message) = self;

        message.header.family.hash(state);
        message.header.prefix_len.hash(state);
        message.header.flags.hash(state);
        message.header.scope.hash(state);
        message.header.index.hash(state);

        message.nlas.iter().for_each(|nla| {
            let mut buffer = vec![0u8; nla.value_len() as usize];
            nla.emit_value(&mut buffer);
            buffer.hash(state);
        });
    }
}

// NetlinkAddressMessage conversion related errors.
#[derive(Debug, PartialEq)]
enum NetlinkAddressMessageConversionError {
    // Interface id could not be downcasted to fit into the expected u32.
    InvalidInterfaceId(u64),
}

#[allow(unused)]
// Implement conversions from `Properties` to `Vec<NetlinkAddressMessage>`
// which is fallible iff, the interface has an id greater than u32.
fn interface_properties_to_address_messages(
    fnet_interfaces_ext::Properties {
        id,
        name,
        addresses,
        device_class: _,
        online: _,
        has_default_ipv4_route: _,
        has_default_ipv6_route: _,
    }: fnet_interfaces_ext::Properties,
) -> Result<HashSet<NetlinkAddressMessage>, NetlinkAddressMessageConversionError> {
    {
        // We expect interface ids to safely fit in the range of the u32 values.
        let id: u32 = match id.get().try_into() {
            Err(std::num::TryFromIntError { .. }) => {
                return Err(NetlinkAddressMessageConversionError::InvalidInterfaceId(id.into()))
            }
            Ok(id) => id,
        };

        let address_messages = addresses
            .into_iter()
            .map(|fnet_interfaces_ext::Address { addr: subnet, valid_until: _ }| {
                let mut addr_header = AddressHeader::default();

                let (family, addr) = match subnet.addr {
                    fnet::IpAddress::Ipv4(ip_addr) => (AF_INET, ip_addr.addr.to_vec()),
                    fnet::IpAddress::Ipv6(ip_addr) => (AF_INET6, ip_addr.addr.to_vec()),
                };

                // The possible constants below are in the range of u8-accepted values, so they can
                // be safely casted to a u8.
                addr_header.family = family.try_into().expect("should fit into u8");
                addr_header.prefix_len = subnet.prefix_len;
                // In the header, flags are stored as u8, and in the NLAs, flags are stored as u32,
                // requiring casting. There are several possible flags, such as
                // IFA_F_NOPREFIXROUTE that do not fit into a u8, and are expected to be lost in
                // the header. This NLA is present in netlink_packet_route but is not shown on the
                // rtnetlink man page.
                // TODO(https://issuetracker.google.com/284980862): Determine proper mapping from
                // Netstack properties to address flags.
                let flags = IFA_F_PERMANENT;
                addr_header.flags = flags as u8;
                addr_header.index = id;

                // The NLA order follows the list that attributes are listed on the
                // rtnetlink man page.
                // The following fields are used in the options in the NLA, but they do
                // not have any corresponding values in `fnet_interfaces_ext::Properties` or
                // `fnet_interfaces_ext::Address`.
                //
                // IFA_LOCAL
                // IFA_BROADCAST
                // IFA_ANYCAST
                // IFA_CACHEINFO
                //
                // IFA_MULTICAST is documented via the netlink_packet_route crate but is not
                // present on the rtnetlink page.
                let nlas = vec![
                    netlink_packet_route::address::nlas::Nla::Address(addr),
                    netlink_packet_route::address::nlas::Nla::Label(name.clone()),
                    netlink_packet_route::address::nlas::Nla::Flags(flags.into()),
                ];

                let mut addr_message = AddressMessage::default();
                addr_message.header = addr_header;
                addr_message.nlas = nlas;
                NetlinkAddressMessage(addr_message)
            })
            .collect();

        Ok(address_messages)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::num::NonZeroU64;

    use fidl_fuchsia_net as fnet;
    use fuchsia_async::{self as fasync};
    use futures::{future::FutureExt as _, stream::Stream};

    use assert_matches::assert_matches;
    use net_declare::fidl_ip;
    use netlink_packet_route::{AddressHeader, AddressMessage, AF_INET, AF_INET6};
    use test_case::test_case;

    use crate::messaging::testutil::FakeSender;

    const ETHERNET: fnet_interfaces::DeviceClass =
        fnet_interfaces::DeviceClass::Device(fhwnet::DeviceClass::Ethernet);
    const WLAN: fnet_interfaces::DeviceClass =
        fnet_interfaces::DeviceClass::Device(fhwnet::DeviceClass::Wlan);
    const WLAN_AP: fnet_interfaces::DeviceClass =
        fnet_interfaces::DeviceClass::Device(fhwnet::DeviceClass::WlanAp);
    const PPP: fnet_interfaces::DeviceClass =
        fnet_interfaces::DeviceClass::Device(fhwnet::DeviceClass::Ppp);
    const LOOPBACK: fnet_interfaces::DeviceClass =
        fnet_interfaces::DeviceClass::Loopback(fnet_interfaces::Empty {});
    const TEST_V4_ADDR: fnet::Subnet = fnet::Subnet { addr: fidl_ip!("192.0.2.0"), prefix_len: 24 };
    const TEST_V6_ADDR: fnet::Subnet =
        fnet::Subnet { addr: fidl_ip!("2001:db8::0"), prefix_len: 32 };

    fn create_interface(
        id: u64,
        name: String,
        device_class: fnet_interfaces::DeviceClass,
        online: bool,
        addresses: Vec<fnet_interfaces_ext::Address>,
    ) -> fnet_interfaces_ext::Properties {
        fnet_interfaces_ext::Properties {
            id: NonZeroU64::new(id).unwrap(),
            name,
            device_class,
            online,
            addresses,
            has_default_ipv4_route: false,
            has_default_ipv6_route: false,
        }
    }

    fn create_interface_with_addresses(
        id: u64,
        name: String,
        device_class: fnet_interfaces::DeviceClass,
        online: bool,
    ) -> fnet_interfaces_ext::Properties {
        let addresses = vec![
            fnet_interfaces_ext::Address { addr: TEST_V4_ADDR, valid_until: Default::default() },
            fnet_interfaces_ext::Address { addr: TEST_V6_ADDR, valid_until: Default::default() },
        ];
        create_interface(id, name, device_class, online, addresses)
    }

    fn create_netlink_link_message(
        id: u64,
        link_type: u16,
        flags: u32,
        nlas: Vec<netlink_packet_route::link::nlas::Nla>,
    ) -> NetlinkLinkMessage {
        let mut link_header = LinkHeader::default();
        link_header.index = id.try_into().expect("should fit into u32");
        link_header.link_layer_type = link_type;
        link_header.flags = flags;
        link_header.change_mask = u32::MAX;

        let mut link_message = LinkMessage::default();
        link_message.header = link_header;
        link_message.nlas = nlas;

        NetlinkLinkMessage(link_message)
    }

    fn create_nlas(
        name: String,
        link_type: u16,
        online: bool,
    ) -> Vec<netlink_packet_route::link::nlas::Nla> {
        vec![
            netlink_packet_route::link::nlas::Nla::IfName(name),
            netlink_packet_route::link::nlas::Nla::Link(link_type.into()),
            netlink_packet_route::link::nlas::Nla::OperState(if online {
                netlink_packet_route::nlas::link::State::Up
            } else {
                netlink_packet_route::nlas::link::State::Down
            }),
        ]
    }

    fn create_default_address_messages(
        interface_id: u64,
        interface_name: String,
        flags: u32,
    ) -> HashSet<NetlinkAddressMessage> {
        let interface_id = interface_id.try_into().expect("should fit into u32");
        HashSet::from_iter([
            create_address_message(interface_id, TEST_V4_ADDR, interface_name.clone(), flags),
            create_address_message(interface_id, TEST_V6_ADDR, interface_name, flags),
        ])
    }

    fn create_address_message(
        interface_id: u32,
        subnet: fnet::Subnet,
        interface_name: String,
        flags: u32,
    ) -> NetlinkAddressMessage {
        let mut addr_header = AddressHeader::default();
        let (family, addr) = match subnet.addr {
            fnet::IpAddress::Ipv4(ip_addr) => (AF_INET, ip_addr.addr.to_vec()),
            fnet::IpAddress::Ipv6(ip_addr) => (AF_INET6, ip_addr.addr.to_vec()),
        };
        addr_header.family = family as u8;
        addr_header.prefix_len = subnet.prefix_len;
        addr_header.flags = flags.try_into().expect("should fit into u8");
        addr_header.index = interface_id;

        let nlas = vec![
            netlink_packet_route::address::nlas::Nla::Address(addr),
            netlink_packet_route::address::nlas::Nla::Label(interface_name),
            netlink_packet_route::address::nlas::Nla::Flags(flags.into()),
        ];

        let mut addr_message = AddressMessage::default();
        addr_message.header = addr_header;
        addr_message.nlas = nlas;
        NetlinkAddressMessage(addr_message)
    }

    #[fuchsia::test]
    fn test_handle_interface_watcher_event() {
        let mut interface_properties: HashMap<u64, fnet_interfaces_ext::Properties> =
            HashMap::new();
        let route_clients = ClientTable::<NetlinkRoute, FakeSender<_>>::default();

        let mut interface1 = create_interface(1, "test".into(), ETHERNET, true, vec![]);
        let interface2 = create_interface(2, "lo".into(), LOOPBACK, true, vec![]);

        let interface1_add_event = fnet_interfaces::Event::Added(interface1.clone().into());
        assert_matches!(
            handle_interface_watcher_event(
                &mut interface_properties,
                &route_clients,
                interface1_add_event
            ),
            Ok(())
        );
        assert_eq!(interface_properties.get(&1).unwrap(), &interface1);

        // Sending an updated interface properties with a different field
        // should update the properties stored under the same interface id.
        interface1.online = false;
        let interface1_change_event = fnet_interfaces::Event::Changed(interface1.clone().into());
        assert_matches!(
            handle_interface_watcher_event(
                &mut interface_properties,
                &route_clients,
                interface1_change_event
            ),
            Ok(())
        );
        assert_eq!(interface_properties.get(&1).unwrap(), &interface1);

        let interface2_add_event = fnet_interfaces::Event::Added(interface2.clone().into());
        assert_matches!(
            handle_interface_watcher_event(
                &mut interface_properties,
                &route_clients,
                interface2_add_event
            ),
            Ok(())
        );
        assert_eq!(interface_properties.get(&1).unwrap(), &interface1);
        assert_eq!(interface_properties.get(&2).unwrap(), &interface2);

        // A remove event should result in no longer seeing the LinkMessage in the
        // interface properties HashMap.
        let interface1_remove_event = fnet_interfaces::Event::Removed(1);
        assert_matches!(
            handle_interface_watcher_event(
                &mut interface_properties,
                &route_clients,
                interface1_remove_event
            ),
            Ok(())
        );
        assert_eq!(interface_properties.get(&1), None);
        assert_eq!(interface_properties.get(&2).unwrap(), &interface2);
    }

    fn get_fake_interface(
        id: u64,
        name: &'static str,
        device_class: fnet_interfaces::DeviceClass,
    ) -> fnet_interfaces_ext::Properties {
        fnet_interfaces_ext::Properties {
            id: id.try_into().unwrap(),
            name: name.to_string(),
            device_class,
            online: true,
            addresses: Vec::new(),
            has_default_ipv4_route: false,
            has_default_ipv6_route: false,
        }
    }

    struct Setup<W> {
        event_loop: EventLoop<FakeSender<NetlinkMessage<RtnlMessage>>>,
        watcher_stream: W,
    }

    fn setup_with_route_clients(
        route_clients: ClientTable<NetlinkRoute, FakeSender<NetlinkMessage<RtnlMessage>>>,
    ) -> Setup<impl Stream<Item = fnet_interfaces::WatcherRequest>> {
        let (state_proxy, if_stream) =
            fidl::endpoints::create_proxy_and_stream::<fnet_interfaces::StateMarker>().unwrap();
        let event_loop = EventLoop::<FakeSender<_>> { state_proxy, route_clients };

        let watcher_stream = if_stream
            .and_then(|req| match req {
                fnet_interfaces::StateRequest::GetWatcher {
                    options: _,
                    watcher,
                    control_handle: _,
                } => futures::future::ready(watcher.into_stream()),
            })
            .try_flatten()
            .map(|res| res.expect("watcher stream error"));

        Setup { event_loop, watcher_stream }
    }

    fn setup() -> Setup<impl Stream<Item = fnet_interfaces::WatcherRequest>> {
        setup_with_route_clients(ClientTable::default())
    }

    async fn respond_to_watcher<S: Stream<Item = fnet_interfaces::WatcherRequest>>(
        stream: S,
        updates: impl IntoIterator<Item = fnet_interfaces::Event>,
    ) {
        stream
            .zip(futures::stream::iter(updates.into_iter()))
            .for_each(|(req, update)| async move {
                match req {
                    fnet_interfaces::WatcherRequest::Watch { responder } => {
                        responder.send(&update).expect("send watch response")
                    }
                }
            })
            .await
    }

    async fn respond_to_watcher_with_interfaces<
        S: Stream<Item = fnet_interfaces::WatcherRequest>,
    >(
        stream: S,
        existing_interfaces: impl IntoIterator<Item = fnet_interfaces_ext::Properties>,
        new_interfaces: Option<(
            impl IntoIterator<Item = fnet_interfaces_ext::Properties>,
            fn(fnet_interfaces::Properties) -> fnet_interfaces::Event,
        )>,
    ) {
        let map_fn = |fnet_interfaces_ext::Properties {
                          id,
                          name,
                          device_class,
                          online,
                          addresses,
                          has_default_ipv4_route,
                          has_default_ipv6_route,
                      }| {
            fnet_interfaces::Properties {
                id: Some(id.get()),
                name: Some(name),
                device_class: Some(device_class),
                online: Some(online),
                addresses: Some(
                    addresses
                        .into_iter()
                        .map(|fnet_interfaces_ext::Address { addr, valid_until }| {
                            fnet_interfaces::Address {
                                addr: Some(addr),
                                valid_until: Some(valid_until),
                                ..Default::default()
                            }
                        })
                        .collect(),
                ),
                has_default_ipv4_route: Some(has_default_ipv4_route),
                has_default_ipv6_route: Some(has_default_ipv6_route),
                ..Default::default()
            }
        };
        respond_to_watcher(
            stream,
            existing_interfaces
                .into_iter()
                .map(|properties| fnet_interfaces::Event::Existing(map_fn(properties)))
                .chain(std::iter::once(fnet_interfaces::Event::Idle(fnet_interfaces::Empty)))
                .chain(
                    new_interfaces
                        .map(|(new_interfaces, event_fn)| {
                            new_interfaces
                                .into_iter()
                                .map(move |properties| event_fn(map_fn(properties)))
                        })
                        .into_iter()
                        .flatten(),
                ),
        )
        .await
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_event_loop_errors_stream_ended() {
        let Setup { event_loop, watcher_stream } = setup();
        let event_loop_fut = event_loop.run();
        let interfaces = vec![get_fake_interface(1, "lo", LOOPBACK)];
        let watcher_fut =
            respond_to_watcher_with_interfaces(watcher_stream, interfaces, None::<(Option<_>, _)>);

        let (err, ()) = futures::join!(event_loop_fut, watcher_fut);
        assert_matches!(
            err,
            InterfacesEventLoopError::Fidl(FidlError::EventStream(
                fidl::Error::ClientChannelClosed { .. }
            ))
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_event_loop_errors_duplicate_events() {
        let Setup { event_loop, watcher_stream } = setup();
        let event_loop_fut = event_loop.run();
        let interfaces =
            vec![get_fake_interface(1, "lo", LOOPBACK), get_fake_interface(1, "lo", LOOPBACK)];
        let watcher_fut =
            respond_to_watcher_with_interfaces(watcher_stream, interfaces, None::<(Option<_>, _)>);

        let (err, ()) = futures::join!(event_loop_fut, watcher_fut);
        assert_matches!(err, InterfacesEventLoopError::Netstack(NetstackError::Unspecified(_)));
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_event_loop_errors_duplicate_adds() {
        let Setup { event_loop, watcher_stream } = setup();
        let event_loop_fut = event_loop.run();
        let interfaces_existing = vec![get_fake_interface(1, "lo", LOOPBACK)];
        let interfaces_new = vec![get_fake_interface(1, "lo", LOOPBACK)];
        let watcher_fut = respond_to_watcher_with_interfaces(
            watcher_stream,
            interfaces_existing,
            Some((interfaces_new, fnet_interfaces::Event::Added)),
        );

        let (err, ()) = futures::join!(event_loop_fut, watcher_fut);
        // The properties that are being added again has interface id 1.
        assert_matches!(err,
            InterfacesEventLoopError::Netstack(
                NetstackError::Update(
                    fnet_interfaces_ext::UpdateError::DuplicateAdded(properties)
                    )
                )
            if properties.id.unwrap() == 1);
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_event_loop_errors_existing_after_add() {
        let Setup { event_loop, watcher_stream } = setup();
        let event_loop_fut = event_loop.run();
        let interfaces_existing =
            vec![get_fake_interface(1, "lo", LOOPBACK), get_fake_interface(2, "eth001", ETHERNET)];
        let interfaces_new = vec![get_fake_interface(3, "eth002", ETHERNET)];
        let watcher_fut = respond_to_watcher_with_interfaces(
            watcher_stream,
            interfaces_existing,
            Some((interfaces_new, fnet_interfaces::Event::Existing)),
        );

        let (err, ()) = futures::join!(event_loop_fut, watcher_fut);
        // The second existing properties has interface id 3.
        assert_matches!(
            err,
            InterfacesEventLoopError::Netstack(
                NetstackError::ExistingEventReceived(properties)
            ) if properties.id.get() == 3
        );
    }

    #[test_case(ETHERNET, false, 0)]
    #[test_case(ETHERNET, true, IFF_UP | IFF_RUNNING)]
    #[test_case(WLAN, false, 0)]
    #[test_case(WLAN, true, IFF_UP | IFF_RUNNING)]
    #[test_case(WLAN_AP, false, 0)]
    #[test_case(WLAN_AP, true, IFF_UP | IFF_RUNNING)]
    #[test_case(PPP, false, 0)]
    #[test_case(PPP, true, IFF_UP | IFF_RUNNING)]
    #[test_case(LOOPBACK, false, IFF_LOOPBACK)]
    #[test_case(LOOPBACK, true, IFF_UP | IFF_RUNNING | IFF_LOOPBACK)]
    fn test_interface_conversion(
        device_class: fnet_interfaces::DeviceClass,
        online: bool,
        flags: u32,
    ) {
        let interface_id = 1;
        let interface_name: String = "test".into();

        let interface =
            create_interface(interface_id, interface_name.clone(), device_class, online, vec![]);
        let actual: NetlinkLinkMessage = interface.try_into().unwrap();

        let link_layer_type = device_class_to_link_type(device_class);
        let nlas = create_nlas(interface_name, link_layer_type, online);
        let expected = create_netlink_link_message(interface_id, link_layer_type, flags, nlas);
        assert_eq!(actual, expected);
    }

    #[fuchsia::test]
    fn test_oversized_interface_id_link_address_conversion() {
        let invalid_interface_id = (u32::MAX as u64) + 1;
        let interface =
            create_interface(invalid_interface_id, "test".into(), ETHERNET, true, vec![]);

        let actual_link_message: Result<NetlinkLinkMessage, NetlinkLinkMessageConversionError> =
            interface.clone().try_into();
        assert_eq!(
            actual_link_message,
            Err(NetlinkLinkMessageConversionError::InvalidInterfaceId(invalid_interface_id))
        );

        assert_eq!(
            interface_properties_to_address_messages(interface),
            Err(NetlinkAddressMessageConversionError::InvalidInterfaceId(invalid_interface_id))
        );
    }

    #[fuchsia::test]
    fn test_interface_to_address_conversion() {
        let interface_name: String = "test".into();
        let interface_id = 1;

        let interface =
            create_interface_with_addresses(interface_id, interface_name.clone(), ETHERNET, true);
        let actual = interface_properties_to_address_messages(interface).unwrap();

        let expected =
            create_default_address_messages(interface_id, interface_name, IFA_F_PERMANENT);
        assert_eq!(actual, expected);
    }

    #[fuchsia::test]
    async fn test_deliver_updates() {
        const LO_INTERFACE_ID: u64 = 1;
        const LO_NAME: &str = "lo";
        const ETH_INTERFACE_ID: u64 = 2;
        const ETH_NAME: &str = "eth";
        const WLAN_INTERFACE_ID: u64 = 3;
        const WLAN_NAME: &str = "wlan";

        let (mut right_sink, right_client) =
            crate::client::testutil::new_fake_client::<NetlinkRoute>(&[ModernGroup(RTNLGRP_LINK)]);
        let (mut wrong_sink, wrong_client) = crate::client::testutil::new_fake_client::<NetlinkRoute>(
            &[ModernGroup(RTNLGRP_LINK + 1)],
        );
        let Setup { event_loop, mut watcher_stream } = setup_with_route_clients({
            let route_clients = ClientTable::default();
            route_clients.add_client(right_client);
            route_clients.add_client(wrong_client);
            route_clients
        });
        let event_loop_fut = event_loop.run().fuse();
        futures::pin_mut!(event_loop_fut);

        let watcher_stream_fut = respond_to_watcher(
            watcher_stream.by_ref(),
            [
                fnet_interfaces::Event::Existing(fnet_interfaces::Properties {
                    id: Some(LO_INTERFACE_ID),
                    name: Some(LO_NAME.to_string()),
                    device_class: Some(LOOPBACK),
                    online: Some(false),
                    addresses: Some(Vec::new()),
                    has_default_ipv4_route: Some(false),
                    has_default_ipv6_route: Some(false),
                    ..Default::default()
                }),
                fnet_interfaces::Event::Existing(fnet_interfaces::Properties {
                    id: Some(ETH_INTERFACE_ID),
                    name: Some(ETH_NAME.to_string()),
                    device_class: Some(ETHERNET),
                    online: Some(false),
                    addresses: Some(Vec::new()),
                    has_default_ipv4_route: Some(false),
                    has_default_ipv6_route: Some(false),
                    ..Default::default()
                }),
                fnet_interfaces::Event::Idle(fnet_interfaces::Empty),
            ],
        );
        futures::select! {
            () = watcher_stream_fut.fuse() => {},
            err = event_loop_fut => unreachable!("eventloop should not return: {err:?}"),
        }
        assert_eq!(&right_sink.take_messages()[..], &[]);
        assert_eq!(&wrong_sink.take_messages()[..], &[]);

        // Note that we provide the stream by value so that it is dropped/closed
        // after this round of updates is sent to the event loop. We wait for
        // the eventloop to terminate below to indicate that all updates have
        // been received and handled so that we can properly assert the sent
        // messages.
        let watcher_stream_fut = respond_to_watcher(
            watcher_stream,
            [
                fnet_interfaces::Event::Added(fnet_interfaces::Properties {
                    id: Some(WLAN_INTERFACE_ID),
                    name: Some(WLAN_NAME.to_string()),
                    device_class: Some(WLAN),
                    online: Some(false),
                    addresses: Some(Vec::new()),
                    has_default_ipv4_route: Some(false),
                    has_default_ipv6_route: Some(false),
                    ..Default::default()
                }),
                fnet_interfaces::Event::Changed(fnet_interfaces::Properties {
                    id: Some(LO_INTERFACE_ID),
                    online: Some(true),
                    ..Default::default()
                }),
                fnet_interfaces::Event::Removed(ETH_INTERFACE_ID),
            ],
        );
        let ((), err) = futures::future::join(watcher_stream_fut, event_loop_fut).await;
        assert_matches!(
            err,
            InterfacesEventLoopError::Fidl(FidlError::EventStream(
                fidl::Error::ClientChannelClosed { .. },
            ))
        );
        assert_eq!(
            &right_sink.take_messages()[..],
            &[
                create_netlink_link_message(
                    WLAN_INTERFACE_ID,
                    ARPHRD_ETHER,
                    0,
                    create_nlas(WLAN_NAME.to_string(), ARPHRD_ETHER, false),
                )
                .into_rtnl_new_link(),
                create_netlink_link_message(
                    LO_INTERFACE_ID,
                    ARPHRD_LOOPBACK,
                    IFF_UP | IFF_RUNNING | IFF_LOOPBACK,
                    create_nlas(LO_NAME.to_string(), ARPHRD_LOOPBACK, true),
                )
                .into_rtnl_new_link(),
                create_netlink_link_message(
                    ETH_INTERFACE_ID,
                    ARPHRD_ETHER,
                    0,
                    create_nlas(ETH_NAME.to_string(), ARPHRD_ETHER, false),
                )
                .into_rtnl_del_link(),
            ],
        );
        assert_eq!(&wrong_sink.take_messages()[..], &[]);
    }
}
