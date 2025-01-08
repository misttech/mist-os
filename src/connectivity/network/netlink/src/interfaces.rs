// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A module for managing RTM_LINK and RTM_ADDR information by generating
//! RTM_LINK and RTM_ADDR Netlink messages based on events received from
//! Netstack's interface watcher.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::net::IpAddr;
use std::num::{NonZeroU32, NonZeroU64};

use fidl_fuchsia_net_ext::IntoExt as _;
use fidl_fuchsia_net_interfaces_admin::{
    self as fnet_interfaces_admin, AddressRemovalReason, InterfaceRemovedReason,
};
use fidl_fuchsia_net_interfaces_ext::admin::{
    wait_for_address_added_event, AddressStateProviderError, TerminalError,
};
use fidl_fuchsia_net_interfaces_ext::{self as fnet_interfaces_ext, Update as _};
use {
    fidl_fuchsia_net as fnet, fidl_fuchsia_net_interfaces as fnet_interfaces,
    fidl_fuchsia_net_root as fnet_root,
};

use assert_matches::assert_matches;
use derivative::Derivative;
use either::Either;
use futures::channel::oneshot;
use futures::StreamExt as _;
use linux_uapi::{
    net_device_flags_IFF_LOOPBACK, net_device_flags_IFF_LOWER_UP, net_device_flags_IFF_RUNNING,
    net_device_flags_IFF_UP, rtnetlink_groups_RTNLGRP_IPV4_IFADDR,
    rtnetlink_groups_RTNLGRP_IPV6_IFADDR, rtnetlink_groups_RTNLGRP_LINK,
    rtnetlink_groups_RTNLGRP_ND_USEROPT, ARPHRD_6LOWPAN, ARPHRD_ETHER, ARPHRD_LOOPBACK, ARPHRD_PPP,
    ARPHRD_VOID,
};
use net_types::ip::{AddrSubnetEither, IpVersion, Ipv6Addr};
use netlink_packet_core::{NetlinkMessage, NLM_F_MULTIPART};
use netlink_packet_route::address::{
    AddressAttribute, AddressFlags, AddressHeader, AddressHeaderFlags, AddressMessage,
};
use netlink_packet_route::link::{
    LinkAttribute, LinkFlags, LinkHeader, LinkLayerType, LinkMessage, State,
};
use netlink_packet_route::neighbour_discovery_user_option::{
    NeighbourDiscoveryIcmpType, NeighbourDiscoveryIcmpV6Type, NeighbourDiscoveryUserOptionHeader,
    NeighbourDiscoveryUserOptionMessage, Nla,
};
use netlink_packet_route::{AddressFamily, RouteNetlinkMessage};
use packet::records::RecordBuilder;
use packet_formats::icmp::ndp;

use crate::client::{ClientTable, InternalClient};
use crate::errors::WorkerInitializationError;
use crate::logging::{log_debug, log_error, log_info, log_warn};
use crate::messaging::Sender;
use crate::multicast_groups::ModernGroup;
use crate::netlink_packet::errno::Errno;
use crate::netlink_packet::UNSPECIFIED_SEQUENCE_NUMBER;
use crate::protocol_family::route::NetlinkRoute;
use crate::protocol_family::ProtocolFamily;
use crate::util::respond_to_completer;

/// A handler for interface events.
pub trait InterfacesHandler: Send + Sync + 'static {
    /// Handle a new link.
    fn handle_new_link(&mut self, name: &str);

    /// Handle a deleted link.
    fn handle_deleted_link(&mut self, name: &str);
}

/// Represents the ways RTM_*LINK messages may specify an individual link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LinkSpecifier {
    Index(NonZeroU32),
    Name(String),
}

/// Arguments for an RTM_GETLINK [`Request`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum GetLinkArgs {
    /// Dump state for all the links.
    Dump,
    /// Get a specific link.
    Get(LinkSpecifier),
}

/// Arguments for an RTM_SETLINK ['Request`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SetLinkArgs {
    /// The link to update.
    pub(crate) link: LinkSpecifier,
    /// `Some` if the link's admin enabled state should be updated to the
    /// provided `bool`.
    pub(crate) enable: Option<bool>,
}

/// [`Request`] arguments associated with links.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LinkRequestArgs {
    /// RTM_GETLINK
    Get(GetLinkArgs),
    /// RTM_SETLINK
    Set(SetLinkArgs),
}

/// Arguments for an RTM_GETADDR [`Request`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum GetAddressArgs {
    /// Dump state for all addresses with the optional IP version filter.
    Dump { ip_version_filter: Option<IpVersion> },
    // TODO(https://issues.fuchsia.dev/296616404): Support get requests w/
    // filter.
}

/// The address and interface ID arguments for address requests.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct AddressAndInterfaceArgs {
    pub address: AddrSubnetEither,
    pub interface_id: NonZeroU32,
}

/// Arguments for an RTM_NEWADDR [`Request`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct NewAddressArgs {
    /// The address to be added and the interface to add it to.
    pub address_and_interface_id: AddressAndInterfaceArgs,
    /// Indicates whether or not an on-link route should be added for the
    /// address's subnet.
    pub add_subnet_route: bool,
}

/// Arguments for an RTM_DELADDR [`Request`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct DelAddressArgs {
    /// The address to be removed and the interface to remove it from.
    pub address_and_interface_id: AddressAndInterfaceArgs,
}

/// [`Request`] arguments associated with addresses.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum AddressRequestArgs {
    /// RTM_GETADDR
    Get(GetAddressArgs),
    /// RTM_NEWADDR
    New(NewAddressArgs),
    /// RTM_DELADDR
    #[allow(unused)]
    Del(DelAddressArgs),
}

/// The argument(s) for a [`Request`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RequestArgs {
    Link(LinkRequestArgs),
    Address(AddressRequestArgs),
}

/// An error encountered while handling a [`Request`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum RequestError {
    Unknown,
    InvalidRequest,
    UnrecognizedInterface,
    AlreadyExists,
    AddressNotFound,
}

impl RequestError {
    pub(crate) fn into_errno(self) -> Errno {
        match self {
            RequestError::Unknown => {
                log_error!("observed an unknown error, reporting `EINVAL` as the best guess");
                Errno::EINVAL
            }
            RequestError::InvalidRequest => Errno::EINVAL,
            RequestError::UnrecognizedInterface => Errno::ENODEV,
            RequestError::AlreadyExists => Errno::EEXIST,
            RequestError::AddressNotFound => Errno::EADDRNOTAVAIL,
        }
    }
}

fn map_existing_interface_terminal_error(
    e: TerminalError<InterfaceRemovedReason>,
    interface_id: NonZeroU64,
) -> RequestError {
    match e {
        TerminalError::Fidl(e) => {
            // If the channel was closed, then we likely tried to get a control
            // chandle to an interface that does not exist.
            if !e.is_closed() {
                log_error!(
                    "unexpected interface terminal error for interface ({:?}): {:?}",
                    interface_id,
                    e,
                )
            }
        }
        TerminalError::Terminal(reason) => match reason {
            reason @ (InterfaceRemovedReason::DuplicateName
            | InterfaceRemovedReason::PortAlreadyBound
            | InterfaceRemovedReason::BadPort) => {
                // These errors are only expected when the interface fails to
                // be installed.
                unreachable!(
                    "unexpected interface removed reason {:?} for interface ({:?})",
                    reason, interface_id,
                )
            }
            InterfaceRemovedReason::PortClosed | InterfaceRemovedReason::User => {
                // The interface was removed. Treat this scenario as if the
                // interface did not exist.
            }
            reason => {
                // `InterfaceRemovedReason` is a flexible FIDL enum so we
                // cannot exhaustively match.
                //
                // We don't know what the reason is but we know the interface
                // was removed so just assume that the unrecognized reason is
                // valid and return the same error as if it was removed with
                // `PortClosed`/`User` reasons.
                log_error!(
                    "unrecognized removal reason {:?} from interface {:?}",
                    reason,
                    interface_id
                )
            }
        },
    }

    RequestError::UnrecognizedInterface
}

/// A request associated with links or addresses.
#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub(crate) struct Request<S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>> {
    /// The resource and operation-specific argument(s) for this request.
    pub args: RequestArgs,
    /// The request's sequence number.
    ///
    /// This value will be copied verbatim into any message sent as a result of
    /// this request.
    pub sequence_number: u32,
    /// The client that made the request.
    pub client: InternalClient<NetlinkRoute, S>,
    /// A completer that will have the result of the request sent over.
    pub completer: oneshot::Sender<Result<(), RequestError>>,
}

/// Handles asynchronous work related to RTM_LINK and RTM_ADDR messages.
///
/// Can respond to interface watcher events and RTM_LINK and RTM_ADDR
/// message requests.
pub(crate) struct InterfacesWorkerState<
    H,
    S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>,
> {
    /// A handler for interface events.
    interfaces_handler: H,
    /// An `InterfacesProxy` to get controlling access to interfaces.
    interfaces_proxy: fnet_root::InterfacesProxy,
    /// The current set of clients of NETLINK_ROUTE protocol family.
    route_clients: ClientTable<NetlinkRoute, S>,
    /// The table of interfaces and associated state discovered through the
    /// interfaces watcher.
    pub(crate) interface_properties: BTreeMap<
        u64,
        fnet_interfaces_ext::PropertiesAndState<InterfaceState, fnet_interfaces_ext::AllInterest>,
    >,
}

/// FIDL errors from the interfaces worker.
#[derive(Debug, thiserror::Error)]
pub(crate) enum InterfacesFidlError {
    /// Error in the FIDL event stream.
    #[error("event stream: {0}")]
    EventStream(fidl::Error),
    /// Error in getting interface event stream from state.
    #[error("watcher creation: {0}")]
    WatcherCreation(fnet_interfaces_ext::WatcherCreationError),
}

/// Netstack errors from the interfaces worker.
#[derive(Debug, thiserror::Error)]
pub(crate) enum InterfacesNetstackError {
    /// Event stream ended unexpectedly.
    #[error("event stream ended")]
    EventStreamEnded,
    /// Unexpected event was received from interface watcher.
    #[error("unexpected event: {0:?}")]
    UnexpectedEvent(fnet_interfaces::Event),
    /// Inconsistent state between Netstack and interface properties.
    #[error("update: {0}")]
    Update(fnet_interfaces_ext::UpdateError),
}

#[derive(Debug, Default, Clone)]
pub(crate) struct InterfaceState {
    // `BTreeMap` so that addresses are iterated in deterministic order
    // (useful for tests).
    addresses: BTreeMap<fnet::IpAddress, NetlinkAddressMessage>,
    link_address: Option<Vec<u8>>,
    control: Option<fnet_interfaces_ext::admin::Control>,
}

async fn set_link_address(
    interfaces_proxy: &fnet_root::InterfacesProxy,
    id: NonZeroU64,
    link_address: &mut Option<Vec<u8>>,
) {
    if id.get() == fake_ifb0::FAKE_IFB0_LINK_ID {
        return;
    }

    match interfaces_proxy
        .get_mac(id.get())
        .await
        .expect("netstack should never close its end of `fuchsia.net.root/Interfaces`")
    {
        Ok(None) => {
            // The request succeeded but the interface has no address.
            log_debug!("no MAC address for interface ({id:?})")
        }
        Ok(Some(mac)) => {
            let fnet::MacAddress { octets } = *mac;
            assert_eq!(link_address.replace(octets.to_vec()), None)
        }
        Err(fnet_root::InterfacesGetMacError::NotFound) => {
            // We only get here if the interface has been removed after we
            // learned about it through the interfaces watcher. Do nothing as
            // a removed event should come for this interface shortly.
            log_warn!("failed to get MAC address for interface ({id:?}) with not found error")
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum PendingRequestKind {
    AddAddress(AddressAndInterfaceArgs),
    DelAddress(AddressAndInterfaceArgs),
    DisableInterface(NonZeroU64),
    // TODO(https://issues.fuchsia.dev/290372180): Support Pending
    // "EnableInterface" requests once link_state is available via a FIDL API
}

#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub(crate) struct PendingRequest<S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>> {
    kind: PendingRequestKind,
    client: InternalClient<NetlinkRoute, S>,
    completer: oneshot::Sender<Result<(), RequestError>>,
}

fn build_stub_nduseropt_message(interface_index: u32) -> NetlinkMessage<RouteNetlinkMessage> {
    // These are Google's Public DNS64 servers.
    // https://developers.google.com/speed/public-dns/docs/dns64
    const STUBBED_IPV6_DNS_SERVERS: [Ipv6Addr; 2] = [
        net_declare::net_ip_v6!("2001:4860:4860::6464"),
        net_declare::net_ip_v6!("2001:4860:4860::64"),
    ];

    let ndp_option =
        ndp::options::NdpOptionBuilder::RecursiveDnsServer(ndp::options::RecursiveDnsServer::new(
            ndp::options::RecursiveDnsServer::INFINITE_LIFETIME,
            &STUBBED_IPV6_DNS_SERVERS,
        ));

    // NB: It's important to use [`RecordBuilder`] rather than
    // [`packet::records::options::OptionBuilder`] to ensure that `serialized_len`
    // and `serialize_into` include the kind and length bytes.
    let mut buf = vec![0u8; RecordBuilder::serialized_len(&ndp_option)];
    RecordBuilder::serialize_into(&ndp_option, &mut buf[..]);

    let mut message: NetlinkMessage<RouteNetlinkMessage> =
        RouteNetlinkMessage::NewNeighbourDiscoveryUserOption(
            NeighbourDiscoveryUserOptionMessage::new(
                NeighbourDiscoveryUserOptionHeader::new(
                    interface_index,
                    NeighbourDiscoveryIcmpType::Inet6(
                        NeighbourDiscoveryIcmpV6Type::RouterAdvertisement,
                    ),
                ),
                buf,
                vec![Nla::SourceLinkLocalAddress(
                    net_declare::net_ip_v6!("fe80::1").ipv6_bytes().to_vec(),
                )],
            ),
        )
        .into();
    message.finalize();
    message
}

impl<H: InterfacesHandler, S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>>
    InterfacesWorkerState<H, S>
{
    pub(crate) async fn create(
        mut interfaces_handler: H,
        route_clients: ClientTable<NetlinkRoute, S>,
        interfaces_proxy: fnet_root::InterfacesProxy,
        interfaces_state_proxy: fnet_interfaces::StateProxy,
    ) -> Result<
        (
            Self,
            impl futures::Stream<
                Item = Result<
                    fnet_interfaces_ext::EventWithInterest<fnet_interfaces_ext::AllInterest>,
                    fidl::Error,
                >,
            >,
        ),
        WorkerInitializationError<InterfacesFidlError, InterfacesNetstackError>,
    > {
        let mut if_event_stream = Box::pin(
            fnet_interfaces_ext::event_stream_from_state(
                &interfaces_state_proxy,
                fnet_interfaces_ext::IncludedAddresses::All,
            )
            .map_err(|err| {
                WorkerInitializationError::Fidl(InterfacesFidlError::WatcherCreation(err))
            })?,
        );

        let mut interface_properties = {
            match fnet_interfaces_ext::existing(
                if_event_stream.by_ref(),
                BTreeMap::<u64, fnet_interfaces_ext::PropertiesAndState<InterfaceState, _>>::new(),
            )
            .await
            {
                Ok(props) => props,
                Err(fnet_interfaces_ext::WatcherOperationError::UnexpectedEnd { .. }) => {
                    return Err(WorkerInitializationError::Netstack(
                        InterfacesNetstackError::EventStreamEnded,
                    ));
                }
                Err(fnet_interfaces_ext::WatcherOperationError::EventStream(e)) => {
                    return Err(WorkerInitializationError::Fidl(InterfacesFidlError::EventStream(
                        e,
                    )));
                }
                Err(fnet_interfaces_ext::WatcherOperationError::Update(e)) => {
                    return Err(WorkerInitializationError::Netstack(
                        InterfacesNetstackError::Update(e),
                    ));
                }
                Err(fnet_interfaces_ext::WatcherOperationError::UnexpectedEvent(event)) => {
                    return Err(WorkerInitializationError::Netstack(
                        InterfacesNetstackError::UnexpectedEvent(event),
                    ));
                }
            }
        };

        assert_matches!(
            interface_properties.insert(
                fake_ifb0::FAKE_IFB0_LINK_ID,
                fnet_interfaces_ext::PropertiesAndState {
                    state: InterfaceState {
                        addresses: Default::default(),
                        link_address: Some(vec![0, 0, 0, 0, 0, 0]),
                        control: None,
                    },
                    properties: fake_ifb0::fake_ifb0_properties(),
                },
            ),
            None
        );

        for fnet_interfaces_ext::PropertiesAndState {
            properties,
            state: InterfaceState { addresses, link_address, control: _ },
        } in interface_properties.values_mut()
        {
            set_link_address(&interfaces_proxy, properties.id, link_address).await;

            if let Some(interface_addresses) =
                addresses_optionally_from_interface_properties(properties)
            {
                *addresses = interface_addresses;
            }

            interfaces_handler.handle_new_link(&properties.name);
        }

        Ok((
            InterfacesWorkerState {
                interfaces_handler,
                interfaces_proxy,
                route_clients,
                interface_properties,
            },
            if_event_stream,
        ))
    }

    /// Handles events observed from the interface watcher by updating the
    /// table of discovered interfaces.
    ///
    /// Returns an `InterfaceEventLoopError` when unexpected events occur, or an
    /// `UpdateError` when updates are not consistent with the current state.
    pub(crate) async fn handle_interface_watcher_event(
        &mut self,
        event: fnet_interfaces_ext::EventWithInterest<fnet_interfaces_ext::AllInterest>,
    ) -> Result<(), InterfaceEventHandlerError> {
        let update = match self.interface_properties.update(event) {
            Ok(update) => update,
            Err(e) => return Err(InterfaceEventHandlerError::Update(e.into())),
        };

        match update {
            fnet_interfaces_ext::UpdateResult::Added {
                properties,
                state: InterfaceState { addresses, link_address, control: _ },
            } => {
                set_link_address(&self.interfaces_proxy, properties.id, link_address).await;

                if let Some(message) = NetlinkLinkMessage::optionally_from(properties, link_address)
                {
                    self.route_clients.send_message_to_group(
                        message.into_rtnl_new_link(UNSPECIFIED_SEQUENCE_NUMBER, false),
                        ModernGroup(rtnetlink_groups_RTNLGRP_LINK),
                    )
                }

                // Send address messages after the link message for newly added links
                // so that netlink clients are aware of the interface before sending
                // address messages for an interface.
                if let Some(updated_addresses) =
                    addresses_optionally_from_interface_properties(properties)
                {
                    update_addresses(addresses, updated_addresses, &self.route_clients);
                }

                self.interfaces_handler.handle_new_link(&properties.name);

                log_debug!("processed add/existing event for id {}", properties.id);
            }
            fnet_interfaces_ext::UpdateResult::Changed {
                previous:
                    fnet_interfaces::Properties {
                        online,
                        addresses,
                        id: _,
                        name: _,
                        has_default_ipv4_route: _,
                        has_default_ipv6_route: previously_had_default_ipv6_route,
                        port_class: _,
                        ..
                    },
                current:
                    current @ fnet_interfaces_ext::Properties {
                        id,
                        addresses: _,
                        name: _,
                        port_class: _,
                        online: _,
                        has_default_ipv4_route: _,
                        has_default_ipv6_route,
                    },
                state: InterfaceState { addresses: interface_addresses, link_address, control: _ },
            } => {
                let previously_did_not_have_default_ipv6_route =
                    previously_had_default_ipv6_route.map(|prev| prev == false).unwrap_or(false);

                // If we've newly acquired a default IPv6 route, send a fake NDUSEROPT message with
                // a fake RDNSS record to prompt clients that we've achieved upstream IPv6
                // connectivity.
                // TODO(https://fxbug.dev/341759423): Rather than constructing a stub NDUSEROPT
                // message from a fake RDNSS record, consume real data on this from the netstack
                // once it is exposed.
                if *has_default_ipv6_route && previously_did_not_have_default_ipv6_route {
                    let message = build_stub_nduseropt_message(
                        u32::try_from(id.get()).expect("should fit in u32"),
                    );

                    log_info!(
                        "detected newly-acquired default route for interface {id}, \
                         sending stub NDUSEROPT message"
                    );
                    self.route_clients.send_message_to_group(
                        message,
                        ModernGroup(rtnetlink_groups_RTNLGRP_ND_USEROPT),
                    );
                }

                if online.is_some() {
                    if let Some(message) =
                        NetlinkLinkMessage::optionally_from(current, link_address)
                    {
                        self.route_clients.send_message_to_group(
                            message.into_rtnl_new_link(UNSPECIFIED_SEQUENCE_NUMBER, false),
                            ModernGroup(rtnetlink_groups_RTNLGRP_LINK),
                        )
                    }

                    log_debug!("processed interface link change event for id {}", id);
                };

                // The `is_some` check is not strictly necessary because
                // `update_addresses` will calculate the delta before sending
                // updates but is useful as an optimization when addresses don't
                // change (<avoid allocations and message comparisons that will net
                // no updates).
                if addresses.is_some() {
                    if let Some(updated_addresses) =
                        addresses_optionally_from_interface_properties(current)
                    {
                        update_addresses(
                            interface_addresses,
                            updated_addresses,
                            &self.route_clients,
                        );
                    }

                    log_debug!("processed interface address change event for id {}", id);
                }
            }
            fnet_interfaces_ext::UpdateResult::Removed(
                fnet_interfaces_ext::PropertiesAndState {
                    properties,
                    state: InterfaceState { mut addresses, link_address, control: _ },
                },
            ) => {
                update_addresses(&mut addresses, BTreeMap::new(), &self.route_clients);

                // Send link messages after the address message for removed links
                // so that netlink clients are aware of the interface throughout the
                // address messages.
                if let Some(message) =
                    NetlinkLinkMessage::optionally_from(&properties, &link_address)
                {
                    self.route_clients.send_message_to_group(
                        message.into_rtnl_del_link(UNSPECIFIED_SEQUENCE_NUMBER),
                        ModernGroup(rtnetlink_groups_RTNLGRP_LINK),
                    )
                }

                self.interfaces_handler.handle_deleted_link(&properties.name);

                log_debug!("processed interface remove event for id {}", properties.id);
            }
            fnet_interfaces_ext::UpdateResult::Existing { properties, state: _ } => {
                return Err(InterfaceEventHandlerError::ExistingEventReceived(properties.clone()));
            }
            fnet_interfaces_ext::UpdateResult::NoChange => {}
        }
        Ok(())
    }

    /// Checks whether a `PendingRequest` can be marked completed given the current state of the
    /// worker. If so, notifies the request's completer and returns `None`. If not, returns
    /// the `PendingRequest` as `Some`.
    pub(crate) fn handle_pending_request(
        &self,
        pending_request: PendingRequest<S>,
    ) -> Option<PendingRequest<S>> {
        let PendingRequest { kind, client: _, completer: _ } = &pending_request;
        let contains_addr = |&AddressAndInterfaceArgs { address, interface_id }| {
            // NB: The interface must exist, because we were able to
            // successfully add/remove an address (hence the pending
            // request). The Netstack will send a 'changed' event to
            // reflect the address add/remove before sending a `removed`
            // event for the interface.
            let fnet_interfaces_ext::PropertiesAndState {
                properties: _,
                state: InterfaceState { addresses, link_address: _, control: _ },
            } = self
                .interface_properties
                .get(&interface_id.get().into())
                .expect("interfaces with pending address change should exist");
            let fnet::Subnet { addr, prefix_len: _ } = address.clone().into_ext();
            addresses.contains_key(&addr)
        };

        let done = match kind {
            PendingRequestKind::AddAddress(address_and_interface_args) => {
                contains_addr(address_and_interface_args)
            }
            PendingRequestKind::DelAddress(address_and_interface_args) => {
                !contains_addr(address_and_interface_args)
            }
            PendingRequestKind::DisableInterface(interface_id) => {
                // NB: The interface must exist, because we were able to
                // successfully disabled it (hence the pending request).
                // The Netstack will send a 'changed' event to reflect
                // the disable, before sending a `removed` event.
                let fnet_interfaces_ext::PropertiesAndState { properties, state: _ } =
                    self.interface_properties.get(&interface_id.get()).unwrap_or_else(|| {
                        panic!("interface {interface_id} with pending disable should exist")
                    });
                // Note here we check "is the interface offline" which
                // is a combination of, "is the underlying link state
                // down" and "is the interface admin disabled". This
                // means we cannot know with certainty whether the link
                // is enabled or disabled. we take our best guess here.
                // TODO(https://issues.fuchsia.dev/290372180): Make this
                // check exact once link status is available via a FIDL
                // API.
                !properties.online
            }
        };

        if done {
            log_debug!("completed pending request; req = {pending_request:?}");

            let PendingRequest { kind, client, completer } = pending_request;

            respond_to_completer(client, completer, Ok(()), kind);
            None
        } else {
            // Put the pending request back so that it can be handled later.
            log_debug!("pending request not done yet; req = {pending_request:?}");
            Some(pending_request)
        }
    }

    /// Returns an admistrative control for the interface.
    ///
    /// Returns `None` if the interface is not known by the `EventLoop`.
    fn get_interface_control(
        &mut self,
        interface_id: NonZeroU64,
    ) -> Option<&fnet_interfaces_ext::admin::Control> {
        let interface = self.interface_properties.get_mut(&interface_id.get())?;

        Some(interface.state.control.get_or_insert_with(|| {
            let (control, server_end) = fnet_interfaces_ext::admin::Control::create_endpoints()
                .expect("create Control endpoints");
            self.interfaces_proxy
                .get_admin(interface_id.get().into(), server_end)
                .expect("send get admin request");
            control
        }))
    }

    // Get the associated `PropertiesAndState` for the given `LinkSpecifier`
    fn get_link(
        &self,
        specifier: LinkSpecifier,
    ) -> Option<
        &fnet_interfaces_ext::PropertiesAndState<InterfaceState, fnet_interfaces_ext::AllInterest>,
    > {
        match specifier {
            LinkSpecifier::Index(id) => self.interface_properties.get(&id.get().into()),
            LinkSpecifier::Name(name) => self.interface_properties.values().find(
                |fnet_interfaces_ext::PropertiesAndState { properties, state: _ }| {
                    properties.name == name
                },
            ),
        }
    }

    /// Handles a "RTM_GETLINK" request.
    ///
    /// The resulting "RTM_NEWLINK" messages will be sent directly to the
    /// provided 'client'.
    fn handle_get_link_request(
        &self,
        args: GetLinkArgs,
        sequence_number: u32,
        client: &mut InternalClient<NetlinkRoute, S>,
    ) -> Result<(), RequestError> {
        let (is_dump, interfaces_iter) = match args {
            GetLinkArgs::Dump => {
                let ifaces = self.interface_properties.values();
                (true, Either::Left(ifaces))
            }
            GetLinkArgs::Get(specifier) => {
                let iface = self.get_link(specifier).ok_or(RequestError::UnrecognizedInterface)?;
                (false, Either::Right(std::iter::once(iface)))
            }
        };

        interfaces_iter
            .filter_map(
                |fnet_interfaces_ext::PropertiesAndState {
                     properties,
                     state: InterfaceState { addresses: _, link_address, control: _ },
                 }| {
                    NetlinkLinkMessage::optionally_from(&properties, &link_address)
                },
            )
            .for_each(|message| {
                client.send_unicast(message.into_rtnl_new_link(sequence_number, is_dump))
            });
        Ok(())
    }

    /// Handles a "RTM_SETLINK" request.
    async fn handle_set_link_request(
        &mut self,
        args: SetLinkArgs,
    ) -> Result<Option<PendingRequestKind>, RequestError> {
        let SetLinkArgs { link, enable } = args;
        let id = self.get_link(link).ok_or(RequestError::UnrecognizedInterface)?.properties.id;
        if id.get() == fake_ifb0::FAKE_IFB0_LINK_ID {
            return Ok(None);
        }

        // NB: Only check if their is a change after verifying the provided
        // interface is valid. This is for conformance with Linux which will
        // return ENODEV for invalid devices, even if no-change was requested.
        let Some(enable) = enable else { return Ok(None) };

        let control = self.get_interface_control(id).ok_or(RequestError::UnrecognizedInterface)?;

        if enable {
            let _did_enable = control
                .enable()
                .await
                .map_err(|e| {
                    log_warn!("error enabling interface {id}: {e:?}");
                    map_existing_interface_terminal_error(e, id)
                })?
                .map_err(|e: fnet_interfaces_admin::ControlEnableError| {
                    // `ControlEnableError` is currently an empty flexible enum.
                    // It's not possible to know what went wrong.
                    log_error!("failed to enable interface {id} for unknown reason: {e:?}");
                    RequestError::Unknown
                })?;
            // TODO(https://issues.fuchsia.dev/290372180): Synchronize this
            // request with observed changes from the watcher, once link status
            // is available via a FIDL API.
            Ok(None)
        } else {
            let did_disable = control
                .disable()
                .await
                .map_err(|e| {
                    log_warn!("error disabling interface {id}: {e:?}");
                    map_existing_interface_terminal_error(e, id)
                })?
                .map_err(|e: fnet_interfaces_admin::ControlDisableError| {
                    // `ControlDisableError` is currently an empty flexible enum.
                    // It's not possible to know what went wrong,
                    log_error!("failed to disable interface {id} for unknown reason: {e:?}");
                    RequestError::Unknown
                })?;
            Ok(did_disable.then_some(PendingRequestKind::DisableInterface(id)))
        }
    }

    /// Handles a new address request.
    ///
    /// Returns the address and interface ID if the address was successfully
    /// added so that the caller can make sure their local state (from the
    /// interfaces watcher) has sent an event holding the added address.
    async fn handle_new_address_request(
        &mut self,
        NewAddressArgs {
            address_and_interface_id:
                address_and_interface_id @ AddressAndInterfaceArgs { address, interface_id },
            add_subnet_route,
        }: NewAddressArgs,
    ) -> Result<Option<AddressAndInterfaceArgs>, RequestError> {
        if u64::from(interface_id.get()) == fake_ifb0::FAKE_IFB0_LINK_ID {
            return Ok(None);
        }
        let control = self
            .get_interface_control(interface_id.into())
            .ok_or(RequestError::UnrecognizedInterface)?;

        let (asp, asp_server_end) =
            fidl::endpoints::create_proxy::<fnet_interfaces_admin::AddressStateProviderMarker>();
        control
            .add_address(
                &address.into_ext(),
                &fnet_interfaces_admin::AddressParameters {
                    // TODO(https://fxbug.dev/42074223): Update how we add subnet
                    // routes for addresses.
                    add_subnet_route: Some(add_subnet_route),
                    ..fnet_interfaces_admin::AddressParameters::default()
                },
                asp_server_end,
            )
            .map_err(|e| {
                log_warn!("error adding {address} to interface ({interface_id}): {e:?}");
                map_existing_interface_terminal_error(e, interface_id.into())
            })?;

        // Detach the ASP so that the address's lifetime isn't bound to the
        // client end of the ASP.
        //
        // We do this first because `assignment_state_stream` takes ownership
        // of the ASP proxy.
        asp.detach().unwrap_or_else(|e| {
            // Likely failed because the address addition failed or it was
            // immediately removed. Don't fail just yet because we need to check
            // the assignment state & terminal error below.
            log_warn!(
                "error detaching ASP for {} on interface ({}): {:?}",
                address,
                interface_id,
                e
            )
        });

        match wait_for_address_added_event(&mut asp.take_event_stream()).await {
            Ok(()) => {
                log_debug!("{} added on interface ({})", address, interface_id);
                Ok(Some(address_and_interface_id))
            }
            Err(e) => {
                log_warn!(
                    "error waiting for state update for {} on interface ({}): {:?}",
                    address,
                    interface_id,
                    e,
                );

                Err(match e {
                    AddressStateProviderError::AddressRemoved(reason) => match reason {
                        AddressRemovalReason::Invalid | AddressRemovalReason::InvalidProperties => {
                            RequestError::InvalidRequest
                        }
                        AddressRemovalReason::AlreadyAssigned => RequestError::AlreadyExists,
                        reason @ (AddressRemovalReason::DadFailed
                        | AddressRemovalReason::InterfaceRemoved
                        | AddressRemovalReason::UserRemoved) => {
                            // These errors are only returned when the address
                            // is removed after it has been added. We have not
                            // yet observed the initial state so these removal
                            // reasons are unexpected.
                            unreachable!(
                                "expected netstack to send initial state before removing {} on interface ({}) with reason {:?}",
                                address, interface_id, reason,
                            )
                        }
                    },
                    AddressStateProviderError::Fidl(e) => {
                        if !e.is_closed() {
                            log_error!(
                                "unexpected ASP error when adding {} on interface ({}): {:?}",
                                address,
                                interface_id,
                                e,
                            )
                        }

                        RequestError::UnrecognizedInterface
                    }
                    AddressStateProviderError::ChannelClosed => {
                        // If the channel is closed, assume the interface was
                        // removed.
                        RequestError::UnrecognizedInterface
                    }
                })
            }
        }
    }

    /// Handles a delete address request.
    ///
    /// Returns the address and interface ID if the address was successfully
    /// removed so that the caller can make sure their local state (from the
    /// interfaces watcher) has sent an event without the removed address.
    async fn handle_del_address_request(
        &mut self,
        DelAddressArgs {
            address_and_interface_id:
                address_and_interface_id @ AddressAndInterfaceArgs { address, interface_id },
        }: DelAddressArgs,
    ) -> Result<AddressAndInterfaceArgs, RequestError> {
        if u64::from(interface_id.get()) == fake_ifb0::FAKE_IFB0_LINK_ID {
            return Ok(address_and_interface_id);
        }

        let control = self
            .get_interface_control(interface_id.into())
            .ok_or(RequestError::UnrecognizedInterface)?;

        match control.remove_address(&address.into_ext()).await.map_err(|e| {
            log_warn!("error removing {address} from interface ({interface_id}): {e:?}");
            map_existing_interface_terminal_error(e, interface_id.into())
        })? {
            Ok(did_remove) => {
                if did_remove {
                    Ok(address_and_interface_id)
                } else {
                    Err(RequestError::AddressNotFound)
                }
            }
            Err(e) => {
                // `e` is a flexible FIDL enum so we cannot exhaustively match.
                let e: fnet_interfaces_admin::ControlRemoveAddressError = e;
                match e {
                    fnet_interfaces_admin::ControlRemoveAddressErrorUnknown!() => {
                        log_error!(
                            "unrecognized address removal error {:?} for address {} on interface ({})",
                            e, address, interface_id,
                        );

                        // Assume the error was because the request was invalid.
                        Err(RequestError::InvalidRequest)
                    }
                }
            }
        }
    }

    /// Handles a [`Request`].
    ///
    /// Returns a [`PendingRequest`] if state was updated and the caller needs
    /// to make sure the update has been propagated to the local state (the
    /// interfaces watcher has sent an event for our update).
    pub(crate) async fn handle_request(
        &mut self,
        Request { args, sequence_number, mut client, completer }: Request<S>,
    ) -> Option<PendingRequest<S>> {
        log_debug!("handling request {args:?} from {client}");

        let result = match args.clone() {
            RequestArgs::Link(LinkRequestArgs::Get(args)) => {
                self.handle_get_link_request(args, sequence_number, &mut client)
            }
            RequestArgs::Link(LinkRequestArgs::Set(args)) => {
                match self.handle_set_link_request(args).await {
                    Ok(Some(kind)) => return Some(PendingRequest { kind, client, completer }),
                    Ok(None) => Ok(()),
                    Err(e) => Err(e),
                }
            }
            RequestArgs::Address(args) => match args {
                AddressRequestArgs::Get(args) => match args {
                    GetAddressArgs::Dump { ip_version_filter } => {
                        self.interface_properties
                            .values()
                            .map(|iface| iface.state.addresses.values())
                            .flatten()
                            .filter(|NetlinkAddressMessage(message)| {
                                ip_version_filter.map_or(true, |ip_version| {
                                    ip_version.eq(&match message.header.family {
                                        AddressFamily::Inet => IpVersion::V4,
                                        AddressFamily::Inet6 => IpVersion::V6,
                                        family => unreachable!(
                                            "unexpected address family ({:?}); addr = {:?}",
                                            family, message,
                                        ),
                                    })
                                })
                            })
                            .for_each(|message| {
                                client.send_unicast(message.to_rtnl_new_addr(sequence_number, true))
                            });
                        Ok(())
                    }
                },
                AddressRequestArgs::New(args) => {
                    match self.handle_new_address_request(args).await {
                        Ok(None) => Ok(()),
                        Ok(Some(address_and_interface_id)) => {
                            return Some(PendingRequest {
                                kind: PendingRequestKind::AddAddress(address_and_interface_id),
                                client,
                                completer,
                            })
                        }
                        Err(e) => Err(e),
                    }
                }
                AddressRequestArgs::Del(args) => {
                    match self.handle_del_address_request(args).await {
                        Ok(address_and_interface_id) => {
                            return Some(PendingRequest {
                                kind: PendingRequestKind::DelAddress(address_and_interface_id),
                                client,
                                completer,
                            })
                        }
                        Err(e) => Err(e),
                    }
                }
            },
        };

        log_debug!("handled request {args:?} from {client} with result = {result:?}");
        respond_to_completer(client, completer, result, args);
        None
    }
}

/// Errors related to handling interface events.
#[derive(Debug, thiserror::Error)]
pub(crate) enum InterfaceEventHandlerError {
    #[error("interface event handler updated the map with an event, but received an unexpected response: {0:?}")]
    Update(fnet_interfaces_ext::UpdateError),
    #[error("interface event handler attempted to process an event for an interface that already existed: {0:?}")]
    ExistingEventReceived(fnet_interfaces_ext::Properties<fnet_interfaces_ext::AllInterest>),
}

fn update_addresses<S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>>(
    existing_addresses: &mut BTreeMap<fnet::IpAddress, NetlinkAddressMessage>,
    updated_addresses: BTreeMap<fnet::IpAddress, NetlinkAddressMessage>,
    route_clients: &ClientTable<NetlinkRoute, S>,
) {
    enum UpdateKind {
        New,
        Del,
    }

    let send_update = |addr: &NetlinkAddressMessage, kind| {
        let NetlinkAddressMessage(inner) = addr;
        let group = match inner.header.family {
            AddressFamily::Inet => rtnetlink_groups_RTNLGRP_IPV4_IFADDR,
            AddressFamily::Inet6 => rtnetlink_groups_RTNLGRP_IPV6_IFADDR,
            family => {
                unreachable!("unrecognized interface address family ({family:?}); addr = {addr:?}")
            }
        };

        let message = match kind {
            UpdateKind::New => addr.to_rtnl_new_addr(UNSPECIFIED_SEQUENCE_NUMBER, false),
            UpdateKind::Del => addr.to_rtnl_del_addr(UNSPECIFIED_SEQUENCE_NUMBER),
        };

        route_clients.send_message_to_group(message, ModernGroup(group));
    };

    // Send a message to interested listeners only if the address is newly added
    // or its message has changed.
    for (key, message) in updated_addresses.iter() {
        if existing_addresses.get(key) != Some(message) {
            send_update(message, UpdateKind::New)
        }
    }

    existing_addresses.retain(|addr, message| {
        // If the address exists in the latest update, keep it. If it was
        // updated, we will update this map with the updated values below.
        if updated_addresses.contains_key(addr) {
            return true;
        }

        // The address is not present in the interfaces latest update so it
        // has been deleted.
        send_update(message, UpdateKind::Del);

        false
    });

    // Update our set of existing addresses with the latest set known to be
    // assigned to the interface.
    existing_addresses.extend(updated_addresses);
}

/// A wrapper type for the netlink_packet_route `LinkMessage` to enable conversions
/// from [`fnet_interfaces_ext::Properties`]. The addresses component of this
/// struct will be handled separately.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NetlinkLinkMessage(LinkMessage);

// TODO(https://fxbug.dev/387998791): Remove these once blackhole interfaces are implemented.
mod fake_ifb0 {
    use super::*;

    pub(super) const FAKE_IFB0_LINK_NAME: &str = "ifb0";

    pub(super) const FAKE_IFB0_LINK_ID: u64 = 255;

    pub(super) fn fake_ifb0_properties(
    ) -> fnet_interfaces_ext::Properties<fnet_interfaces_ext::AllInterest> {
        fnet_interfaces_ext::Properties {
            id: NonZeroU64::new(FAKE_IFB0_LINK_ID).unwrap(),
            name: FAKE_IFB0_LINK_NAME.to_string(),
            online: true,
            addresses: Vec::new(),
            has_default_ipv4_route: false,
            has_default_ipv6_route: false,
            port_class: fnet_interfaces_ext::PortClass::Virtual,
        }
    }
}

impl NetlinkLinkMessage {
    fn optionally_from(
        properties: &fnet_interfaces_ext::Properties<fnet_interfaces_ext::AllInterest>,
        link_address: &Option<Vec<u8>>,
    ) -> Option<Self> {
        match interface_properties_to_link_message(properties, link_address) {
            Ok(o) => Some(o),
            Err(NetlinkLinkMessageConversionError::InvalidInterfaceId(id)) => {
                log_warn!("Invalid interface id: {:?}", id);
                None
            }
        }
    }

    pub(crate) fn into_rtnl_new_link(
        self,
        sequence_number: u32,
        is_dump: bool,
    ) -> NetlinkMessage<RouteNetlinkMessage> {
        let Self(message) = self;
        let mut msg: NetlinkMessage<RouteNetlinkMessage> =
            RouteNetlinkMessage::NewLink(message).into();
        msg.header.sequence_number = sequence_number;
        if is_dump {
            msg.header.flags |= NLM_F_MULTIPART;
        }
        msg.finalize();
        msg
    }

    fn into_rtnl_del_link(self, sequence_number: u32) -> NetlinkMessage<RouteNetlinkMessage> {
        let Self(message) = self;
        let mut msg: NetlinkMessage<RouteNetlinkMessage> =
            RouteNetlinkMessage::DelLink(message).into();
        msg.header.sequence_number = sequence_number;
        msg.finalize();
        msg
    }
}

// NetlinkLinkMessage conversion related errors.
#[derive(Debug, PartialEq)]
pub(crate) enum NetlinkLinkMessageConversionError {
    // Interface id could not be downcasted to fit into the expected u32.
    InvalidInterfaceId(u64),
}

fn port_class_to_link_type(port_class: fnet_interfaces_ext::PortClass) -> u16 {
    match port_class {
        fnet_interfaces_ext::PortClass::Loopback => ARPHRD_LOOPBACK,
        fnet_interfaces_ext::PortClass::Ethernet
        | fnet_interfaces_ext::PortClass::Bridge
        | fnet_interfaces_ext::PortClass::WlanClient
        | fnet_interfaces_ext::PortClass::WlanAp => ARPHRD_ETHER,
        fnet_interfaces_ext::PortClass::Ppp => ARPHRD_PPP,
        // NB: Virtual devices on fuchsia are overloaded. This may be a
        // tun/tap/no-op interface. Return `ARPHRD_VOID` since we have
        // insufficient information to precisely classify the link_type.
        fnet_interfaces_ext::PortClass::Virtual => ARPHRD_VOID,
        fnet_interfaces_ext::PortClass::Lowpan => ARPHRD_6LOWPAN,
    }
    .try_into()
    .expect("potential values will fit into the u16 range")
}

// Netstack only reports 'online' when the 'admin status' is 'enabled' and the 'link
// state' is UP. IFF_RUNNING represents only `link state` UP, so it is likely that
// there will be cases where a flag should be set to IFF_RUNNING but we can not make
// the determination with the information provided.
//
// Per https://www.kernel.org/doc/html/latest/networking/operstates.html#querying-from-userspace,
//
//   Both admin and operational state can be queried via the netlink operation
//   RTM_GETLINK. It is also possible to subscribe to RTNLGRP_LINK to be
//   notified of updates while the interface is admin up. This is important for
//   setting from userspace.
//
//   These values contain interface state:
//
//   ifinfomsg::if_flags & IFF_UP:
//       Interface is admin up
//
//   ifinfomsg::if_flags & IFF_RUNNING:
//       Interface is in RFC2863 operational state UP or UNKNOWN. This is for
//       backward compatibility, routing daemons, dhcp clients can use this flag
//       to determine whether they should use the interface.
//
//   ifinfomsg::if_flags & IFF_LOWER_UP:
//       Driver has signaled netif_carrier_on()
//
//   ...
const ONLINE_IF_FLAGS: u32 =
    net_device_flags_IFF_UP | net_device_flags_IFF_RUNNING | net_device_flags_IFF_LOWER_UP;

// Implement conversions from `Properties` to `NetlinkLinkMessage`
// which is fallible iff, the interface has an id greater than u32.
fn interface_properties_to_link_message(
    fnet_interfaces_ext::Properties {
        id,
        name,
        port_class,
        online,
        addresses: _,
        has_default_ipv4_route: _,
        has_default_ipv6_route: _,
    }: &fnet_interfaces_ext::Properties<fnet_interfaces_ext::AllInterest>,
    link_address: &Option<Vec<u8>>,
) -> Result<NetlinkLinkMessage, NetlinkLinkMessageConversionError> {
    let online = *online;
    let mut link_header = LinkHeader::default();

    link_header.interface_family = AddressFamily::Unspec;

    // We expect interface ids to safely fit in the range of u32 values.
    let id: u32 = match id.get().try_into() {
        Err(std::num::TryFromIntError { .. }) => {
            return Err(NetlinkLinkMessageConversionError::InvalidInterfaceId(id.clone().into()))
        }
        Ok(id) => id,
    };
    link_header.index = id;

    let link_layer_type = port_class_to_link_type(*port_class);
    link_header.link_layer_type = LinkLayerType::from(link_layer_type);

    let mut flags = 0;
    if online {
        flags |= ONLINE_IF_FLAGS;
    };
    if link_header.link_layer_type == LinkLayerType::Loopback {
        flags |= net_device_flags_IFF_LOOPBACK;
    };

    // SAFETY: This and the following .unwrap() are safe as LinkFlags
    // can hold any valid u32.
    link_header.flags = LinkFlags::from_bits(flags).unwrap();

    // As per netlink_package_route and rtnetlink documentation, this should be set to
    // `0xffff_ffff` and reserved for future use.
    link_header.change_mask = LinkFlags::from_bits(u32::MAX).unwrap();

    // The NLA order follows the list that attributes are listed on the
    // rtnetlink man page.
    // The following fields are used in the options in the NLA, but they do
    // not have any corresponding values in `fnet_interfaces_ext::Properties`.
    //
    // IFLA_BROADCAST
    // IFLA_MTU
    // IFLA_LINK
    // IFLA_QDISC
    // IFLA_STATS
    //
    // There are other NLAs observed via the netlink_packet_route crate, and do
    // not have corresponding values in `fnet_interfaces_ext::Properties`.
    // This list is documented within issuetracker.google.com/283137644.
    let nlas = [
        LinkAttribute::IfName(name.clone()),
        LinkAttribute::Link(link_layer_type.into()),
        // Netstack only exposes enough state to determine between `Up` and `Down`
        // operating state.
        LinkAttribute::OperState(if online { State::Up } else { State::Down }),
    ]
    .into_iter()
    // If the interface has a link-address, include it in the NLAs.
    .chain(link_address.clone().map(LinkAttribute::Address))
    .collect();

    let mut link_message = LinkMessage::default();
    link_message.header = link_header;
    link_message.attributes = nlas;

    return Ok(NetlinkLinkMessage(link_message));
}

/// A wrapper type for the netlink_packet_route `AddressMessage` to enable conversions
/// from [`fnet_interfaces_ext::Properties`] and implement hashing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NetlinkAddressMessage(AddressMessage);

impl NetlinkAddressMessage {
    pub(crate) fn to_rtnl_new_addr(
        &self,
        sequence_number: u32,
        is_dump: bool,
    ) -> NetlinkMessage<RouteNetlinkMessage> {
        let Self(message) = self;
        let mut message: NetlinkMessage<RouteNetlinkMessage> =
            RouteNetlinkMessage::NewAddress(message.clone()).into();
        message.header.sequence_number = sequence_number;
        if is_dump {
            message.header.flags |= NLM_F_MULTIPART;
        }
        message.finalize();
        message
    }

    pub(crate) fn to_rtnl_del_addr(
        &self,
        sequence_number: u32,
    ) -> NetlinkMessage<RouteNetlinkMessage> {
        let Self(message) = self;
        let mut message: NetlinkMessage<RouteNetlinkMessage> =
            RouteNetlinkMessage::DelAddress(message.clone()).into();
        message.header.sequence_number = sequence_number;
        message.finalize();
        message
    }
}

// NetlinkAddressMessage conversion related errors.
#[derive(Debug, PartialEq)]
enum NetlinkAddressMessageConversionError {
    // Interface id could not be downcasted to fit into the expected u32.
    InvalidInterfaceId(u64),
}

fn addresses_optionally_from_interface_properties(
    properties: &fnet_interfaces_ext::Properties<fnet_interfaces_ext::AllInterest>,
) -> Option<BTreeMap<fnet::IpAddress, NetlinkAddressMessage>> {
    match interface_properties_to_address_messages(properties) {
        Ok(o) => Some(o),
        Err(NetlinkAddressMessageConversionError::InvalidInterfaceId(id)) => {
            log_warn!("Invalid interface id: {:?}", id);
            None
        }
    }
}

// Implement conversions from `Properties` to `Vec<NetlinkAddressMessage>`
// which is fallible iff, the interface has an id greater than u32.
fn interface_properties_to_address_messages(
    fnet_interfaces_ext::Properties {
        id,
        name,
        addresses,
        port_class: _,
        online: _,
        has_default_ipv4_route: _,
        has_default_ipv6_route: _,
    }: &fnet_interfaces_ext::Properties<fnet_interfaces_ext::AllInterest>,
) -> Result<BTreeMap<fnet::IpAddress, NetlinkAddressMessage>, NetlinkAddressMessageConversionError>
{
    // We expect interface ids to safely fit in the range of the u32 values.
    let id: u32 = match id.get().try_into() {
        Err(std::num::TryFromIntError { .. }) => {
            return Err(NetlinkAddressMessageConversionError::InvalidInterfaceId(id.clone().into()))
        }
        Ok(id) => id,
    };

    let address_messages = addresses
        .into_iter()
        .map(
            |fnet_interfaces_ext::Address {
                 addr: fnet::Subnet { addr, prefix_len },
                 valid_until: _,
                 preferred_lifetime_info: _,
                 assignment_state,
             }| {
                let mut addr_header = AddressHeader::default();

                let (family, addr_bytes) = match addr {
                    fnet::IpAddress::Ipv4(ip_addr) => {
                        (AddressFamily::Inet, IpAddr::V4(ip_addr.addr.into()))
                    }
                    fnet::IpAddress::Ipv6(ip_addr) => {
                        (AddressFamily::Inet6, IpAddr::V6(ip_addr.addr.into()))
                    }
                };

                // The possible constants below are in the range of u8-accepted values, so they can
                // be safely casted to a u8.
                addr_header.family = family.try_into().expect("should fit into u8");
                addr_header.prefix_len = *prefix_len;

                // TODO(https://issues.fuchsia.dev/284980862): Determine proper
                // mapping from Netstack properties to address flags.
                let flags = AddressHeaderFlags::Permanent
                    | match assignment_state {
                        fnet_interfaces::AddressAssignmentState::Assigned => {
                            AddressHeaderFlags::empty()
                        }
                        fnet_interfaces::AddressAssignmentState::Tentative
                        | fnet_interfaces::AddressAssignmentState::Unavailable => {
                            // There is no equivalent `IFA_F_` flag for
                            // `Unavailable` so we treat it as tentative to
                            // signal that the address is installed but not
                            // considered assigned.
                            AddressHeaderFlags::Tentative
                        }
                    };
                addr_header.flags = flags;
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
                    AddressAttribute::Address(addr_bytes),
                    AddressAttribute::Label(name.clone()),
                    // SAFETY: This unwrap is safe because AddressFlags overlaps with
                    // AddressHeaderFlags.
                    AddressAttribute::Flags(AddressFlags::from_bits(flags.bits().into()).unwrap()),
                ];

                let mut addr_message = AddressMessage::default();
                addr_message.header = addr_header;
                addr_message.attributes = nlas;
                (addr.clone(), NetlinkAddressMessage(addr_message))
            },
        )
        .collect();

    Ok(address_messages)
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;

    use std::sync::{Arc, Mutex};

    use futures::channel::mpsc;
    use futures::future::Future;
    use futures::stream::Stream;
    use futures::TryStreamExt as _;
    use net_declare::{fidl_subnet, net_addr_subnet};

    use crate::eventloop::{EventLoopComponent, IncludedWorkers, Optional, Required};
    use crate::messaging::testutil::FakeSender;

    pub(crate) const LO_INTERFACE_ID: u64 = 1;
    pub(crate) const LO_NAME: &str = "lo";
    pub(crate) const ETH_INTERFACE_ID: u64 = 2;
    pub(crate) const ETH_NAME: &str = "eth";
    pub(crate) const WLAN_INTERFACE_ID: u64 = 3;
    pub(crate) const WLAN_NAME: &str = "wlan";
    pub(crate) const PPP_INTERFACE_ID: u64 = 4;
    pub(crate) const PPP_NAME: &str = "ppp";

    pub(crate) const BRIDGE: fnet_interfaces_ext::PortClass =
        fnet_interfaces_ext::PortClass::Bridge;
    pub(crate) const ETHERNET: fnet_interfaces_ext::PortClass =
        fnet_interfaces_ext::PortClass::Ethernet;
    pub(crate) const WLAN_CLIENT: fnet_interfaces_ext::PortClass =
        fnet_interfaces_ext::PortClass::WlanClient;
    pub(crate) const WLAN_AP: fnet_interfaces_ext::PortClass =
        fnet_interfaces_ext::PortClass::WlanAp;
    pub(crate) const PPP: fnet_interfaces_ext::PortClass = fnet_interfaces_ext::PortClass::Ppp;
    pub(crate) const LOOPBACK: fnet_interfaces_ext::PortClass =
        fnet_interfaces_ext::PortClass::Loopback;
    pub(crate) const TEST_V4_ADDR: fnet::Subnet = fidl_subnet!("192.0.2.1/24");
    pub(crate) const TEST_V6_ADDR: fnet::Subnet = fidl_subnet!("2001:db8::1/32");

    // AddrSubnetEither does not have any const methods so we need a method.
    pub(crate) fn test_addr_subnet_v4() -> AddrSubnetEither {
        net_addr_subnet!("192.0.2.1/24")
    }

    // AddrSubnetEither does not have any const methods so we need a method.
    pub(crate) fn test_addr_subnet_v6() -> AddrSubnetEither {
        net_addr_subnet!("2001:db8::1/32")
    }

    // AddrSubnetEither does not have any const methods so we need a method.
    pub(crate) fn add_test_addr_subnet_v4() -> AddrSubnetEither {
        net_addr_subnet!("192.0.2.2/24")
    }

    // AddrSubnetEither does not have any const methods so we need a method.
    pub(crate) fn add_test_addr_subnet_v6() -> AddrSubnetEither {
        net_addr_subnet!("2001:db8::2/32")
    }

    #[derive(Debug, PartialEq, Eq)]
    pub(crate) enum HandledLinkKind {
        New,
        Del,
    }

    #[derive(Debug, PartialEq, Eq)]
    pub(crate) struct HandledLink {
        pub name: String,
        pub kind: HandledLinkKind,
    }

    pub(crate) struct FakeInterfacesHandlerSink(Arc<Mutex<Vec<HandledLink>>>);

    impl FakeInterfacesHandlerSink {
        pub(crate) fn take_handled(&mut self) -> Vec<HandledLink> {
            let Self(rc) = self;
            core::mem::take(&mut *rc.lock().unwrap())
        }
    }

    pub(crate) struct FakeInterfacesHandler(Arc<Mutex<Vec<HandledLink>>>);

    impl FakeInterfacesHandler {
        pub(crate) fn new() -> (FakeInterfacesHandler, FakeInterfacesHandlerSink) {
            let inner = Arc::default();
            (FakeInterfacesHandler(Arc::clone(&inner)), FakeInterfacesHandlerSink(inner))
        }
    }

    impl InterfacesHandler for FakeInterfacesHandler {
        fn handle_new_link(&mut self, name: &str) {
            let Self(rc) = self;
            rc.lock()
                .unwrap()
                .push(HandledLink { name: name.to_string(), kind: HandledLinkKind::New })
        }

        fn handle_deleted_link(&mut self, name: &str) {
            let Self(rc) = self;
            rc.lock()
                .unwrap()
                .push(HandledLink { name: name.to_string(), kind: HandledLinkKind::Del })
        }
    }

    enum OnlyInterfaces {}
    impl crate::eventloop::EventLoopSpec for OnlyInterfaces {
        type InterfacesProxy = Required;
        type InterfacesStateProxy = Required;
        type InterfacesHandler = Required;
        type RouteClients = Required;

        type V4RoutesState = Optional;
        type V6RoutesState = Optional;
        type V4RoutesSetProvider = Optional;
        type V6RoutesSetProvider = Optional;
        type V4RouteTableProvider = Optional;
        type V6RouteTableProvider = Optional;

        type InterfacesWorker = Required;
        type RoutesV4Worker = Optional;
        type RoutesV6Worker = Optional;
        type RuleV4Worker = Optional;
        type RuleV6Worker = Optional;
    }

    pub(crate) struct Setup<E, W> {
        pub event_loop_fut: E,
        pub watcher_stream: W,
        pub request_sink:
            mpsc::Sender<crate::eventloop::UnifiedRequest<FakeSender<RouteNetlinkMessage>>>,
        pub interfaces_request_stream: fnet_root::InterfacesRequestStream,
        pub interfaces_handler_sink: FakeInterfacesHandlerSink,
    }

    pub(crate) fn setup_with_route_clients(
        route_clients: ClientTable<NetlinkRoute, FakeSender<RouteNetlinkMessage>>,
    ) -> Setup<
        impl Future<Output = Result<std::convert::Infallible, anyhow::Error>>,
        impl Stream<Item = fnet_interfaces::WatcherRequest>,
    > {
        let (request_sink, request_stream) = mpsc::channel(1);
        let (interfaces_handler, interfaces_handler_sink) = FakeInterfacesHandler::new();
        let (interfaces_proxy, interfaces) =
            fidl::endpoints::create_proxy::<fnet_root::InterfacesMarker>();
        let (interfaces_state_proxy, interfaces_state) =
            fidl::endpoints::create_proxy::<fnet_interfaces::StateMarker>();
        let event_loop_inputs = crate::eventloop::EventLoopInputs::<_, _, OnlyInterfaces> {
            route_clients: EventLoopComponent::Present(route_clients),
            interfaces_handler: EventLoopComponent::Present(interfaces_handler),
            interfaces_proxy: EventLoopComponent::Present(interfaces_proxy),
            interfaces_state_proxy: EventLoopComponent::Present(interfaces_state_proxy),

            v4_routes_state: EventLoopComponent::Absent(Optional),
            v6_routes_state: EventLoopComponent::Absent(Optional),
            v4_main_route_table: EventLoopComponent::Absent(Optional),
            v6_main_route_table: EventLoopComponent::Absent(Optional),
            v4_route_table_provider: EventLoopComponent::Absent(Optional),
            v6_route_table_provider: EventLoopComponent::Absent(Optional),
            v4_rule_table: EventLoopComponent::Absent(Optional),
            v6_rule_table: EventLoopComponent::Absent(Optional),

            unified_request_stream: request_stream,
        };

        let interfaces_request_stream = interfaces.into_stream();
        let if_stream = interfaces_state.into_stream();
        let watcher_stream = if_stream
            .and_then(|req| match req {
                fnet_interfaces::StateRequest::GetWatcher {
                    options: _,
                    watcher,
                    control_handle: _,
                } => futures::future::ready(Ok(watcher.into_stream())),
            })
            .try_flatten()
            .map(|res| res.expect("watcher stream error"));

        Setup {
            event_loop_fut: async move {
                let event_loop = event_loop_inputs
                    .initialize(IncludedWorkers {
                        interfaces: EventLoopComponent::Present(()),
                        routes_v4: EventLoopComponent::Absent(Optional),
                        routes_v6: EventLoopComponent::Absent(Optional),
                        rules_v4: EventLoopComponent::Absent(Optional),
                        rules_v6: EventLoopComponent::Absent(Optional),
                    })
                    .await?;
                event_loop.run().await
            },
            watcher_stream,
            request_sink,
            interfaces_request_stream,
            interfaces_handler_sink,
        }
    }

    pub(crate) fn setup() -> Setup<
        impl Future<Output = Result<std::convert::Infallible, anyhow::Error>>,
        impl Stream<Item = fnet_interfaces::WatcherRequest>,
    > {
        setup_with_route_clients(ClientTable::default())
    }

    pub(crate) async fn respond_to_watcher<S: Stream<Item = fnet_interfaces::WatcherRequest>>(
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

    pub(crate) fn create_netlink_link_message(
        id: u64,
        link_type: u16,
        flags: u32,
        nlas: Vec<LinkAttribute>,
    ) -> NetlinkLinkMessage {
        let mut link_header = LinkHeader::default();
        link_header.index = id.try_into().expect("should fit into u32");
        link_header.link_layer_type = LinkLayerType::from(link_type);
        link_header.flags = LinkFlags::from_bits(flags).unwrap();
        link_header.change_mask = LinkFlags::from_bits(u32::MAX).unwrap();

        let mut link_message = LinkMessage::default();
        link_message.header = link_header;
        link_message.attributes = nlas;

        NetlinkLinkMessage(link_message)
    }

    pub(crate) fn create_nlas(
        name: String,
        link_type: u16,
        online: bool,
        mac: &Option<fnet::MacAddress>,
    ) -> Vec<LinkAttribute> {
        [
            LinkAttribute::IfName(name),
            LinkAttribute::Link(link_type.into()),
            LinkAttribute::OperState(if online { State::Up } else { State::Down }),
        ]
        .into_iter()
        .chain(mac.map(|fnet::MacAddress { octets }| LinkAttribute::Address(octets.to_vec())))
        .collect()
    }

    pub(crate) fn create_address_message(
        interface_id: u32,
        subnet: fnet::Subnet,
        interface_name: String,
        flags: u32,
    ) -> NetlinkAddressMessage {
        let mut addr_header = AddressHeader::default();
        let (family, addr) = match subnet.addr {
            fnet::IpAddress::Ipv4(ip_addr) => {
                (AddressFamily::Inet, IpAddr::V4(ip_addr.addr.into()))
            }
            fnet::IpAddress::Ipv6(ip_addr) => {
                (AddressFamily::Inet6, IpAddr::V6(ip_addr.addr.into()))
            }
        };
        addr_header.family = family;
        addr_header.prefix_len = subnet.prefix_len;
        addr_header.flags = AddressHeaderFlags::from_bits(flags as u8).unwrap();
        addr_header.index = interface_id;

        let nlas = vec![
            AddressAttribute::Address(addr),
            AddressAttribute::Label(interface_name),
            AddressAttribute::Flags(AddressFlags::from_bits(flags).unwrap()),
        ];

        let mut addr_message = AddressMessage::default();
        addr_message.header = addr_header;
        addr_message.attributes = nlas;
        NetlinkAddressMessage(addr_message)
    }

    pub(crate) fn test_addr_with_assignment_state(
        addr: fnet::Subnet,
        assignment_state: fnet_interfaces::AddressAssignmentState,
    ) -> fnet_interfaces::Address {
        fnet_interfaces_ext::Address::<fnet_interfaces_ext::AllInterest> {
            addr,
            valid_until: fnet_interfaces_ext::PositiveMonotonicInstant::INFINITE_FUTURE,
            preferred_lifetime_info: fnet_interfaces_ext::PreferredLifetimeInfo::preferred_forever(
            ),
            assignment_state,
        }
        .into()
    }

    pub(crate) fn test_addr(addr: fnet::Subnet) -> fnet_interfaces::Address {
        test_addr_with_assignment_state(addr, fnet_interfaces::AddressAssignmentState::Assigned)
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;

    use std::pin::{pin, Pin};

    use fidl::endpoints::{ControlHandle as _, RequestStream as _, Responder as _};
    use fidl_fuchsia_net as fnet;
    use fnet_interfaces::AddressAssignmentState;
    use fuchsia_async::{self as fasync};

    use assert_matches::assert_matches;
    use futures::sink::SinkExt as _;
    use futures::stream::Stream;
    use futures::FutureExt as _;
    use linux_uapi::{rtnetlink_groups_RTNLGRP_IPV4_ROUTE, IFA_F_PERMANENT, IFA_F_TENTATIVE};
    use pretty_assertions::assert_eq;
    use test_case::test_case;

    use crate::messaging::testutil::SentMessage;

    const TEST_SEQUENCE_NUMBER: u32 = 1234;

    fn create_interface(
        id: u64,
        name: String,
        port_class: fnet_interfaces_ext::PortClass,
        online: bool,
        addresses: Vec<fnet_interfaces_ext::Address<fnet_interfaces_ext::AllInterest>>,
    ) -> fnet_interfaces_ext::Properties<fnet_interfaces_ext::AllInterest> {
        fnet_interfaces_ext::Properties {
            id: NonZeroU64::new(id).unwrap(),
            name,
            port_class,
            online,
            addresses,
            has_default_ipv4_route: false,
            has_default_ipv6_route: false,
        }
    }

    fn create_interface_with_addresses(
        id: u64,
        name: String,
        port_class: fnet_interfaces_ext::PortClass,
        online: bool,
    ) -> fnet_interfaces_ext::Properties<fnet_interfaces_ext::AllInterest> {
        let addresses = vec![
            fnet_interfaces_ext::Address {
                addr: TEST_V4_ADDR,
                valid_until: fnet_interfaces_ext::PositiveMonotonicInstant::INFINITE_FUTURE,
                assignment_state: AddressAssignmentState::Assigned,
                preferred_lifetime_info:
                    fnet_interfaces_ext::PreferredLifetimeInfo::preferred_forever(),
            },
            fnet_interfaces_ext::Address {
                addr: TEST_V6_ADDR,
                valid_until: fnet_interfaces_ext::PositiveMonotonicInstant::INFINITE_FUTURE,
                assignment_state: AddressAssignmentState::Assigned,
                preferred_lifetime_info:
                    fnet_interfaces_ext::PreferredLifetimeInfo::preferred_forever(),
            },
        ];
        create_interface(id, name, port_class, online, addresses)
    }

    fn create_default_address_messages(
        interface_id: u64,
        interface_name: String,
        flags: u32,
    ) -> BTreeMap<fnet::IpAddress, NetlinkAddressMessage> {
        let interface_id = interface_id.try_into().expect("should fit into u32");
        BTreeMap::from_iter([
            (
                TEST_V4_ADDR.addr,
                create_address_message(interface_id, TEST_V4_ADDR, interface_name.clone(), flags),
            ),
            (
                TEST_V6_ADDR.addr,
                create_address_message(interface_id, TEST_V6_ADDR, interface_name, flags),
            ),
        ])
    }

    fn get_fake_interface(
        id: u64,
        name: &'static str,
        port_class: fnet_interfaces_ext::PortClass,
    ) -> fnet_interfaces_ext::Properties<fnet_interfaces_ext::AllInterest> {
        fnet_interfaces_ext::Properties {
            id: id.try_into().unwrap(),
            name: name.to_string(),
            port_class,
            online: true,
            addresses: Vec::new(),
            has_default_ipv4_route: false,
            has_default_ipv6_route: false,
        }
    }

    async fn respond_to_watcher_with_interfaces<
        S: Stream<Item = fnet_interfaces::WatcherRequest>,
    >(
        stream: S,
        existing_interfaces: impl IntoIterator<
            Item = fnet_interfaces_ext::Properties<fnet_interfaces_ext::AllInterest>,
        >,
        new_interfaces: Option<(
            impl IntoIterator<Item = fnet_interfaces_ext::Properties<fnet_interfaces_ext::AllInterest>>,
            fn(fnet_interfaces::Properties) -> fnet_interfaces::Event,
        )>,
    ) {
        let map_fn = |fnet_interfaces_ext::Properties {
                          id,
                          name,
                          port_class,
                          online,
                          addresses,
                          has_default_ipv4_route,
                          has_default_ipv6_route,
                      }| {
            fnet_interfaces::Properties {
                id: Some(id.get()),
                name: Some(name),
                port_class: Some(port_class.into()),
                online: Some(online),
                addresses: Some(
                    addresses.into_iter().map(fnet_interfaces::Address::from).collect(),
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
        let Setup {
            event_loop_fut,
            watcher_stream,
            request_sink: _,
            interfaces_request_stream,
            interfaces_handler_sink: _,
        } = setup();
        let interfaces = vec![get_fake_interface(1, "lo", LOOPBACK)];
        let watcher_fut =
            respond_to_watcher_with_interfaces(watcher_stream, interfaces, None::<(Option<_>, _)>);
        let root_interfaces_fut = expect_only_get_mac_root_requests_fut(interfaces_request_stream);

        let (err, (), ()) = futures::join!(event_loop_fut, watcher_fut, root_interfaces_fut);
        assert_matches!(
            err.unwrap_err().downcast::<crate::eventloop::EventStreamError>().unwrap(),
            crate::eventloop::EventStreamError::Interfaces(fidl::Error::ClientChannelClosed { .. })
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_event_loop_errors_duplicate_events() {
        let Setup {
            event_loop_fut,
            watcher_stream,
            request_sink: _,
            interfaces_request_stream: _,
            interfaces_handler_sink: _,
        } = setup();
        let interfaces =
            vec![get_fake_interface(1, "lo", LOOPBACK), get_fake_interface(1, "lo", LOOPBACK)];
        let watcher_fut =
            respond_to_watcher_with_interfaces(watcher_stream, interfaces, None::<(Option<_>, _)>);

        let (err, ()) = futures::join!(event_loop_fut, watcher_fut);
        // The event being sent again as existing has interface id 1.
        assert_matches!(
            err.unwrap_err()
                .downcast::<WorkerInitializationError<InterfacesFidlError, InterfacesNetstackError>>()
                .unwrap(),
            WorkerInitializationError::Netstack(
                InterfacesNetstackError::Update(
                    fnet_interfaces_ext::UpdateError::DuplicateExisting(properties)
                )
            )
            if properties.id.unwrap() == 1
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_event_loop_errors_duplicate_adds() {
        let Setup {
            event_loop_fut,
            watcher_stream,
            request_sink: _,
            interfaces_request_stream,
            interfaces_handler_sink: _,
        } = setup();
        let interfaces_existing = vec![get_fake_interface(1, "lo", LOOPBACK)];
        let interfaces_new = vec![get_fake_interface(1, "lo", LOOPBACK)];
        let watcher_fut = respond_to_watcher_with_interfaces(
            watcher_stream,
            interfaces_existing,
            Some((interfaces_new, fnet_interfaces::Event::Added)),
        );
        let root_interfaces_fut = expect_only_get_mac_root_requests_fut(interfaces_request_stream);

        let (result, (), ()) = futures::join!(event_loop_fut, watcher_fut, root_interfaces_fut);
        // The properties that are being added again has interface id 1.
        assert_matches!(
            result.unwrap_err()
                .downcast::<InterfaceEventHandlerError>().unwrap(),
            InterfaceEventHandlerError::Update(
                fnet_interfaces_ext::UpdateError::DuplicateAdded(properties)
            ) if properties.id.unwrap() == 1
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_event_loop_errors_existing_after_add() {
        let Setup {
            event_loop_fut,
            watcher_stream,
            request_sink: _,
            interfaces_request_stream,
            interfaces_handler_sink: _,
        } = setup();
        let interfaces_existing =
            vec![get_fake_interface(1, "lo", LOOPBACK), get_fake_interface(2, "eth001", ETHERNET)];
        let interfaces_new = vec![get_fake_interface(3, "eth002", ETHERNET)];
        let watcher_fut = respond_to_watcher_with_interfaces(
            watcher_stream,
            interfaces_existing,
            Some((interfaces_new, fnet_interfaces::Event::Existing)),
        );
        let root_interfaces_fut = expect_only_get_mac_root_requests_fut(interfaces_request_stream);

        let (result, (), ()) = futures::join!(event_loop_fut, watcher_fut, root_interfaces_fut);
        // The second existing properties has interface id 3.
        assert_matches!(
            result.unwrap_err()
                .downcast::<InterfaceEventHandlerError>().unwrap(),
            InterfaceEventHandlerError::ExistingEventReceived(properties)
                if properties.id.get() == 3
        );
    }

    #[test_case(ETHERNET, false, 0, ARPHRD_ETHER)]
    #[test_case(ETHERNET, true, ONLINE_IF_FLAGS, ARPHRD_ETHER)]
    #[test_case(WLAN_CLIENT, false, 0, ARPHRD_ETHER)]
    #[test_case(WLAN_CLIENT, true, ONLINE_IF_FLAGS, ARPHRD_ETHER)]
    #[test_case(WLAN_AP, false, 0, ARPHRD_ETHER)]
    #[test_case(WLAN_AP, true, ONLINE_IF_FLAGS, ARPHRD_ETHER)]
    #[test_case(PPP, false, 0, ARPHRD_PPP)]
    #[test_case(PPP, true, ONLINE_IF_FLAGS, ARPHRD_PPP)]
    #[test_case(LOOPBACK, false, net_device_flags_IFF_LOOPBACK, ARPHRD_LOOPBACK)]
    #[test_case(LOOPBACK, true, ONLINE_IF_FLAGS | net_device_flags_IFF_LOOPBACK, ARPHRD_LOOPBACK)]
    #[test_case(BRIDGE, false, 0, ARPHRD_ETHER)]
    #[test_case(BRIDGE, true, ONLINE_IF_FLAGS, ARPHRD_ETHER)]
    fn test_interface_conversion(
        port_class: fnet_interfaces_ext::PortClass,
        online: bool,
        flags: u32,
        expected_link_type: u32,
    ) {
        // This conversion is safe as the link type is actually a u16,
        // but our bindings generator declared it as a u32.
        let expected_link_type = expected_link_type as u16;
        let interface_name = LO_NAME.to_string();
        let interface =
            create_interface(LO_INTERFACE_ID, interface_name.clone(), port_class, online, vec![]);
        let actual: NetlinkLinkMessage =
            interface_properties_to_link_message(&interface, &LO_MAC.map(|a| a.octets.to_vec()))
                .unwrap();

        let nlas = create_nlas(interface_name, expected_link_type, online, &LO_MAC);
        let expected =
            create_netlink_link_message(LO_INTERFACE_ID, expected_link_type, flags, nlas);
        pretty_assertions::assert_eq!(actual, expected);
    }

    #[fuchsia::test]
    fn test_oversized_interface_id_link_address_conversion() {
        let invalid_interface_id = (u32::MAX as u64) + 1;
        let interface =
            create_interface(invalid_interface_id, "test".into(), ETHERNET, true, vec![]);

        let actual_link_message = interface_properties_to_link_message(&interface, &None);
        assert_eq!(
            actual_link_message,
            Err(NetlinkLinkMessageConversionError::InvalidInterfaceId(invalid_interface_id))
        );

        assert_eq!(
            interface_properties_to_address_messages(&interface),
            Err(NetlinkAddressMessageConversionError::InvalidInterfaceId(invalid_interface_id))
        );
    }

    #[fuchsia::test]
    fn test_interface_to_address_conversion() {
        let interface_name: String = "test".into();
        let interface_id = 1;

        let interface =
            create_interface_with_addresses(interface_id, interface_name.clone(), ETHERNET, true);
        let actual = interface_properties_to_address_messages(&interface).unwrap();

        let expected =
            create_default_address_messages(interface_id, interface_name, IFA_F_PERMANENT);
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_into_rtnl_new_link_is_serializable() {
        let link = create_netlink_link_message(0, 0, 0, vec![]);
        let new_link_message = link.into_rtnl_new_link(UNSPECIFIED_SEQUENCE_NUMBER, false);
        let mut buf = vec![0; new_link_message.buffer_len()];
        // Serialize will panic if `new_route_message` is malformed.
        new_link_message.serialize(&mut buf);
    }

    #[test]
    fn test_into_rtnl_del_link_is_serializable() {
        let link = create_netlink_link_message(0, 0, 0, vec![]);
        let del_link_message = link.into_rtnl_del_link(UNSPECIFIED_SEQUENCE_NUMBER);
        let mut buf = vec![0; del_link_message.buffer_len()];
        // Serialize will panic if `del_route_message` is malformed.
        del_link_message.serialize(&mut buf);
    }

    #[fuchsia::test]
    async fn test_deliver_updates() {
        let (mut link_sink, link_client) = crate::client::testutil::new_fake_client::<NetlinkRoute>(
            crate::client::testutil::CLIENT_ID_1,
            &[ModernGroup(rtnetlink_groups_RTNLGRP_LINK)],
        );
        let (mut addr4_sink, addr4_client) = crate::client::testutil::new_fake_client::<NetlinkRoute>(
            crate::client::testutil::CLIENT_ID_2,
            &[ModernGroup(rtnetlink_groups_RTNLGRP_IPV4_IFADDR)],
        );
        let (mut addr6_sink, addr6_client) = crate::client::testutil::new_fake_client::<NetlinkRoute>(
            crate::client::testutil::CLIENT_ID_3,
            &[ModernGroup(rtnetlink_groups_RTNLGRP_IPV6_IFADDR)],
        );
        let (mut other_sink, other_client) = crate::client::testutil::new_fake_client::<NetlinkRoute>(
            crate::client::testutil::CLIENT_ID_4,
            &[ModernGroup(rtnetlink_groups_RTNLGRP_IPV4_ROUTE)],
        );
        let (mut all_sink, all_client) = crate::client::testutil::new_fake_client::<NetlinkRoute>(
            crate::client::testutil::CLIENT_ID_5,
            &[
                ModernGroup(rtnetlink_groups_RTNLGRP_LINK),
                ModernGroup(rtnetlink_groups_RTNLGRP_IPV6_IFADDR),
                ModernGroup(rtnetlink_groups_RTNLGRP_IPV4_IFADDR),
                ModernGroup(rtnetlink_groups_RTNLGRP_ND_USEROPT),
            ],
        );
        let Setup {
            event_loop_fut,
            mut watcher_stream,
            request_sink: _,
            interfaces_request_stream,
            mut interfaces_handler_sink,
        } = setup_with_route_clients({
            let route_clients = ClientTable::default();
            route_clients.add_client(link_client);
            route_clients.add_client(addr4_client);
            route_clients.add_client(addr6_client);
            route_clients.add_client(all_client);
            route_clients.add_client(other_client);
            route_clients
        });
        let event_loop_fut = event_loop_fut.fuse();
        let mut event_loop_fut = pin!(event_loop_fut);
        let root_interfaces_fut =
            expect_only_get_mac_root_requests_fut(interfaces_request_stream).fuse();
        let mut root_interfaces_fut = pin!(root_interfaces_fut);

        // Existing events should never trigger messages to be sent.
        let watcher_stream_fut = respond_to_watcher(
            watcher_stream.by_ref(),
            [
                fnet_interfaces::Event::Existing(fnet_interfaces::Properties {
                    id: Some(LO_INTERFACE_ID),
                    name: Some(LO_NAME.to_string()),
                    port_class: Some(LOOPBACK.into()),
                    online: Some(false),
                    addresses: Some(vec![test_addr_with_assignment_state(
                        TEST_V4_ADDR,
                        fnet_interfaces::AddressAssignmentState::Assigned,
                    )]),
                    has_default_ipv4_route: Some(false),
                    has_default_ipv6_route: Some(false),
                    ..Default::default()
                }),
                fnet_interfaces::Event::Existing(fnet_interfaces::Properties {
                    id: Some(ETH_INTERFACE_ID),
                    name: Some(ETH_NAME.to_string()),
                    port_class: Some(ETHERNET.into()),
                    online: Some(false),
                    addresses: Some(vec![
                        test_addr_with_assignment_state(
                            TEST_V6_ADDR,
                            fnet_interfaces::AddressAssignmentState::Unavailable,
                        ),
                        test_addr_with_assignment_state(
                            TEST_V4_ADDR,
                            fnet_interfaces::AddressAssignmentState::Unavailable,
                        ),
                    ]),
                    has_default_ipv4_route: Some(false),
                    has_default_ipv6_route: Some(false),
                    ..Default::default()
                }),
                fnet_interfaces::Event::Existing(fnet_interfaces::Properties {
                    id: Some(PPP_INTERFACE_ID),
                    name: Some(PPP_NAME.to_string()),
                    port_class: Some(PPP.into()),
                    online: Some(false),
                    addresses: Some(vec![
                        test_addr_with_assignment_state(
                            TEST_V4_ADDR,
                            fnet_interfaces::AddressAssignmentState::Assigned,
                        ),
                        test_addr_with_assignment_state(
                            TEST_V6_ADDR,
                            fnet_interfaces::AddressAssignmentState::Assigned,
                        ),
                    ]),
                    has_default_ipv4_route: Some(false),
                    has_default_ipv6_route: Some(false),
                    ..Default::default()
                }),
                fnet_interfaces::Event::Idle(fnet_interfaces::Empty),
            ],
        );
        futures::select! {
            () = watcher_stream_fut.fuse() => {},
            () = root_interfaces_fut => {
                unreachable!("root interfaces request stream should never end")
            }
            err = event_loop_fut => unreachable!("eventloop should not return: {err:?}"),
        }
        assert_eq!(&link_sink.take_messages()[..], &[]);
        assert_eq!(&addr4_sink.take_messages()[..], &[]);
        assert_eq!(&addr6_sink.take_messages()[..], &[]);
        assert_eq!(&all_sink.take_messages()[..], &[]);
        assert_eq!(&other_sink.take_messages()[..], &[]);

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
                    port_class: Some(WLAN_CLIENT.into()),
                    online: Some(false),
                    addresses: Some(vec![
                        test_addr_with_assignment_state(
                            TEST_V4_ADDR,
                            fnet_interfaces::AddressAssignmentState::Tentative,
                        ),
                        test_addr_with_assignment_state(
                            TEST_V6_ADDR,
                            fnet_interfaces::AddressAssignmentState::Tentative,
                        ),
                    ]),
                    has_default_ipv4_route: Some(false),
                    has_default_ipv6_route: Some(false),
                    ..Default::default()
                }),
                fnet_interfaces::Event::Changed(fnet_interfaces::Properties {
                    id: Some(LO_INTERFACE_ID),
                    online: Some(true),
                    addresses: Some(vec![
                        test_addr_with_assignment_state(
                            TEST_V4_ADDR,
                            fnet_interfaces::AddressAssignmentState::Assigned,
                        ),
                        test_addr_with_assignment_state(
                            TEST_V6_ADDR,
                            fnet_interfaces::AddressAssignmentState::Assigned,
                        ),
                    ]),
                    ..Default::default()
                }),
                fnet_interfaces::Event::Removed(ETH_INTERFACE_ID),
                fnet_interfaces::Event::Changed(fnet_interfaces::Properties {
                    id: Some(PPP_INTERFACE_ID),
                    addresses: Some(Vec::new()),
                    ..Default::default()
                }),
                fnet_interfaces::Event::Changed(fnet_interfaces::Properties {
                    id: Some(WLAN_INTERFACE_ID),
                    has_default_ipv6_route: Some(true),
                    ..Default::default()
                }),
            ],
        );
        let ((), (), err) =
            futures::future::join3(watcher_stream_fut, root_interfaces_fut, event_loop_fut).await;
        assert_matches!(
            err.unwrap_err().downcast::<crate::eventloop::EventStreamError>().unwrap(),
            crate::eventloop::EventStreamError::Interfaces(fidl::Error::ClientChannelClosed { .. })
        );
        assert_eq!(
            interfaces_handler_sink.take_handled(),
            [
                HandledLink { name: LO_NAME.to_string(), kind: HandledLinkKind::New },
                HandledLink { name: ETH_NAME.to_string(), kind: HandledLinkKind::New },
                HandledLink { name: PPP_NAME.to_string(), kind: HandledLinkKind::New },
                // TODO(https://fxbug.dev/387998791): Remove this once blackhole interfaces are
                // implemented.
                HandledLink {
                    name: fake_ifb0::FAKE_IFB0_LINK_NAME.to_string(),
                    kind: HandledLinkKind::New
                },
                HandledLink { name: WLAN_NAME.to_string(), kind: HandledLinkKind::New },
                HandledLink { name: ETH_NAME.to_string(), kind: HandledLinkKind::Del },
            ],
        );
        // Conversion to u16 is safe because 1 < 65535
        let arphrd_ether_u16: u16 = ARPHRD_ETHER as u16;
        // Conversion to u16 is safe because 772 < 65535
        let arphrd_loopback_u16: u16 = ARPHRD_LOOPBACK as u16;
        let wlan_link = SentMessage::multicast(
            create_netlink_link_message(
                WLAN_INTERFACE_ID,
                arphrd_ether_u16,
                0,
                create_nlas(WLAN_NAME.to_string(), arphrd_ether_u16, false, &WLAN_MAC),
            )
            .into_rtnl_new_link(UNSPECIFIED_SEQUENCE_NUMBER, false),
            ModernGroup(rtnetlink_groups_RTNLGRP_LINK),
        );
        let lo_link = SentMessage::multicast(
            create_netlink_link_message(
                LO_INTERFACE_ID,
                arphrd_loopback_u16,
                ONLINE_IF_FLAGS | net_device_flags_IFF_LOOPBACK,
                create_nlas(LO_NAME.to_string(), arphrd_loopback_u16, true, &LO_MAC),
            )
            .into_rtnl_new_link(UNSPECIFIED_SEQUENCE_NUMBER, false),
            ModernGroup(rtnetlink_groups_RTNLGRP_LINK),
        );
        let eth_link = SentMessage::multicast(
            create_netlink_link_message(
                ETH_INTERFACE_ID,
                arphrd_ether_u16,
                0,
                create_nlas(ETH_NAME.to_string(), arphrd_ether_u16, false, &ETH_MAC),
            )
            .into_rtnl_del_link(UNSPECIFIED_SEQUENCE_NUMBER),
            ModernGroup(rtnetlink_groups_RTNLGRP_LINK),
        );
        assert_eq!(
            &link_sink.take_messages()[..],
            &[wlan_link.clone(), lo_link.clone(), eth_link.clone(),],
        );

        let wlan_v4_addr = SentMessage::multicast(
            create_address_message(
                WLAN_INTERFACE_ID.try_into().unwrap(),
                TEST_V4_ADDR,
                WLAN_NAME.to_string(),
                IFA_F_PERMANENT | IFA_F_TENTATIVE,
            )
            .to_rtnl_new_addr(UNSPECIFIED_SEQUENCE_NUMBER, false),
            ModernGroup(rtnetlink_groups_RTNLGRP_IPV4_IFADDR),
        );
        let eth_v4_addr = SentMessage::multicast(
            create_address_message(
                ETH_INTERFACE_ID.try_into().unwrap(),
                TEST_V4_ADDR,
                ETH_NAME.to_string(),
                IFA_F_PERMANENT | IFA_F_TENTATIVE,
            )
            .to_rtnl_del_addr(UNSPECIFIED_SEQUENCE_NUMBER),
            ModernGroup(rtnetlink_groups_RTNLGRP_IPV4_IFADDR),
        );
        let ppp_v4_addr = SentMessage::multicast(
            create_address_message(
                PPP_INTERFACE_ID.try_into().unwrap(),
                TEST_V4_ADDR,
                PPP_NAME.to_string(),
                IFA_F_PERMANENT,
            )
            .to_rtnl_del_addr(UNSPECIFIED_SEQUENCE_NUMBER),
            ModernGroup(rtnetlink_groups_RTNLGRP_IPV4_IFADDR),
        );
        assert_eq!(
            &addr4_sink.take_messages()[..],
            &[wlan_v4_addr.clone(), eth_v4_addr.clone(), ppp_v4_addr.clone(),],
        );

        let wlan_v6_addr = SentMessage::multicast(
            create_address_message(
                WLAN_INTERFACE_ID.try_into().unwrap(),
                TEST_V6_ADDR,
                WLAN_NAME.to_string(),
                IFA_F_PERMANENT | IFA_F_TENTATIVE,
            )
            .to_rtnl_new_addr(UNSPECIFIED_SEQUENCE_NUMBER, false),
            ModernGroup(rtnetlink_groups_RTNLGRP_IPV6_IFADDR),
        );
        let lo_v6_addr = SentMessage::multicast(
            create_address_message(
                LO_INTERFACE_ID.try_into().unwrap(),
                TEST_V6_ADDR,
                LO_NAME.to_string(),
                IFA_F_PERMANENT,
            )
            .to_rtnl_new_addr(UNSPECIFIED_SEQUENCE_NUMBER, false),
            ModernGroup(rtnetlink_groups_RTNLGRP_IPV6_IFADDR),
        );
        let eth_v6_addr = SentMessage::multicast(
            create_address_message(
                ETH_INTERFACE_ID.try_into().unwrap(),
                TEST_V6_ADDR,
                ETH_NAME.to_string(),
                IFA_F_PERMANENT | IFA_F_TENTATIVE,
            )
            .to_rtnl_del_addr(UNSPECIFIED_SEQUENCE_NUMBER),
            ModernGroup(rtnetlink_groups_RTNLGRP_IPV6_IFADDR),
        );
        let ppp_v6_addr = SentMessage::multicast(
            create_address_message(
                PPP_INTERFACE_ID.try_into().unwrap(),
                TEST_V6_ADDR,
                PPP_NAME.to_string(),
                IFA_F_PERMANENT,
            )
            .to_rtnl_del_addr(UNSPECIFIED_SEQUENCE_NUMBER),
            ModernGroup(rtnetlink_groups_RTNLGRP_IPV6_IFADDR),
        );
        let nduseropt = SentMessage::multicast(
            build_stub_nduseropt_message(WLAN_INTERFACE_ID.try_into().unwrap()),
            ModernGroup(rtnetlink_groups_RTNLGRP_ND_USEROPT),
        );
        assert_eq!(
            &addr6_sink.take_messages()[..],
            &[wlan_v6_addr.clone(), lo_v6_addr.clone(), eth_v6_addr.clone(), ppp_v6_addr.clone(),],
        );

        assert_eq!(
            &all_sink.take_messages()[..],
            &[
                // New links always appear before their addresses.
                wlan_link,
                wlan_v4_addr,
                wlan_v6_addr,
                lo_link,
                lo_v6_addr,
                // Removed addresses always appear before removed interfaces.
                eth_v4_addr,
                eth_v6_addr,
                eth_link,
                ppp_v4_addr,
                ppp_v6_addr,
                nduseropt,
            ],
        );
        assert_eq!(&other_sink.take_messages()[..], &[]);
    }

    const LO_MAC: Option<fnet::MacAddress> = None;
    const ETH_MAC: Option<fnet::MacAddress> = Some(fnet::MacAddress { octets: [1, 1, 1, 1, 1, 1] });
    const PPP_MAC: Option<fnet::MacAddress> = Some(fnet::MacAddress { octets: [2, 2, 2, 2, 2, 2] });
    const WLAN_MAC: Option<fnet::MacAddress> =
        Some(fnet::MacAddress { octets: [3, 3, 3, 3, 3, 3] });
    // TODO(https://fxbug.dev/387998791): Remove this once blackhole interfaces are implemented.
    const FAKE_IFB0_MAC: Option<fnet::MacAddress> =
        Some(fnet::MacAddress { octets: [0, 0, 0, 0, 0, 0] });

    fn handle_get_mac_root_request_or_panic(req: fnet_root::InterfacesRequest) {
        match req {
            fnet_root::InterfacesRequest::GetMac { id, responder } => {
                let link_address = match id {
                    LO_INTERFACE_ID => LO_MAC,
                    ETH_INTERFACE_ID => ETH_MAC,
                    PPP_INTERFACE_ID => PPP_MAC,
                    WLAN_INTERFACE_ID => WLAN_MAC,
                    fake_ifb0::FAKE_IFB0_LINK_ID => FAKE_IFB0_MAC,
                    id => panic!("unexpected interface ID {id}"),
                };

                responder.send(Ok(link_address.as_ref())).unwrap()
            }
            req => panic!("unexpected request {:?}", req),
        }
    }

    fn expect_only_get_mac_root_requests(
        interfaces_request_stream: fnet_root::InterfacesRequestStream,
    ) -> impl Stream<Item = fnet_interfaces::Event> {
        futures::stream::unfold(interfaces_request_stream, |interfaces_request_stream| async move {
            interfaces_request_stream
                .for_each(|req| async move { handle_get_mac_root_request_or_panic(req.unwrap()) })
                .await;

            None
        })
    }

    async fn expect_only_get_mac_root_requests_fut(
        interfaces_request_stream: fnet_root::InterfacesRequestStream,
    ) {
        expect_only_get_mac_root_requests(interfaces_request_stream)
            .for_each(|item| async move { panic!("unexpected item = {item:?}") })
            .await
    }

    #[derive(Debug, PartialEq)]
    struct TestRequestResult {
        messages: Vec<SentMessage<RouteNetlinkMessage>>,
        waiter_results: Vec<Result<(), RequestError>>,
    }

    /// Test helper to handle a request.
    ///
    /// `root_handler` returns a future that returns an iterator of
    /// `fuchsia.net.interfaces/Event`s to feed to the netlink eventloop's
    /// interfaces watcher after a root API request is handled.
    async fn test_request<
        St: Stream<Item = fnet_interfaces::Event>,
        F: FnOnce(fnet_root::InterfacesRequestStream) -> St,
    >(
        args: impl IntoIterator<Item = RequestArgs>,
        root_handler: F,
    ) -> TestRequestResult {
        test_request_with_initial_state(
            args,
            root_handler,
            InitialState { eth_interface_online: false },
        )
        .await
    }

    #[derive(Clone, Copy, Debug)]
    struct InitialState {
        eth_interface_online: bool,
    }

    /// Test helper to handle a request.
    ///
    /// `root_handler` returns a future that returns an iterator of
    /// `fuchsia.net.interfaces/Event`s to feed to the netlink eventloop's
    /// interfaces watcher after a root API request is handled.
    /// `initial_state` parametrizes the initial state of the interfaces prior to the request being
    /// handled.
    async fn test_request_with_initial_state<
        St: Stream<Item = fnet_interfaces::Event>,
        F: FnOnce(fnet_root::InterfacesRequestStream) -> St,
    >(
        args: impl IntoIterator<Item = RequestArgs>,
        root_handler: F,
        initial_state: InitialState,
    ) -> TestRequestResult {
        let InitialState { eth_interface_online } = initial_state;

        let (mut expected_sink, expected_client) = crate::client::testutil::new_fake_client::<
            NetlinkRoute,
        >(
            crate::client::testutil::CLIENT_ID_1, &[]
        );
        let (mut other_sink, other_client) = crate::client::testutil::new_fake_client::<NetlinkRoute>(
            crate::client::testutil::CLIENT_ID_2,
            &[],
        );
        let Setup {
            event_loop_fut,
            mut watcher_stream,
            request_sink,
            interfaces_request_stream,
            interfaces_handler_sink: _,
        } = setup_with_route_clients({
            let route_clients = ClientTable::default();
            route_clients.add_client(expected_client.clone());
            route_clients.add_client(other_client);
            route_clients
        });
        let event_loop_fut = event_loop_fut.fuse();
        let mut event_loop_fut = pin!(event_loop_fut);

        let watcher_stream_fut = respond_to_watcher(
            watcher_stream.by_ref(),
            [
                fnet_interfaces::Event::Existing(fnet_interfaces::Properties {
                    id: Some(LO_INTERFACE_ID),
                    name: Some(LO_NAME.to_string()),
                    port_class: Some(LOOPBACK.into()),
                    online: Some(true),
                    addresses: Some(vec![test_addr(TEST_V6_ADDR), test_addr(TEST_V4_ADDR)]),
                    has_default_ipv4_route: Some(false),
                    has_default_ipv6_route: Some(false),
                    ..Default::default()
                }),
                fnet_interfaces::Event::Existing(fnet_interfaces::Properties {
                    id: Some(ETH_INTERFACE_ID),
                    name: Some(ETH_NAME.to_string()),
                    port_class: Some(ETHERNET.into()),
                    online: Some(eth_interface_online),
                    addresses: Some(vec![test_addr(TEST_V4_ADDR), test_addr(TEST_V6_ADDR)]),
                    has_default_ipv4_route: Some(false),
                    has_default_ipv6_route: Some(false),
                    ..Default::default()
                }),
                fnet_interfaces::Event::Idle(fnet_interfaces::Empty),
            ],
        );
        futures::select_biased! {
            err = event_loop_fut => unreachable!("eventloop should not return: {err:?}"),
            () = watcher_stream_fut.fuse() => {},
        }
        assert_eq!(&expected_sink.take_messages()[..], &[]);
        assert_eq!(&other_sink.take_messages()[..], &[]);

        let expected_client = &expected_client;
        let fut = futures::stream::iter(args).fold(
            (Vec::new(), request_sink),
            |(mut results, mut request_sink), args| async move {
                let (completer, waiter) = oneshot::channel();
                request_sink
                    .send(crate::eventloop::UnifiedRequest::InterfacesRequest(Request {
                        args,
                        sequence_number: TEST_SEQUENCE_NUMBER,
                        client: expected_client.clone(),
                        completer,
                    }))
                    .await
                    .unwrap();
                results.push(waiter.await.unwrap());
                (results, request_sink)
            },
        );
        // Handle root API requests then feed the returned
        // `fuchsia.net.interfaces/Event`s to the watcher.
        let watcher_fut = root_handler(interfaces_request_stream).map(Ok).forward(
            futures::sink::unfold(watcher_stream.by_ref(), |st, event| async {
                respond_to_watcher(st.by_ref(), [event]).await;
                Ok::<_, std::convert::Infallible>(st)
            }),
        );
        let waiter_results = futures::select_biased! {
            res = futures::future::join(watcher_fut, event_loop_fut) => {
                unreachable!("eventloop/watcher should not return: {res:?}")
            },
            (results, _request_sink) = fut.fuse() => results
        };
        assert_eq!(&other_sink.take_messages()[..], &[]);
        TestRequestResult { messages: expected_sink.take_messages(), waiter_results }
    }

    #[test_case(
        GetLinkArgs::Dump,
        &[LO_INTERFACE_ID, ETH_INTERFACE_ID,
        // TODO(https://fxbug.dev/387998791): Remove this once blackhole interfaces are implemented.
        fake_ifb0::FAKE_IFB0_LINK_ID],
        Ok(()); "dump")]
    #[test_case(
        GetLinkArgs::Get(LinkSpecifier::Index(
            NonZeroU32::new(LO_INTERFACE_ID.try_into().unwrap()).unwrap())),
        &[LO_INTERFACE_ID],
        Ok(()); "id")]
    #[test_case(
        GetLinkArgs::Get(LinkSpecifier::Index(
            NonZeroU32::new(WLAN_INTERFACE_ID.try_into().unwrap()).unwrap())),
        &[],
        Err(RequestError::UnrecognizedInterface); "id_not_found")]
    #[test_case(
        GetLinkArgs::Get(LinkSpecifier::Name(LO_NAME.to_string())),
        &[LO_INTERFACE_ID],
        Ok(()); "name")]
    #[test_case(
        GetLinkArgs::Get(LinkSpecifier::Name(WLAN_NAME.to_string())),
        &[],
        Err(RequestError::UnrecognizedInterface); "name_not_found")]
    #[fuchsia::test]
    async fn test_get_link(
        args: GetLinkArgs,
        expected_new_links: &[u64],
        expected_result: Result<(), RequestError>,
    ) {
        let is_dump = match args {
            GetLinkArgs::Dump => true,
            GetLinkArgs::Get(_) => false,
        };
        // Conversion to u16 is safe because 1 <= 65535
        let arphrd_ether_u16: u16 = ARPHRD_ETHER as u16;
        // Conversion to u16 is safe because 772 <= 65535
        let arphrd_loopback_u16: u16 = ARPHRD_LOOPBACK as u16;
        // Conversion to u16 is safe because this is 65535 (and the safety of this casts in a const
        // context is checked by the compiler).
        let arphrd_void_u16: u16 = ARPHRD_VOID as u16;
        let expected_messages = expected_new_links
            .iter()
            .map(|link_id| {
                let msg = match *link_id {
                    LO_INTERFACE_ID => create_netlink_link_message(
                        LO_INTERFACE_ID,
                        arphrd_loopback_u16,
                        ONLINE_IF_FLAGS | net_device_flags_IFF_LOOPBACK,
                        create_nlas(LO_NAME.to_string(), arphrd_loopback_u16, true, &LO_MAC),
                    ),
                    ETH_INTERFACE_ID => create_netlink_link_message(
                        ETH_INTERFACE_ID,
                        arphrd_ether_u16,
                        0,
                        create_nlas(ETH_NAME.to_string(), arphrd_ether_u16, false, &ETH_MAC),
                    ),
                    // TODO(https://fxbug.dev/387998791): Remove this once blackhole interfaces are
                    // implemented.
                    fake_ifb0::FAKE_IFB0_LINK_ID => create_netlink_link_message(
                        fake_ifb0::FAKE_IFB0_LINK_ID,
                        arphrd_void_u16,
                        ONLINE_IF_FLAGS,
                        create_nlas(
                            fake_ifb0::FAKE_IFB0_LINK_NAME.to_string(),
                            arphrd_void_u16,
                            true,
                            &FAKE_IFB0_MAC,
                        ),
                    ),
                    _ => unreachable!("GetLink should only be tested with loopback and ethernet"),
                };
                SentMessage::unicast(msg.into_rtnl_new_link(TEST_SEQUENCE_NUMBER, is_dump))
            })
            .collect();

        assert_eq!(
            test_request(
                [RequestArgs::Link(LinkRequestArgs::Get(args))],
                expect_only_get_mac_root_requests,
            )
            .await,
            TestRequestResult {
                messages: expected_messages,
                waiter_results: vec![expected_result],
            },
        )
    }

    /// Returns a `FnOnce` suitable for use with [`test_request`].
    ///
    /// The closure serves a single `GetAdmin` request for the Ethernet
    /// interface, and handles all subsequent
    /// [`fnet_interfaces_admin::ControlRequest`] by calling the provided
    /// handler.
    // TODO(https://github.com/rust-lang/rust/issues/99697): Remove the
    // `Pin<Box<dyn ...>>` from the return type once Rust supports
    // `impl Fn() -> impl <SomeTrait>` style declarations.
    fn expect_get_admin_with_handler<
        I: IntoIterator<Item = fnet_interfaces::Event> + 'static,
        H: FnMut(fnet_interfaces_admin::ControlRequest) -> I + 'static,
    >(
        admin_handler: H,
    ) -> impl FnOnce(
        fnet_root::InterfacesRequestStream,
    ) -> Pin<Box<dyn Stream<Item = fnet_interfaces::Event>>> {
        move |interfaces_request_stream: fnet_root::InterfacesRequestStream| {
            Box::pin(
                interfaces_request_stream
                    .filter_map(|req| {
                        futures::future::ready(match req.unwrap() {
                            fnet_root::InterfacesRequest::GetAdmin {
                                id,
                                control,
                                control_handle: _,
                            } => {
                                pretty_assertions::assert_eq!(id, ETH_INTERFACE_ID);
                                Some(control.into_stream())
                            }
                            req => {
                                handle_get_mac_root_request_or_panic(req);
                                None
                            }
                        })
                    })
                    .into_future()
                    // This module's implementation is expected to only acquire one
                    // admin control handle per interface, so drop the remaining
                    // stream of admin control request streams.
                    .map(|(admin_control_stream, _stream_of_admin_control_streams)| {
                        admin_control_stream.unwrap()
                    })
                    .flatten_stream()
                    // Handle each Control request with the provided handler.
                    // `scan` transfers ownership of `admin_handle`, which
                    // circumvents some borrow chcker issues we would encounter
                    // with `map`.
                    .scan(admin_handler, |admin_handler, req| {
                        futures::future::ready(Some(futures::stream::iter(admin_handler(
                            req.unwrap(),
                        ))))
                    })
                    .flatten(),
            )
        }
    }

    #[test_case(
        InitialState { eth_interface_online: false },
        SetLinkArgs{
            link: LinkSpecifier::Name(ETH_NAME.to_string()),
            enable: None,
        },
        Ok(true),
        Ok(()); "no_change")]
    #[test_case(
        InitialState { eth_interface_online: false },
        SetLinkArgs{
            link: LinkSpecifier::Name(WLAN_NAME.to_string()),
            enable: None,
        },
        Ok(true),
        Err(RequestError::UnrecognizedInterface); "no_change_name_not_found")]
    #[test_case(
        InitialState { eth_interface_online: false },
        SetLinkArgs {
            link: LinkSpecifier::Index(
                NonZeroU32::new(WLAN_INTERFACE_ID.try_into().unwrap()).unwrap()),
            enable: None,
        },
        Ok(true),
        Err(RequestError::UnrecognizedInterface); "no_change_id_not_found")]
    #[test_case(
        InitialState { eth_interface_online: false },
        SetLinkArgs{
            link: LinkSpecifier::Name(ETH_NAME.to_string()),
            enable: Some(true),
        },
        Ok(false),
        Ok(()); "enable_no_op_succeeds")]
    #[test_case(
        InitialState { eth_interface_online: false },
        SetLinkArgs{
            link: LinkSpecifier::Name(ETH_NAME.to_string()),
            enable: Some(true),
        },
        Ok(true),
        Ok(()); "enable_newly_succeeds")]
    #[test_case(
        InitialState { eth_interface_online: false },
        SetLinkArgs{
            link: LinkSpecifier::Name(WLAN_NAME.to_string()),
            enable: Some(true),
        },
        Ok(true),
        Err(RequestError::UnrecognizedInterface); "enable_not_found")]
    #[test_case(
        InitialState { eth_interface_online: false },
        SetLinkArgs{
            link: LinkSpecifier::Name(ETH_NAME.to_string()),
            enable: Some(true),
        },
        Err(()),
        Err(RequestError::Unknown); "enable_fails")]
    #[test_case(
        InitialState { eth_interface_online: false },
        SetLinkArgs{
            link: LinkSpecifier::Name(ETH_NAME.to_string()),
            enable: Some(false),
        },
        Ok(false),
        Ok(()); "disable_no_op_succeeds")]
    #[test_case(
        InitialState { eth_interface_online: true },
        SetLinkArgs{
            link: LinkSpecifier::Name(ETH_NAME.to_string()),
            enable: Some(false),
        },
        Ok(true),
        Ok(()); "disable_newly_succeeds")]
    #[test_case(
        InitialState { eth_interface_online: false },
        SetLinkArgs{
            link: LinkSpecifier::Name(WLAN_NAME.to_string()),
            enable: Some(false),
        },
        Ok(true),
        Err(RequestError::UnrecognizedInterface); "disable_not_found")]
    #[test_case(
        InitialState { eth_interface_online: false },
        SetLinkArgs{
            link: LinkSpecifier::Name(ETH_NAME.to_string()),
            enable: Some(false),
        },
        Err(()),
        Err(RequestError::Unknown); "disable_fails")]
    #[fuchsia::test]
    async fn test_set_link(
        initial_state: InitialState,
        args: SetLinkArgs,
        control_response: Result<bool, ()>,
        expected_result: Result<(), RequestError>,
    ) {
        let SetLinkArgs { link: _, enable } = args.clone();
        let request = RequestArgs::Link(LinkRequestArgs::Set(args));

        let control_response_clone = control_response.clone();
        let handle_enable =
            move |req: fnet_interfaces_admin::ControlRequest| -> Option<fnet_interfaces::Event> {
                let responder = match req {
                    fnet_interfaces_admin::ControlRequest::Enable { responder } => responder,
                    _ => panic!("unexpected ControlRequest received"),
                };
                match control_response {
                    Err(()) => {
                        responder
                            .send(Err(fnet_interfaces_admin::ControlEnableError::unknown()))
                            .expect("should send response");
                        None
                    }
                    Ok(newly_enabled) => {
                        responder.send(Ok(newly_enabled)).expect("should send response");
                        newly_enabled.then_some(fnet_interfaces::Event::Changed(
                            fnet_interfaces::Properties {
                                id: Some(ETH_INTERFACE_ID),
                                online: Some(true),
                                ..fnet_interfaces::Properties::default()
                            },
                        ))
                    }
                }
            };
        let handle_disable =
            move |req: fnet_interfaces_admin::ControlRequest| -> Option<fnet_interfaces::Event> {
                let responder = match req {
                    fnet_interfaces_admin::ControlRequest::Disable { responder } => responder,
                    _ => panic!("unexpected ControlRequest received"),
                };
                match control_response_clone {
                    Err(()) => {
                        responder
                            .send(Err(fnet_interfaces_admin::ControlDisableError::unknown()))
                            .expect("should send response");
                        None
                    }
                    Ok(newly_disabled) => {
                        responder.send(Ok(newly_disabled)).expect("should send response");
                        newly_disabled.then_some(fnet_interfaces::Event::Changed(
                            fnet_interfaces::Properties {
                                id: Some(ETH_INTERFACE_ID),
                                online: Some(false),
                                ..fnet_interfaces::Properties::default()
                            },
                        ))
                    }
                }
            };

        let test_result = match enable {
            None => {
                test_request_with_initial_state(
                    [request],
                    expect_only_get_mac_root_requests,
                    initial_state,
                )
                .await
            }
            Some(true) => {
                test_request_with_initial_state(
                    [request],
                    expect_get_admin_with_handler(handle_enable),
                    initial_state,
                )
                .await
            }
            Some(false) => {
                test_request_with_initial_state(
                    [request],
                    expect_get_admin_with_handler(handle_disable),
                    initial_state,
                )
                .await
            }
        };

        assert_eq!(
            test_result,
            TestRequestResult {
                // SetLink requests never result in messages. Acks/errors
                // are handled by the caller.
                messages: vec![],
                waiter_results: vec![expected_result],
            },
        )
    }

    #[test_case(Some(IpVersion::V4); "v4")]
    #[test_case(Some(IpVersion::V6); "v6")]
    #[test_case(None; "all")]
    #[fuchsia::test]
    async fn test_get_addr(ip_version_filter: Option<IpVersion>) {
        pretty_assertions::assert_eq!(
            test_request(
                [RequestArgs::Address(AddressRequestArgs::Get(GetAddressArgs::Dump {
                    ip_version_filter
                }))],
                expect_only_get_mac_root_requests,
            )
            .await,
            TestRequestResult {
                messages: [(LO_INTERFACE_ID, LO_NAME), (ETH_INTERFACE_ID, ETH_NAME)]
                    .into_iter()
                    .map(|(id, name)| {
                        [TEST_V4_ADDR, TEST_V6_ADDR]
                            .into_iter()
                            .filter(|fnet::Subnet { addr, prefix_len: _ }| {
                                ip_version_filter.map_or(true, |ip_version| {
                                    ip_version.eq(&match addr {
                                        fnet::IpAddress::Ipv4(_) => IpVersion::V4,
                                        fnet::IpAddress::Ipv6(_) => IpVersion::V6,
                                    })
                                })
                            })
                            .map(move |addr| {
                                SentMessage::unicast(
                                    create_address_message(
                                        id.try_into().unwrap(),
                                        addr,
                                        name.to_string(),
                                        IFA_F_PERMANENT,
                                    )
                                    .to_rtnl_new_addr(TEST_SEQUENCE_NUMBER, true),
                                )
                            })
                    })
                    .flatten()
                    .collect(),
                waiter_results: vec![Ok(())],
            },
        );
    }

    /// Tests RTM_NEWADDR and RTM_DEL_ADDR when the interface is removed,
    /// indicated by the closure of the admin Control's server-end.
    #[test_case(
        test_addr_subnet_v4(),
        None,
        true; "v4_no_terminal_new")]
    #[test_case(
        test_addr_subnet_v6(),
        None,
        true; "v6_no_terminal_new")]
    #[test_case(
        test_addr_subnet_v4(),
        Some(InterfaceRemovedReason::PortClosed),
        true; "v4_port_closed_terminal_new")]
    #[test_case(
        test_addr_subnet_v6(),
        Some(InterfaceRemovedReason::PortClosed),
        true; "v6_port_closed_terminal_new")]
    #[test_case(
        test_addr_subnet_v4(),
        Some(InterfaceRemovedReason::User),
        true; "v4_user_terminal_new")]
    #[test_case(
        test_addr_subnet_v6(),
        Some(InterfaceRemovedReason::User),
        true; "v6_user_terminal_new")]
    #[test_case(
        test_addr_subnet_v4(),
        None,
        false; "v4_no_terminal_del")]
    #[test_case(
        test_addr_subnet_v6(),
        None,
        false; "v6_no_terminal_del")]
    #[test_case(
        test_addr_subnet_v4(),
        Some(InterfaceRemovedReason::PortClosed),
        false; "v4_port_closed_terminal_del")]
    #[test_case(
        test_addr_subnet_v6(),
        Some(InterfaceRemovedReason::PortClosed),
        false; "v6_port_closed_terminal_del")]
    #[test_case(
        test_addr_subnet_v4(),
        Some(InterfaceRemovedReason::User),
        false; "v4_user_terminal_del")]
    #[test_case(
        test_addr_subnet_v6(),
        Some(InterfaceRemovedReason::User),
        false; "v6_user_terminal_del")]
    #[fuchsia::test]
    async fn test_new_del_addr_interface_removed(
        address: AddrSubnetEither,
        removal_reason: Option<InterfaceRemovedReason>,
        is_new: bool,
    ) {
        let interface_id = NonZeroU32::new(LO_INTERFACE_ID.try_into().unwrap()).unwrap();
        let address_and_interface_id = AddressAndInterfaceArgs { address, interface_id };
        pretty_assertions::assert_eq!(
            test_request(
                [if is_new {
                    RequestArgs::Address(AddressRequestArgs::New(NewAddressArgs {
                        address_and_interface_id,
                        add_subnet_route: false,
                    }))
                } else {
                    RequestArgs::Address(AddressRequestArgs::Del(DelAddressArgs {
                        address_and_interface_id,
                    }))
                }],
                |interfaces_request_stream| futures::stream::unfold(
                    interfaces_request_stream,
                    |interfaces_request_stream| async move {
                        interfaces_request_stream
                            .for_each(|req| {
                                futures::future::ready(match req.unwrap() {
                                    fnet_root::InterfacesRequest::GetAdmin {
                                        id,
                                        control,
                                        control_handle: _,
                                    } => {
                                        pretty_assertions::assert_eq!(id, LO_INTERFACE_ID);
                                        let control = control.into_stream();
                                        let control = control.control_handle();
                                        if let Some(reason) = removal_reason {
                                            control.send_on_interface_removed(reason).unwrap()
                                        }
                                        control.shutdown();
                                    }
                                    req => handle_get_mac_root_request_or_panic(req),
                                })
                            })
                            .await;

                        unreachable!("interfaces request stream should not end")
                    },
                ),
            )
            .await,
            TestRequestResult {
                messages: Vec::new(),
                waiter_results: vec![Err(RequestError::UnrecognizedInterface)],
            },
        )
    }

    enum AddressRequestKind {
        New { add_subnet_route: bool },
        Del,
    }

    /// Test that a request for an interface the eventloop does not recognize
    /// fails with an unrecognized interface error.
    #[test_case(
        add_test_addr_subnet_v4(),
        AddressRequestKind::New { add_subnet_route: false }; "v4_new")]
    #[test_case(
        add_test_addr_subnet_v6(),
        AddressRequestKind::New { add_subnet_route: false }; "v6_new")]
    #[test_case(add_test_addr_subnet_v4(), AddressRequestKind::Del; "v4_del")]
    #[test_case(add_test_addr_subnet_v6(), AddressRequestKind::Del; "v6_del")]
    #[fuchsia::test]
    async fn test_unknown_interface_request(address: AddrSubnetEither, kind: AddressRequestKind) {
        let interface_id = NonZeroU32::new(WLAN_INTERFACE_ID.try_into().unwrap()).unwrap();
        let address_and_interface_id = AddressAndInterfaceArgs { address, interface_id };
        pretty_assertions::assert_eq!(
            test_request(
                [match kind {
                    AddressRequestKind::New { add_subnet_route } => {
                        RequestArgs::Address(AddressRequestArgs::New(NewAddressArgs {
                            address_and_interface_id,
                            add_subnet_route,
                        }))
                    }
                    AddressRequestKind::Del => {
                        RequestArgs::Address(AddressRequestArgs::Del(DelAddressArgs {
                            address_and_interface_id,
                        }))
                    }
                }],
                expect_only_get_mac_root_requests,
            )
            .await,
            TestRequestResult {
                messages: Vec::new(),
                waiter_results: vec![Err(RequestError::UnrecognizedInterface)],
            },
        )
    }

    struct TestInterfaceRequestCase<F> {
        address: AddrSubnetEither,
        kind: AddressRequestKind,
        control_request_handler: F,
    }

    impl<F> TestInterfaceRequestCase<F> {
        fn into_request_args_and_handler(self, interface_id: NonZeroU32) -> (RequestArgs, F) {
            let Self { address, kind, control_request_handler } = self;
            let address_and_interface_id = AddressAndInterfaceArgs { address, interface_id };
            let args = match kind {
                AddressRequestKind::New { add_subnet_route } => {
                    RequestArgs::Address(AddressRequestArgs::New(NewAddressArgs {
                        address_and_interface_id,
                        add_subnet_route,
                    }))
                }
                AddressRequestKind::Del => {
                    RequestArgs::Address(AddressRequestArgs::Del(DelAddressArgs {
                        address_and_interface_id,
                    }))
                }
            };

            (args, control_request_handler)
        }
    }

    /// A test helper that calls the (up to two) test cases' callback with a
    /// [`fnet_interfaces_admin::ControlRequest`] as they arrive.
    ///
    /// This implementation makes sure that the the control handle for the
    /// interface is only requested once.
    async fn test_maybe_two_interface_requests_on_single_control<
        St1: Stream<Item = fnet_interfaces::Event>,
        F1: FnMut(fnet_interfaces_admin::ControlRequest) -> St1,
        St2: Stream<Item = fnet_interfaces::Event>,
        F2: FnMut(fnet_interfaces_admin::ControlRequest) -> St2,
    >(
        case1: TestInterfaceRequestCase<F1>,
        case2: Option<TestInterfaceRequestCase<F2>>,
    ) -> TestRequestResult {
        let interface_id = NonZeroU32::new(ETH_INTERFACE_ID.try_into().unwrap()).unwrap();
        let (args1, mut control_request_handler1) =
            case1.into_request_args_and_handler(interface_id);

        let (args2, control_request_handler2) = if let Some(case) = case2 {
            let (args, control_request_handler) = case.into_request_args_and_handler(interface_id);
            (Some(args), Some(control_request_handler))
        } else {
            (None, None)
        };

        test_request([args1].into_iter().chain(args2), |interfaces_request_stream| {
            interfaces_request_stream
                .filter_map(|req| {
                    futures::future::ready(match req.unwrap() {
                        fnet_root::InterfacesRequest::GetAdmin {
                            id,
                            control,
                            control_handle: _,
                        } => {
                            pretty_assertions::assert_eq!(id, ETH_INTERFACE_ID);
                            Some(control.into_stream())
                        }
                        req => {
                            handle_get_mac_root_request_or_panic(req);
                            None
                        }
                    })
                })
                .into_future()
                // This method supports tests that want to make sure that the
                // admin control is only requested once so we drop the remaining
                // stream of admin control request streams.
                .map(|(admin_control_stream, _stream_of_admin_control_streams)| {
                    admin_control_stream.unwrap()
                })
                .flatten_stream()
                .into_future()
                .map(|(admin_control_req, admin_control_stream)| {
                    control_request_handler1(admin_control_req.unwrap().unwrap()).chain(
                        futures::stream::iter(control_request_handler2.map(
                            |mut control_request_handler2| {
                                admin_control_stream
                                    .into_future()
                                    .map(move |(admin_control_req, _admin_control_stream)| {
                                        control_request_handler2(
                                            admin_control_req.unwrap().unwrap(),
                                        )
                                    })
                                    .flatten_stream()
                            },
                        ))
                        .flatten(),
                    )
                })
                .flatten_stream()
        })
        .await
    }

    /// A test helper that calls the callback with a
    /// [`fnet_interfaces_admin::ControlRequest`] as they arrive.
    async fn test_interface_request<
        St: Stream<Item = fnet_interfaces::Event>,
        F: FnMut(fnet_interfaces_admin::ControlRequest) -> St,
    >(
        case: TestInterfaceRequestCase<F>,
    ) -> TestRequestResult {
        test_maybe_two_interface_requests_on_single_control(
            case,
            None::<TestInterfaceRequestCase<fn(_) -> futures::stream::Pending<_>>>,
        )
        .await
    }

    /// An RTM_NEWADDR test helper that calls the callback with a stream of ASP
    /// requests.
    async fn test_new_addr_asp_helper<
        St: Stream<Item = fnet_interfaces::Event>,
        F: Fn(fnet_interfaces_admin::AddressStateProviderRequestStream) -> St,
    >(
        address: AddrSubnetEither,
        add_subnet_route: bool,
        asp_handler: F,
    ) -> TestRequestResult {
        test_interface_request(TestInterfaceRequestCase {
            address,
            kind: AddressRequestKind::New { add_subnet_route },
            control_request_handler: |req| match req {
                fnet_interfaces_admin::ControlRequest::AddAddress {
                    address: got_address,
                    parameters,
                    address_state_provider,
                    control_handle: _,
                } => {
                    pretty_assertions::assert_eq!(got_address, address.into_ext());
                    pretty_assertions::assert_eq!(
                        parameters,
                        fnet_interfaces_admin::AddressParameters {
                            add_subnet_route: Some(add_subnet_route),
                            ..fnet_interfaces_admin::AddressParameters::default()
                        },
                    );
                    asp_handler(address_state_provider.into_stream())
                }
                req => panic!("unexpected request {req:?}"),
            },
        })
        .await
    }

    /// Tests RTM_NEWADDR when the ASP is dropped immediately (doesn't handle
    /// any request).
    #[test_case(test_addr_subnet_v4(); "v4")]
    #[test_case(test_addr_subnet_v6(); "v6")]
    #[fuchsia::test]
    async fn test_new_addr_drop_asp_immediately(address: AddrSubnetEither) {
        pretty_assertions::assert_eq!(
            test_new_addr_asp_helper(address, false, |_asp_request_stream| {
                futures::stream::empty()
            })
            .await,
            TestRequestResult {
                messages: Vec::new(),
                waiter_results: vec![Err(RequestError::UnrecognizedInterface)],
            },
        )
    }

    /// RTM_NEWADDR test helper that exercises the ASP being closed with a
    /// terminal event.
    async fn test_new_addr_failed_helper(
        address: AddrSubnetEither,
        reason: AddressRemovalReason,
    ) -> TestRequestResult {
        test_new_addr_asp_helper(address, true, |asp_request_stream| {
            asp_request_stream.control_handle().send_on_address_removed(reason).unwrap();
            futures::stream::empty()
        })
        .await
    }

    /// Tests RTM_NEWADDR when the ASP is closed with an unexpected terminal
    /// event.
    #[test_case(
        test_addr_subnet_v4(),
        AddressRemovalReason::DadFailed; "v4_dad_failed")]
    #[test_case(
        test_addr_subnet_v6(),
        AddressRemovalReason::DadFailed; "v6_dad_failed")]
    #[test_case(
        test_addr_subnet_v4(),
        AddressRemovalReason::InterfaceRemoved; "v4_interface_removed")]
    #[test_case(
        test_addr_subnet_v6(),
        AddressRemovalReason::InterfaceRemoved; "v6_interface_removed")]
    #[test_case(
        test_addr_subnet_v4(),
        AddressRemovalReason::UserRemoved; "v4_user_removed")]
    #[test_case(
        test_addr_subnet_v6(),
        AddressRemovalReason::UserRemoved; "v6_user_removed")]
    #[should_panic(expected = "expected netstack to send initial state before removing")]
    #[fuchsia::test]
    async fn test_new_addr_failed_unexpected_reason(
        address: AddrSubnetEither,
        reason: AddressRemovalReason,
    ) {
        let _: TestRequestResult = test_new_addr_failed_helper(address, reason).await;
    }

    /// Tests RTM_NEWADDR when the ASP is gracefully closed with a terminal event.
    #[test_case(
        test_addr_subnet_v4(),
        AddressRemovalReason::Invalid,
        RequestError::InvalidRequest; "v4_invalid")]
    #[test_case(
        test_addr_subnet_v6(),
        AddressRemovalReason::Invalid,
        RequestError::InvalidRequest; "v6_invalid")]
    #[test_case(
        test_addr_subnet_v4(),
        AddressRemovalReason::AlreadyAssigned,
        RequestError::AlreadyExists; "v4_exists")]
    #[test_case(
        test_addr_subnet_v6(),
        AddressRemovalReason::AlreadyAssigned,
        RequestError::AlreadyExists; "v6_exists")]
    #[fuchsia::test]
    async fn test_new_addr_failed(
        address: AddrSubnetEither,
        reason: AddressRemovalReason,
        expected_error: RequestError,
    ) {
        pretty_assertions::assert_eq!(
            test_new_addr_failed_helper(address, reason).await,
            TestRequestResult { messages: Vec::new(), waiter_results: vec![Err(expected_error)] },
        )
    }

    /// An RTM_NEWADDR test helper that calls the callback with a stream of ASP
    /// requests after the Detach request is handled.
    async fn test_new_addr_asp_detach_handled_helper<
        St: Stream<Item = fnet_interfaces::Event>,
        F: Fn(fnet_interfaces_admin::AddressStateProviderRequestStream) -> St,
    >(
        address: AddrSubnetEither,
        add_subnet_route: bool,
        asp_handler: F,
    ) -> TestRequestResult {
        test_new_addr_asp_helper(address, add_subnet_route, |asp_request_stream| {
            asp_request_stream
                .into_future()
                .map(|(asp_request, asp_request_stream)| {
                    let _: fnet_interfaces_admin::AddressStateProviderControlHandle = asp_request
                        .expect("eventloop uses ASP before dropping")
                        .expect("unexpected error while waiting for Detach request")
                        .into_detach()
                        .expect("eventloop makes detach request immediately");

                    asp_handler(asp_request_stream)
                })
                .flatten_stream()
        })
        .await
    }

    /// Test RTM_NEWADDR when the ASP is dropped immediately after handling the
    /// Detach request (no assignment state update or terminal event).
    #[test_case(test_addr_subnet_v4(); "v4")]
    #[test_case(test_addr_subnet_v6(); "v6")]
    #[fuchsia::test]
    async fn test_new_addr_drop_asp_after_detach(address: AddrSubnetEither) {
        pretty_assertions::assert_eq!(
            test_new_addr_asp_detach_handled_helper(address, false, |_asp_stream| {
                futures::stream::empty()
            })
            .await,
            TestRequestResult {
                messages: Vec::new(),
                waiter_results: vec![Err(RequestError::UnrecognizedInterface)],
            },
        )
    }

    /// Test RTM_NEWADDR when the ASP yields an assignment state update.
    #[test_case(add_test_addr_subnet_v4(); "v4")]
    #[test_case(add_test_addr_subnet_v6(); "v6")]
    #[fuchsia::test]
    async fn test_new_addr_with_address_added_event(address: AddrSubnetEither) {
        pretty_assertions::assert_eq!(
            test_new_addr_asp_detach_handled_helper(address, true, |asp_request_stream| {
                asp_request_stream
                    .control_handle()
                    .send_on_address_added()
                    .expect("send address added");

                // Send an update with the added address to complete the
                // request.
                futures::stream::iter([fnet_interfaces::Event::Changed(
                    fnet_interfaces::Properties {
                        id: Some(ETH_INTERFACE_ID.try_into().unwrap()),
                        addresses: Some(vec![test_addr(address.into_ext())]),
                        ..fnet_interfaces::Properties::default()
                    },
                )])
            })
            .await,
            TestRequestResult { messages: Vec::new(), waiter_results: vec![Ok(())] },
        )
    }

    /// Test RTM_DELADDR when the interface is closed with an unexpected reaosn.
    #[test_case(
        test_addr_subnet_v4(),
        InterfaceRemovedReason::DuplicateName; "v4_duplicate_name")]
    #[test_case(
        test_addr_subnet_v6(),
        InterfaceRemovedReason::DuplicateName; "v6_duplicate_name")]
    #[test_case(
        test_addr_subnet_v4(),
        InterfaceRemovedReason::PortAlreadyBound; "v4_port_already_bound")]
    #[test_case(
        test_addr_subnet_v6(),
        InterfaceRemovedReason::PortAlreadyBound; "v6_port_already_bound")]
    #[test_case(
        test_addr_subnet_v4(),
        InterfaceRemovedReason::BadPort; "v4_bad_port")]
    #[test_case(
        test_addr_subnet_v6(),
        InterfaceRemovedReason::BadPort; "v6_bad_port")]
    #[should_panic(expected = "unexpected interface removed reason")]
    #[fuchsia::test]
    async fn test_del_addr_interface_closed_unexpected_reason(
        address: AddrSubnetEither,
        removal_reason: InterfaceRemovedReason,
    ) {
        let _: TestRequestResult = test_interface_request(TestInterfaceRequestCase {
            address,
            kind: AddressRequestKind::Del,
            control_request_handler: |req| match req {
                fnet_interfaces_admin::ControlRequest::RemoveAddress {
                    address: got_address,
                    responder,
                } => {
                    pretty_assertions::assert_eq!(got_address, address.into_ext());
                    let control_handle = responder.control_handle();
                    control_handle.send_on_interface_removed(removal_reason).unwrap();
                    control_handle.shutdown();
                    futures::stream::empty()
                }
                req => panic!("unexpected request {req:?}"),
            },
        })
        .await;
    }

    fn del_addr_test_interface_case(
        address: AddrSubnetEither,
        response: Result<bool, fnet_interfaces_admin::ControlRemoveAddressError>,
        remaining_address: Option<AddrSubnetEither>,
    ) -> TestInterfaceRequestCase<
        impl FnMut(
            fnet_interfaces_admin::ControlRequest,
        ) -> futures::stream::Iter<core::array::IntoIter<fnet_interfaces::Event, 1>>,
    > {
        TestInterfaceRequestCase {
            address,
            kind: AddressRequestKind::Del,
            control_request_handler: move |req| {
                match req {
                    fnet_interfaces_admin::ControlRequest::RemoveAddress {
                        address: got_address,
                        responder,
                    } => {
                        pretty_assertions::assert_eq!(got_address, address.into_ext());
                        responder.send(response).unwrap();

                        // Send an update without the deleted address to complete
                        // the request.
                        futures::stream::iter([fnet_interfaces::Event::Changed(
                            fnet_interfaces::Properties {
                                id: Some(ETH_INTERFACE_ID.try_into().unwrap()),
                                addresses: Some(remaining_address.map_or_else(Vec::new, |addr| {
                                    vec![test_addr(addr.into_ext())]
                                })),
                                ..fnet_interfaces::Properties::default()
                            },
                        )])
                    }
                    req => panic!("unexpected request {req:?}"),
                }
            },
        }
    }

    /// Test RTM_DELADDR with all interesting responses to remove address.
    #[test_case(
        test_addr_subnet_v4(),
        Ok(true),
        Ok(()); "v4_did_remove")]
    #[test_case(
        test_addr_subnet_v6(),
        Ok(true),
        Ok(()); "v6_did_remove")]
    #[test_case(
        test_addr_subnet_v4(),
        Ok(false),
        Err(RequestError::AddressNotFound); "v4_did_not_remove")]
    #[test_case(
        test_addr_subnet_v6(),
        Ok(false),
        Err(RequestError::AddressNotFound); "v6_did_not_remove")]
    #[test_case(
        test_addr_subnet_v4(),
        Err(fnet_interfaces_admin::ControlRemoveAddressError::unknown()),
        Err(RequestError::InvalidRequest); "v4_unrecognized_error")]
    #[test_case(
        test_addr_subnet_v6(),
        Err(fnet_interfaces_admin::ControlRemoveAddressError::unknown()),
        Err(RequestError::InvalidRequest); "v6_unrecognized_error")]
    #[fuchsia::test]
    async fn test_del_addr(
        address: AddrSubnetEither,
        response: Result<bool, fnet_interfaces_admin::ControlRemoveAddressError>,
        waiter_result: Result<(), RequestError>,
    ) {
        pretty_assertions::assert_eq!(
            test_interface_request(del_addr_test_interface_case(address, response, None)).await,
            TestRequestResult { messages: Vec::new(), waiter_results: vec![waiter_result] },
        )
    }

    /// Tests that multiple interface update requests result in only one
    /// admin handle being created for that interface.
    #[fuchsia::test]
    async fn test_single_get_admin_for_multiple_interface_requests() {
        let first_address = test_addr_subnet_v4();
        let second_address = test_addr_subnet_v6();
        pretty_assertions::assert_eq!(
            test_maybe_two_interface_requests_on_single_control(
                del_addr_test_interface_case(first_address, Ok(true), Some(second_address)),
                Some(del_addr_test_interface_case(second_address, Ok(true), None)),
            )
            .await,
            TestRequestResult { messages: Vec::new(), waiter_results: vec![Ok(()), Ok(())] },
        )
    }
}
