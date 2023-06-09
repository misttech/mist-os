// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A module for managing RTM_LINK and RTM_ADDR information by generating
//! RTM_LINK and RTM_ADDR Netlink messages based on events received from
//! Netstack's interface watcher.

use std::{
    collections::{BTreeMap, HashMap},
    hash::Hash,
    num::NonZeroU64,
};

use fidl_fuchsia_hardware_network as fhwnet;
use fidl_fuchsia_net as fnet;
use fidl_fuchsia_net_ext::IntoExt as _;
use fidl_fuchsia_net_interfaces as fnet_interfaces;
use fidl_fuchsia_net_interfaces_admin::{
    self as fnet_interfaces_admin, AddressRemovalReason, InterfaceRemovedReason,
};
use fidl_fuchsia_net_interfaces_ext::{
    self as fnet_interfaces_ext,
    admin::{assignment_state_stream, AddressStateProviderError, TerminalError},
    Update as _,
};
use fidl_fuchsia_net_root as fnet_root;

use futures::{
    channel::{mpsc, oneshot},
    pin_mut, StreamExt as _, TryStreamExt as _,
};
use net_types::ip::{AddrSubnetEither, IpVersion};
use netlink_packet_core::NetlinkMessage;
use netlink_packet_route::{
    AddressHeader, AddressMessage, LinkHeader, LinkMessage, RtnlMessage, AF_INET, AF_INET6,
    AF_UNSPEC, ARPHRD_ETHER, ARPHRD_LOOPBACK, ARPHRD_PPP, ARPHRD_VOID, IFA_F_PERMANENT,
    IFF_LOOPBACK, IFF_RUNNING, IFF_UP, RTNLGRP_IPV4_IFADDR, RTNLGRP_IPV6_IFADDR, RTNLGRP_LINK,
};
use tracing::{debug, error, warn};

use crate::{
    client::{ClientTable, InternalClient},
    errors::EventLoopError,
    messaging::Sender,
    multicast_groups::ModernGroup,
    netlink_packet::UNSPECIFIED_SEQUENCE_NUMBER,
    protocol_family::{route::NetlinkRoute, ProtocolFamily},
    NETLINK_LOG_TAG,
};

/// Arguments for an RTM_GETLINK [`Request`].
#[derive(Copy, Clone, Debug)]
pub(crate) enum GetLinkArgs {
    /// Dump state for all the links.
    Dump,
    // TODO(https://issuetracker.google.com/283134954): Support get requests w/
    // filter.
}

/// [`Request`] arguments associated with links.
#[derive(Copy, Clone, Debug)]
pub(crate) enum LinkRequestArgs {
    /// RTM_GETLINK
    Get(GetLinkArgs),
}

/// Arguments for an RTM_GETADDR [`Request`].
#[derive(Copy, Clone, Debug)]
pub(crate) enum GetAddressArgs {
    /// Dump state for all addresses with the optional IP version filter.
    Dump { ip_version_filter: Option<IpVersion> },
    // TODO(https://issuetracker.google.com/283134032): Support get requests w/
    // filter.
}

/// Arguments for an RTM_NEWADDR [`Request`].
#[derive(Copy, Clone, Debug)]
pub(crate) struct NewAddressArgs {
    /// The address to be added.
    #[allow(unused)]
    pub address: AddrSubnetEither,
    /// The ID of the interface the address should be added to.
    #[allow(unused)]
    pub interface_id: u32,
}

/// Arguments for an RTM_DELADDR [`Request`].
#[derive(Copy, Clone, Debug)]
pub(crate) struct DelAddressArgs {
    /// The address to be removed.
    #[allow(unused)]
    pub address: AddrSubnetEither,
    /// The ID of the interface the address should be removed from.
    #[allow(unused)]
    pub interface_id: u32,
}

/// [`Request`] arguments associated with addresses.
#[derive(Copy, Clone, Debug)]
pub(crate) enum AddressRequestArgs {
    /// RTM_GETADDR
    Get(GetAddressArgs),
    /// RTM_NEWADDR
    #[allow(unused)]
    New(NewAddressArgs),
    /// RTM_DELADDR
    #[allow(unused)]
    Del(DelAddressArgs),
}

/// The argument(s) for a [`Request`].
#[derive(Copy, Clone, Debug)]
pub(crate) enum RequestArgs {
    Link(LinkRequestArgs),
    Address(AddressRequestArgs),
}

/// An error encountered while handling a [`Request`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum RequestError {
    InvalidRequest,
    UnrecognizedInterface,
    AlreadyExists,
    NotFound,
}

fn map_existing_interface_terminal_error(
    e: TerminalError<InterfaceRemovedReason>,
    interface_id: u32,
) -> RequestError {
    match e {
        TerminalError::Fidl(e) => {
            // If the channel was closed, then we likely tried to get a control
            // chandle to an interface that does not exist.
            if !e.is_closed() {
                error!(
                    "unexpected interface terminal error for interface ({}): {:?}",
                    interface_id, e,
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
                    "unexpected interface removed reason {:?} for interface ({})",
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
                error!("unrecognized removal reason {:?} from interface {}", reason, interface_id)
            }
        },
    }

    RequestError::UnrecognizedInterface
}

/// A request associated with links or addresses.
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

/// Contains the asynchronous work related to RTM_LINK and RTM_ADDR messages.
///
/// Connects to the interfaces watcher and can respond to RTM_LINK and RTM_ADDR
/// message requests.
pub(crate) struct EventLoop<S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>> {
    /// An `InterfacesProxy` to get controlling access to interfaces.
    interfaces_proxy: fnet_root::InterfacesProxy,
    /// A `StateProxy` to connect to the interfaces watcher.
    state_proxy: fnet_interfaces::StateProxy,
    /// The current set of clients of NETLINK_ROUTE protocol family.
    route_clients: ClientTable<NetlinkRoute, S>,
    /// A stream of [`Request`]s for the event loop to handle.
    request_stream: mpsc::Receiver<Request<S>>,
}

/// FIDL errors from the interfaces worker.
#[derive(Debug, thiserror::Error)]
pub(crate) enum InterfacesFidlError {
    /// Error in the FIDL event stream.
    #[error("event stream: {0}")]
    EventStream(fidl::Error),
    /// Error connecting to the root interfaces protocol.
    #[error("connecting to `fuchsia.net.root/Interfaces`: {0}")]
    RootInterfaces(anyhow::Error),
    /// Error connecting to state marker.
    #[error("connecting to state marker: {0}")]
    State(anyhow::Error),
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

impl<S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>> EventLoop<S> {
    /// `new` returns a `Result<EventLoop, EventLoopError>` instance.
    /// This is fallible iff it is not possible to obtain the `StateProxy`.
    pub(crate) fn new(
        route_clients: ClientTable<NetlinkRoute, S>,
        request_stream: mpsc::Receiver<Request<S>>,
    ) -> Result<Self, EventLoopError<InterfacesFidlError, InterfacesNetstackError>> {
        use fuchsia_component::client::connect_to_protocol;
        let interfaces_proxy = connect_to_protocol::<fnet_root::InterfacesMarker>()
            .map_err(|e| EventLoopError::Fidl(InterfacesFidlError::RootInterfaces(e)))?;
        let state_proxy = connect_to_protocol::<fnet_interfaces::StateMarker>()
            .map_err(|e| EventLoopError::Fidl(InterfacesFidlError::State(e)))?;

        Ok(EventLoop { interfaces_proxy, state_proxy, route_clients, request_stream })
    }

    /// Run the asynchronous work related to RTM_LINK and RTM_ADDR messages.
    ///
    /// The event loop can track interface properties, and is never
    /// expected to complete.
    /// Returns: `InterfacesEventLoopError` that requires restarting the
    /// event loop task, for example, if the watcher stream ends or if
    /// the FIDL protocol cannot be connected.
    pub(crate) async fn run(self) -> EventLoopError<InterfacesFidlError, InterfacesNetstackError> {
        let EventLoop { interfaces_proxy, state_proxy, route_clients, request_stream } = self;
        let if_event_stream = {
            let stream_res = fnet_interfaces_ext::event_stream_from_state(&state_proxy);

            match stream_res {
                Ok(stream) => stream.fuse(),
                Err(e) => {
                    return EventLoopError::Fidl(InterfacesFidlError::WatcherCreation(e));
                }
            }
        };
        pin_mut!(if_event_stream);

        // Fetch the existing interfaces and addresses to generate the initial
        // state before starting to publish events to members of multicast
        // groups. This is because messages should not be sent for
        // interfaces/addresses that already existed. Sending messages in
        // response to existing (`fnet_interfaces::Event::Existing`) events will
        // violate that expectation.
        let (mut interface_properties, mut addresses) = {
            let interface_properties =
                match fnet_interfaces_ext::existing(if_event_stream.by_ref(), HashMap::new()).await
                {
                    Ok(interfaces) => interfaces,
                    Err(fnet_interfaces_ext::WatcherOperationError::UnexpectedEnd { .. }) => {
                        return EventLoopError::Netstack(InterfacesNetstackError::EventStreamEnded);
                    }
                    Err(fnet_interfaces_ext::WatcherOperationError::EventStream(e)) => {
                        return EventLoopError::Fidl(InterfacesFidlError::EventStream(e));
                    }
                    Err(fnet_interfaces_ext::WatcherOperationError::Update(e)) => {
                        return EventLoopError::Netstack(InterfacesNetstackError::Update(e));
                    }
                    Err(fnet_interfaces_ext::WatcherOperationError::UnexpectedEvent(event)) => {
                        return EventLoopError::Netstack(InterfacesNetstackError::UnexpectedEvent(
                            event,
                        ));
                    }
                };

            // `BTreeMap` so that addresses are iterated in deterministic order
            // (useful for tests).
            let addresses: BTreeMap<_, _> = interface_properties
                .values()
                .map(|properties| {
                    addresses_optionally_from_interface_properties(properties)
                        .map(IntoIterator::into_iter)
                        .into_iter()
                        .flatten()
                })
                .flatten()
                .collect();

            (interface_properties, addresses)
        };

        // Chain a pending so that the stream never ends. This is so that tests
        // can safely rely on just closing the watcher to terminate the event
        // loop. This is okay because we do not expect the request stream to
        // reasonably end and if we did want to support graceful shutdown of the
        // event loop, we can have a dedicated shutdown signal.
        let mut request_stream = request_stream.chain(futures::stream::pending());

        loop {
            futures::select! {
                stream_res = if_event_stream.try_next() => {
                    let event = match stream_res {
                        Ok(Some(event)) => event,
                        Ok(None) => {
                            return EventLoopError::Netstack(InterfacesNetstackError::EventStreamEnded);
                        }
                        Err(e) => {
                            return EventLoopError::Fidl(InterfacesFidlError::EventStream(e));
                        }
                    };

                    match handle_interface_watcher_event(
                        &mut interface_properties,
                        &mut addresses,
                        &route_clients,
                        event,
                    ) {
                        Ok(()) => {}
                        Err(InterfaceEventHandlerError::ExistingEventReceived(properties)) => {
                            // This error indicates there is an inconsistent interface state shared
                            // between Netlink and Netstack.
                            return EventLoopError::Netstack(InterfacesNetstackError::UnexpectedEvent(
                                fnet_interfaces::Event::Existing(properties.into()),
                            ));
                        }
                        Err(InterfaceEventHandlerError::Update(e)) => {
                            // This error is severe enough to indicate a larger problem in Netstack.
                            return EventLoopError::Netstack(InterfacesNetstackError::Update(e));
                        }
                    }
                }
                req = request_stream.next() => {
                    Self::handle_request(
                        &interfaces_proxy,
                        &interface_properties,
                        &addresses,
                        req.expect(
                            "request stream should never end because of chained `pending`",
                        ),
                    ).await
                }
            }
        }
    }

    fn get_interface_control(
        interfaces_proxy: &fnet_root::InterfacesProxy,
        interface_id: u32,
    ) -> fnet_interfaces_ext::admin::Control {
        let (control, server_end) = fnet_interfaces_ext::admin::Control::create_endpoints()
            .expect("create Control endpoints");
        interfaces_proxy
            .get_admin(interface_id.into(), server_end)
            .expect("send get admin request");
        control
    }

    async fn handle_new_address_request(
        interfaces_proxy: &fnet_root::InterfacesProxy,
        NewAddressArgs { address, interface_id }: NewAddressArgs,
    ) -> Result<(), RequestError> {
        let control = Self::get_interface_control(interfaces_proxy, interface_id);

        let (asp, asp_server_end) =
            fidl::endpoints::create_proxy::<fnet_interfaces_admin::AddressStateProviderMarker>()
                .expect("create ASP proxy");
        control
            .add_address(
                &mut address.into_ext(),
                fnet_interfaces_admin::AddressParameters::default(),
                asp_server_end,
            )
            .map_err(|e| {
                warn!("error adding {address} to interface ({interface_id}): {e:?}");
                map_existing_interface_terminal_error(e, interface_id)
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
            warn!("error detaching ASP for {} on interface ({}): {:?}", address, interface_id, e)
        });

        match assignment_state_stream(asp).try_next().await {
            Ok(None) => {
                warn!(
                    "ASP state stream ended before update for {} on interface ({})",
                    address, interface_id,
                );

                // If the ASP stream ended without sending a state update, then
                // assume the server end was closed immediately which occurs
                // when the interface does not exist (either because it was
                // removed of the request interface never existed).
                Err(RequestError::UnrecognizedInterface)
            }
            Ok(Some(state)) => {
                debug!(
                    "{} added on interface ({}) with initial state = {:?}",
                    address, interface_id, state,
                );

                // We got an initial state update indicating that the address
                // was successfully added.
                Ok(())
            }
            Err(e) => {
                warn!(
                    "error waiting for state update for {} on interface ({}): {:?}",
                    address, interface_id, e,
                );

                Err(match e {
                    AddressStateProviderError::AddressRemoved(reason) => match reason {
                        AddressRemovalReason::Invalid => RequestError::InvalidRequest,
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
                        // If the channel is closed, assume a similar situation
                        // as `Ok(None)`.
                        if !e.is_closed() {
                            error!(
                                "unexpected ASP error when adding {} on interface ({}): {:?}",
                                address, interface_id, e,
                            )
                        }

                        RequestError::UnrecognizedInterface
                    }
                    AddressStateProviderError::ChannelClosed => {
                        // If the channel is closed, assume a similar situation
                        // as `Ok(None)`.
                        RequestError::UnrecognizedInterface
                    }
                })
            }
        }
    }

    async fn handle_del_address_request(
        interfaces_proxy: &fnet_root::InterfacesProxy,
        DelAddressArgs { address, interface_id }: DelAddressArgs,
    ) -> Result<(), RequestError> {
        let control = Self::get_interface_control(interfaces_proxy, interface_id);

        match control.remove_address(&mut address.into_ext()).await.map_err(|e| {
            warn!("error removing {address} from interface ({interface_id}): {e:?}");
            map_existing_interface_terminal_error(e, interface_id)
        })? {
            Ok(did_remove) => {
                if did_remove {
                    Ok(())
                } else {
                    Err(RequestError::NotFound)
                }
            }
            Err(e) => {
                // `e` is a flexible FIDL enum so we cannot exhaustively match.
                let e: fnet_interfaces_admin::ControlRemoveAddressError = e;
                match e {
                    fnet_interfaces_admin::ControlRemoveAddressErrorUnknown!() => {
                        error!(
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

    async fn handle_request(
        interfaces_proxy: &fnet_root::InterfacesProxy,
        interface_properties: &HashMap<u64, fnet_interfaces_ext::Properties>,
        all_addresses: &BTreeMap<InterfaceAndAddr, NetlinkAddressMessage>,
        Request { args, sequence_number, mut client, completer }: Request<S>,
    ) {
        debug!("handling request {args:?} from {client}");

        let result = match args {
            RequestArgs::Link(LinkRequestArgs::Get(args)) => match args {
                GetLinkArgs::Dump => {
                    interface_properties
                        .values()
                        .filter_map(NetlinkLinkMessage::optionally_from)
                        .for_each(|message| {
                            client.send(message.into_rtnl_new_link(sequence_number))
                        });
                    Ok(())
                }
            },
            RequestArgs::Address(args) => match args {
                AddressRequestArgs::Get(args) => match args {
                    GetAddressArgs::Dump { ip_version_filter } => {
                        all_addresses
                            .values()
                            .filter(|NetlinkAddressMessage(message)| {
                                ip_version_filter.map_or(true, |ip_version| {
                                    ip_version.eq(&match message.header.family.into() {
                                        AF_INET => IpVersion::V4,
                                        AF_INET6 => IpVersion::V6,
                                        family => unreachable!(
                                            "unexpected address family ({}); addr = {:?}",
                                            family, message,
                                        ),
                                    })
                                })
                            })
                            .for_each(|message| {
                                client.send(message.to_rtnl_new_addr(sequence_number))
                            });
                        Ok(())
                    }
                },
                AddressRequestArgs::New(args) => {
                    Self::handle_new_address_request(interfaces_proxy, args).await
                }
                AddressRequestArgs::Del(args) => {
                    Self::handle_del_address_request(interfaces_proxy, args).await
                }
            },
        };

        debug!("handled request {args:?} from {client} with result = {result:?}");

        match completer.send(result) {
            Ok(()) => (),
            Err(result) => {
                // Not treated as a hard error because the socket may have been
                // closed.
                warn!(
                    "failed to send result ({:?}) to {} after handling request {:?}",
                    result, client, args,
                )
            }
        }
    }
}

/// Errors related to handling interface events.
#[derive(Debug)]
enum InterfaceEventHandlerError {
    /// Interface event handler updated the HashMap with an event, but received an
    /// unexpected response.
    Update(fnet_interfaces_ext::UpdateError),
    /// Interface event handler attempted to process an event for an interface that already existed.
    ExistingEventReceived(fnet_interfaces_ext::Properties),
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct InterfaceAndAddr {
    id: u32,
    addr: fnet::IpAddress,
}

fn update_addresses<S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>>(
    interface_id: &NonZeroU64,
    all_addresses: &mut BTreeMap<InterfaceAndAddr, NetlinkAddressMessage>,
    interface_addresses: BTreeMap<InterfaceAndAddr, NetlinkAddressMessage>,
    route_clients: &ClientTable<NetlinkRoute, S>,
) {
    enum UpdateKind {
        New,
        Del,
    }

    let send_update = |addr: &NetlinkAddressMessage, kind| {
        let NetlinkAddressMessage(inner) = addr;
        let group = match inner.header.family.into() {
            AF_INET => RTNLGRP_IPV4_IFADDR,
            AF_INET6 => RTNLGRP_IPV6_IFADDR,
            family => {
                unreachable!("unrecognized interface address family ({family}); addr = {addr:?}")
            }
        };

        let message = match kind {
            UpdateKind::New => addr.to_rtnl_new_addr(UNSPECIFIED_SEQUENCE_NUMBER),
            UpdateKind::Del => addr.to_rtnl_del_addr(UNSPECIFIED_SEQUENCE_NUMBER),
        };

        route_clients.send_message_to_group(message, ModernGroup(group));
    };

    // Send a message to interested listeners only if the address is newly added
    // or its message has changed.
    for (key, message) in interface_addresses.iter() {
        if all_addresses.get(key) != Some(message) {
            send_update(message, UpdateKind::New)
        }
    }

    all_addresses.retain(|key @ InterfaceAndAddr { id, addr: _ }, message| {
        // If the address is for an interface different from what we are
        // operating on, keep the address.
        if u64::from(*id) != interface_id.get() {
            return true;
        }

        // If the address exists in the latest update, keep it. If it was
        // updated, we will update this map with the updated values below.
        if interface_addresses.contains_key(key) {
            return true;
        }

        // The address is not present in the interfaces latest update so it
        // has been deleted.
        send_update(message, UpdateKind::Del);

        false
    });

    // Update our set of all addresses with the latest set known to be assigned
    // to the interface.
    all_addresses.extend(interface_addresses);
}

/// Handles events observed from the interface watcher by updating interfaces
/// from the underlying interface properties HashMap.
///
/// Returns an `InterfaceEventLoopError` when unexpected events occur, or an
/// `UpdateError` when updates are not consistent with the current state.
fn handle_interface_watcher_event<S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>>(
    interface_properties: &mut HashMap<u64, fnet_interfaces_ext::Properties>,
    all_addresses: &mut BTreeMap<InterfaceAndAddr, NetlinkAddressMessage>,
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
                route_clients.send_message_to_group(
                    message.into_rtnl_new_link(UNSPECIFIED_SEQUENCE_NUMBER),
                    ModernGroup(RTNLGRP_LINK),
                )
            }

            // Send address messages after the link message for newly added links
            // so that netlink clients are aware of the interface before sending
            // address messages for an interface.
            if let Some(interface_addresses) =
                addresses_optionally_from_interface_properties(properties)
            {
                update_addresses(&properties.id, all_addresses, interface_addresses, route_clients);
            }

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
                    id,
                    addresses: _,
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
                        message.into_rtnl_new_link(UNSPECIFIED_SEQUENCE_NUMBER),
                        ModernGroup(RTNLGRP_LINK),
                    )
                }

                debug!(
                    tag = NETLINK_LOG_TAG,
                    "processed interface link change event for id {}", id
                );
            };

            // The `is_some` check is not strictly necessary because
            // `update_addresses` will calculate the delta before sending
            // updates but is useful as an optimization when addresses don't
            // change (avoid allocations and message comparisons that will net
            // no updates).
            if addresses.is_some() {
                if let Some(interface_addresses) =
                    addresses_optionally_from_interface_properties(current)
                {
                    update_addresses(&id, all_addresses, interface_addresses, route_clients);
                }

                debug!(
                    tag = NETLINK_LOG_TAG,
                    "processed interface address change event for id {}", id
                );
            }
        }
        fnet_interfaces_ext::UpdateResult::Removed(properties) => {
            update_addresses(&properties.id, all_addresses, BTreeMap::new(), route_clients);

            // Send link messages after the address message for removed links
            // so that netlink clients are aware of the interface throughout the
            // address messages.
            if let Some(message) = NetlinkLinkMessage::optionally_from(&properties) {
                route_clients.send_message_to_group(
                    message.into_rtnl_del_link(UNSPECIFIED_SEQUENCE_NUMBER),
                    ModernGroup(RTNLGRP_LINK),
                )
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
pub(crate) struct NetlinkLinkMessage(LinkMessage);

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

    pub(crate) fn into_rtnl_new_link(self, sequence_number: u32) -> NetlinkMessage<RtnlMessage> {
        let Self(message) = self;
        let mut msg: NetlinkMessage<RtnlMessage> = RtnlMessage::NewLink(message).into();
        msg.header.sequence_number = sequence_number;
        msg.finalize();
        msg
    }

    fn into_rtnl_del_link(self, sequence_number: u32) -> NetlinkMessage<RtnlMessage> {
        let Self(message) = self;
        let mut msg: NetlinkMessage<RtnlMessage> = RtnlMessage::DelLink(message).into();
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
pub(crate) struct NetlinkAddressMessage(AddressMessage);

impl NetlinkAddressMessage {
    pub(crate) fn to_rtnl_new_addr(&self, sequence_number: u32) -> NetlinkMessage<RtnlMessage> {
        let Self(message) = self;
        let mut message: NetlinkMessage<RtnlMessage> =
            RtnlMessage::NewAddress(message.clone()).into();
        message.header.sequence_number = sequence_number;
        message.finalize();
        message
    }

    pub(crate) fn to_rtnl_del_addr(&self, sequence_number: u32) -> NetlinkMessage<RtnlMessage> {
        let Self(message) = self;
        let mut message: NetlinkMessage<RtnlMessage> =
            RtnlMessage::DelAddress(message.clone()).into();
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
    properties: &fnet_interfaces_ext::Properties,
) -> Option<BTreeMap<InterfaceAndAddr, NetlinkAddressMessage>> {
    match interface_properties_to_address_messages(properties) {
        Ok(o) => Some(o),
        Err(NetlinkAddressMessageConversionError::InvalidInterfaceId(id)) => {
            warn!(tag = NETLINK_LOG_TAG, "Invalid interface id: {:?}", id);
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
        device_class: _,
        online: _,
        has_default_ipv4_route: _,
        has_default_ipv6_route: _,
    }: &fnet_interfaces_ext::Properties,
) -> Result<BTreeMap<InterfaceAndAddr, NetlinkAddressMessage>, NetlinkAddressMessageConversionError>
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
             }| {
                let mut addr_header = AddressHeader::default();

                let (family, addr_bytes) = match addr {
                    fnet::IpAddress::Ipv4(ip_addr) => (AF_INET, ip_addr.addr.to_vec()),
                    fnet::IpAddress::Ipv6(ip_addr) => (AF_INET6, ip_addr.addr.to_vec()),
                };

                // The possible constants below are in the range of u8-accepted values, so they can
                // be safely casted to a u8.
                addr_header.family = family.try_into().expect("should fit into u8");
                addr_header.prefix_len = *prefix_len;
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
                    netlink_packet_route::address::nlas::Nla::Address(addr_bytes),
                    netlink_packet_route::address::nlas::Nla::Label(name.clone()),
                    netlink_packet_route::address::nlas::Nla::Flags(flags.into()),
                ];

                let mut addr_message = AddressMessage::default();
                addr_message.header = addr_header;
                addr_message.nlas = nlas;
                (InterfaceAndAddr { id, addr: addr.clone() }, NetlinkAddressMessage(addr_message))
            },
        )
        .collect();

    Ok(address_messages)
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;

    use assert_matches::assert_matches;
    use fuchsia_zircon as zx;
    use futures::stream::Stream;
    use net_declare::fidl_subnet;
    use netlink_packet_core::NetlinkPayload;

    use crate::messaging::testutil::FakeSender;

    pub(crate) const LO_INTERFACE_ID: u64 = 1;
    pub(crate) const LO_NAME: &str = "lo";
    pub(crate) const ETH_INTERFACE_ID: u64 = 2;
    pub(crate) const ETH_NAME: &str = "eth";
    pub(crate) const WLAN_INTERFACE_ID: u64 = 3;
    pub(crate) const WLAN_NAME: &str = "wlan";
    pub(crate) const PPP_INTERFACE_ID: u64 = 4;
    pub(crate) const PPP_NAME: &str = "ppp";

    pub(crate) const ETHERNET: fnet_interfaces::DeviceClass =
        fnet_interfaces::DeviceClass::Device(fhwnet::DeviceClass::Ethernet);
    pub(crate) const WLAN: fnet_interfaces::DeviceClass =
        fnet_interfaces::DeviceClass::Device(fhwnet::DeviceClass::Wlan);
    pub(crate) const WLAN_AP: fnet_interfaces::DeviceClass =
        fnet_interfaces::DeviceClass::Device(fhwnet::DeviceClass::WlanAp);
    pub(crate) const PPP: fnet_interfaces::DeviceClass =
        fnet_interfaces::DeviceClass::Device(fhwnet::DeviceClass::Ppp);
    pub(crate) const LOOPBACK: fnet_interfaces::DeviceClass =
        fnet_interfaces::DeviceClass::Loopback(fnet_interfaces::Empty {});
    pub(crate) const TEST_V4_ADDR: fnet::Subnet = fidl_subnet!("192.0.2.1/24");
    pub(crate) const TEST_V6_ADDR: fnet::Subnet = fidl_subnet!("2001:db8::1/32");

    pub(crate) struct Setup<W> {
        pub event_loop: EventLoop<FakeSender<RtnlMessage>>,
        pub watcher_stream: W,
        pub request_sink: mpsc::Sender<Request<FakeSender<RtnlMessage>>>,
        pub interfaces_request_stream: fnet_root::InterfacesRequestStream,
    }

    pub(crate) fn setup_with_route_clients(
        route_clients: ClientTable<NetlinkRoute, FakeSender<RtnlMessage>>,
    ) -> Setup<impl Stream<Item = fnet_interfaces::WatcherRequest>> {
        let (interfaces_proxy, interfaces_request_stream) =
            fidl::endpoints::create_proxy_and_stream::<fnet_root::InterfacesMarker>().unwrap();
        let (state_proxy, if_stream) =
            fidl::endpoints::create_proxy_and_stream::<fnet_interfaces::StateMarker>().unwrap();
        let (request_sink, request_stream) = mpsc::channel(1);
        let event_loop = EventLoop::<FakeSender<_>> {
            interfaces_proxy,
            state_proxy,
            route_clients,
            request_stream,
        };

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

        Setup { event_loop, watcher_stream, request_sink, interfaces_request_stream }
    }

    pub(crate) fn setup() -> Setup<impl Stream<Item = fnet_interfaces::WatcherRequest>> {
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

    pub(crate) fn sort_link_messages(messages: &mut [NetlinkMessage<RtnlMessage>]) {
        messages.sort_by_key(|message| {
            assert_matches!(
                &message.payload,
                NetlinkPayload::InnerMessage(RtnlMessage::NewLink(m)) => {
                    m.header.index
                }
            )
        })
    }

    pub(crate) fn create_netlink_link_message(
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

    pub(crate) fn create_nlas(
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

    pub(crate) fn create_address_message(
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

    pub(crate) fn test_addr(addr: fnet::Subnet) -> fnet_interfaces::Address {
        fnet_interfaces::Address {
            addr: Some(addr),
            valid_until: Some(zx::sys::ZX_TIME_INFINITE),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{testutil::*, *};

    use fidl::endpoints::{ControlHandle as _, RequestStream as _, Responder as _};
    use fidl_fuchsia_net as fnet;
    use fnet_interfaces::AddressAssignmentState;
    use fuchsia_async::{self as fasync};

    use assert_matches::assert_matches;
    use futures::{
        future::{Future, FutureExt as _},
        sink::SinkExt as _,
        stream::Stream,
    };
    use net_declare::net_addr_subnet;
    use netlink_packet_route::RTNLGRP_IPV4_ROUTE;
    use pretty_assertions::assert_eq;
    use test_case::test_case;

    use crate::messaging::testutil::FakeSender;

    const TEST_SEQUENCE_NUMBER: u32 = 1234;

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

    fn create_default_address_messages(
        interface_id: u64,
        interface_name: String,
        flags: u32,
    ) -> BTreeMap<InterfaceAndAddr, NetlinkAddressMessage> {
        let interface_id = interface_id.try_into().expect("should fit into u32");
        BTreeMap::from_iter([
            (
                InterfaceAndAddr { id: interface_id, addr: TEST_V4_ADDR.addr },
                create_address_message(interface_id, TEST_V4_ADDR, interface_name.clone(), flags),
            ),
            (
                InterfaceAndAddr { id: interface_id, addr: TEST_V6_ADDR.addr },
                create_address_message(interface_id, TEST_V6_ADDR, interface_name, flags),
            ),
        ])
    }

    #[fuchsia::test]
    fn test_handle_interface_watcher_event() {
        let mut interface_properties: HashMap<u64, fnet_interfaces_ext::Properties> =
            HashMap::new();
        let mut addresses = BTreeMap::new();
        let route_clients = ClientTable::<NetlinkRoute, FakeSender<_>>::default();

        let mut interface1 = create_interface(1, "test".into(), ETHERNET, true, vec![]);
        let interface2 = create_interface(2, "lo".into(), LOOPBACK, true, vec![]);

        let interface1_add_event = fnet_interfaces::Event::Added(interface1.clone().into());
        assert_matches!(
            handle_interface_watcher_event(
                &mut interface_properties,
                &mut addresses,
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
                &mut addresses,
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
                &mut addresses,
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
                &mut addresses,
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
        let Setup { event_loop, watcher_stream, request_sink: _, interfaces_request_stream: _ } =
            setup();
        let event_loop_fut = event_loop.run();
        let interfaces = vec![get_fake_interface(1, "lo", LOOPBACK)];
        let watcher_fut =
            respond_to_watcher_with_interfaces(watcher_stream, interfaces, None::<(Option<_>, _)>);

        let (err, ()) = futures::join!(event_loop_fut, watcher_fut);
        assert_matches!(
            err,
            EventLoopError::Fidl(InterfacesFidlError::EventStream(
                fidl::Error::ClientChannelClosed { .. }
            ))
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_event_loop_errors_duplicate_events() {
        let Setup { event_loop, watcher_stream, request_sink: _, interfaces_request_stream: _ } =
            setup();
        let event_loop_fut = event_loop.run();
        let interfaces =
            vec![get_fake_interface(1, "lo", LOOPBACK), get_fake_interface(1, "lo", LOOPBACK)];
        let watcher_fut =
            respond_to_watcher_with_interfaces(watcher_stream, interfaces, None::<(Option<_>, _)>);

        let (err, ()) = futures::join!(event_loop_fut, watcher_fut);
        // The event being sent again as existing has interface id 1.
        assert_matches!(err,
            EventLoopError::Netstack(
                InterfacesNetstackError::Update(
                    fnet_interfaces_ext::UpdateError::DuplicateExisting(properties)
                )
            )
            if properties.id.unwrap() == 1);
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_event_loop_errors_duplicate_adds() {
        let Setup { event_loop, watcher_stream, request_sink: _, interfaces_request_stream: _ } =
            setup();
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
            EventLoopError::Netstack(
                InterfacesNetstackError::Update(
                    fnet_interfaces_ext::UpdateError::DuplicateAdded(properties)
                )
            )
            if properties.id.unwrap() == 1);
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_event_loop_errors_existing_after_add() {
        let Setup { event_loop, watcher_stream, request_sink: _, interfaces_request_stream: _ } =
            setup();
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
            EventLoopError::Netstack(
                InterfacesNetstackError::UnexpectedEvent(
                    fnet_interfaces::Event::Existing(properties)
                )
            )
            if properties.id.unwrap() == 3);
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
        pretty_assertions::assert_eq!(actual, expected);
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
        let new_link_message = link.into_rtnl_new_link(UNSPECIFIED_SEQUENCE_NUMBER);
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
            &[ModernGroup(RTNLGRP_LINK)],
        );
        let (mut addr4_sink, addr4_client) = crate::client::testutil::new_fake_client::<NetlinkRoute>(
            crate::client::testutil::CLIENT_ID_2,
            &[ModernGroup(RTNLGRP_IPV4_IFADDR)],
        );
        let (mut addr6_sink, addr6_client) = crate::client::testutil::new_fake_client::<NetlinkRoute>(
            crate::client::testutil::CLIENT_ID_3,
            &[ModernGroup(RTNLGRP_IPV6_IFADDR)],
        );
        let (mut other_sink, other_client) = crate::client::testutil::new_fake_client::<NetlinkRoute>(
            crate::client::testutil::CLIENT_ID_4,
            &[ModernGroup(RTNLGRP_IPV4_ROUTE)],
        );
        let (mut all_sink, all_client) = crate::client::testutil::new_fake_client::<NetlinkRoute>(
            crate::client::testutil::CLIENT_ID_5,
            &[
                ModernGroup(RTNLGRP_LINK),
                ModernGroup(RTNLGRP_IPV6_IFADDR),
                ModernGroup(RTNLGRP_IPV4_IFADDR),
            ],
        );
        let Setup { event_loop, mut watcher_stream, request_sink: _, interfaces_request_stream: _ } =
            setup_with_route_clients({
                let route_clients = ClientTable::default();
                route_clients.add_client(link_client);
                route_clients.add_client(addr4_client);
                route_clients.add_client(addr6_client);
                route_clients.add_client(all_client);
                route_clients.add_client(other_client);
                route_clients
            });
        let event_loop_fut = event_loop.run().fuse();
        futures::pin_mut!(event_loop_fut);

        // Existing events should never trigger messages to be sent.
        let watcher_stream_fut = respond_to_watcher(
            watcher_stream.by_ref(),
            [
                fnet_interfaces::Event::Existing(fnet_interfaces::Properties {
                    id: Some(LO_INTERFACE_ID),
                    name: Some(LO_NAME.to_string()),
                    device_class: Some(LOOPBACK),
                    online: Some(false),
                    addresses: Some(vec![test_addr(TEST_V4_ADDR)]),
                    has_default_ipv4_route: Some(false),
                    has_default_ipv6_route: Some(false),
                    ..Default::default()
                }),
                fnet_interfaces::Event::Existing(fnet_interfaces::Properties {
                    id: Some(ETH_INTERFACE_ID),
                    name: Some(ETH_NAME.to_string()),
                    device_class: Some(ETHERNET),
                    online: Some(false),
                    addresses: Some(vec![test_addr(TEST_V6_ADDR), test_addr(TEST_V4_ADDR)]),
                    has_default_ipv4_route: Some(false),
                    has_default_ipv6_route: Some(false),
                    ..Default::default()
                }),
                fnet_interfaces::Event::Existing(fnet_interfaces::Properties {
                    id: Some(PPP_INTERFACE_ID),
                    name: Some(PPP_NAME.to_string()),
                    device_class: Some(PPP),
                    online: Some(false),
                    addresses: Some(vec![test_addr(TEST_V4_ADDR), test_addr(TEST_V6_ADDR)]),
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
                    device_class: Some(WLAN),
                    online: Some(false),
                    addresses: Some(vec![test_addr(TEST_V4_ADDR), test_addr(TEST_V6_ADDR)]),
                    has_default_ipv4_route: Some(false),
                    has_default_ipv6_route: Some(false),
                    ..Default::default()
                }),
                fnet_interfaces::Event::Changed(fnet_interfaces::Properties {
                    id: Some(LO_INTERFACE_ID),
                    online: Some(true),
                    addresses: Some(vec![test_addr(TEST_V4_ADDR), test_addr(TEST_V6_ADDR)]),
                    ..Default::default()
                }),
                fnet_interfaces::Event::Removed(ETH_INTERFACE_ID),
                fnet_interfaces::Event::Changed(fnet_interfaces::Properties {
                    id: Some(PPP_INTERFACE_ID),
                    addresses: Some(Vec::new()),
                    ..Default::default()
                }),
            ],
        );
        let ((), err) = futures::future::join(watcher_stream_fut, event_loop_fut).await;
        assert_matches!(
            err,
            EventLoopError::Fidl(InterfacesFidlError::EventStream(
                fidl::Error::ClientChannelClosed { .. },
            ))
        );
        let wlan_link = create_netlink_link_message(
            WLAN_INTERFACE_ID,
            ARPHRD_ETHER,
            0,
            create_nlas(WLAN_NAME.to_string(), ARPHRD_ETHER, false),
        )
        .into_rtnl_new_link(UNSPECIFIED_SEQUENCE_NUMBER);
        let lo_link = create_netlink_link_message(
            LO_INTERFACE_ID,
            ARPHRD_LOOPBACK,
            IFF_UP | IFF_RUNNING | IFF_LOOPBACK,
            create_nlas(LO_NAME.to_string(), ARPHRD_LOOPBACK, true),
        )
        .into_rtnl_new_link(UNSPECIFIED_SEQUENCE_NUMBER);
        let eth_link = create_netlink_link_message(
            ETH_INTERFACE_ID,
            ARPHRD_ETHER,
            0,
            create_nlas(ETH_NAME.to_string(), ARPHRD_ETHER, false),
        )
        .into_rtnl_del_link(UNSPECIFIED_SEQUENCE_NUMBER);
        assert_eq!(
            &link_sink.take_messages()[..],
            &[wlan_link.clone(), lo_link.clone(), eth_link.clone(),],
        );

        let wlan_v4_addr = create_address_message(
            WLAN_INTERFACE_ID.try_into().unwrap(),
            TEST_V4_ADDR,
            WLAN_NAME.to_string(),
            IFA_F_PERMANENT,
        )
        .to_rtnl_new_addr(UNSPECIFIED_SEQUENCE_NUMBER);
        let eth_v4_addr = create_address_message(
            ETH_INTERFACE_ID.try_into().unwrap(),
            TEST_V4_ADDR,
            ETH_NAME.to_string(),
            IFA_F_PERMANENT,
        )
        .to_rtnl_del_addr(UNSPECIFIED_SEQUENCE_NUMBER);
        let ppp_v4_addr = create_address_message(
            PPP_INTERFACE_ID.try_into().unwrap(),
            TEST_V4_ADDR,
            PPP_NAME.to_string(),
            IFA_F_PERMANENT,
        )
        .to_rtnl_del_addr(UNSPECIFIED_SEQUENCE_NUMBER);
        assert_eq!(
            &addr4_sink.take_messages()[..],
            &[wlan_v4_addr.clone(), eth_v4_addr.clone(), ppp_v4_addr.clone(),],
        );

        let wlan_v6_addr = create_address_message(
            WLAN_INTERFACE_ID.try_into().unwrap(),
            TEST_V6_ADDR,
            WLAN_NAME.to_string(),
            IFA_F_PERMANENT,
        )
        .to_rtnl_new_addr(UNSPECIFIED_SEQUENCE_NUMBER);
        let lo_v6_addr = create_address_message(
            LO_INTERFACE_ID.try_into().unwrap(),
            TEST_V6_ADDR,
            LO_NAME.to_string(),
            IFA_F_PERMANENT,
        )
        .to_rtnl_new_addr(UNSPECIFIED_SEQUENCE_NUMBER);
        let eth_v6_addr = create_address_message(
            ETH_INTERFACE_ID.try_into().unwrap(),
            TEST_V6_ADDR,
            ETH_NAME.to_string(),
            IFA_F_PERMANENT,
        )
        .to_rtnl_del_addr(UNSPECIFIED_SEQUENCE_NUMBER);
        let ppp_v6_addr = create_address_message(
            PPP_INTERFACE_ID.try_into().unwrap(),
            TEST_V6_ADDR,
            PPP_NAME.to_string(),
            IFA_F_PERMANENT,
        )
        .to_rtnl_del_addr(UNSPECIFIED_SEQUENCE_NUMBER);
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
            ],
        );
        assert_eq!(&other_sink.take_messages()[..], &[]);
    }

    async fn expect_no_root_requests(
        interfaces_request_stream: fnet_root::InterfacesRequestStream,
    ) {
        interfaces_request_stream
            .for_each(|req| async move { panic!("unexpected request {req:?}") })
            .await
    }

    #[derive(Debug, PartialEq)]
    struct TestRequestResult {
        messages: Vec<NetlinkMessage<RtnlMessage>>,
        waiter_result: Result<(), RequestError>,
    }

    async fn test_request<
        Fut: Future<Output = ()>,
        F: FnOnce(fnet_root::InterfacesRequestStream) -> Fut,
    >(
        args: RequestArgs,
        root_handler: F,
    ) -> TestRequestResult {
        let (mut expected_sink, expected_client) = crate::client::testutil::new_fake_client::<
            NetlinkRoute,
        >(
            crate::client::testutil::CLIENT_ID_1, &[]
        );
        let (mut other_sink, other_client) = crate::client::testutil::new_fake_client::<NetlinkRoute>(
            crate::client::testutil::CLIENT_ID_2,
            &[],
        );
        let Setup { event_loop, mut watcher_stream, mut request_sink, interfaces_request_stream } =
            setup_with_route_clients({
                let route_clients = ClientTable::default();
                route_clients.add_client(expected_client.clone());
                route_clients.add_client(other_client);
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
                    online: Some(true),
                    addresses: Some(vec![test_addr(TEST_V6_ADDR), test_addr(TEST_V4_ADDR)]),
                    has_default_ipv4_route: Some(false),
                    has_default_ipv6_route: Some(false),
                    ..Default::default()
                }),
                fnet_interfaces::Event::Existing(fnet_interfaces::Properties {
                    id: Some(ETH_INTERFACE_ID),
                    name: Some(ETH_NAME.to_string()),
                    device_class: Some(ETHERNET),
                    online: Some(false),
                    addresses: Some(vec![test_addr(TEST_V4_ADDR), test_addr(TEST_V6_ADDR)]),
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
        assert_eq!(&expected_sink.take_messages()[..], &[]);
        assert_eq!(&other_sink.take_messages()[..], &[]);

        let (completer, waiter) = oneshot::channel();
        let fut = request_sink
            .send(Request {
                args,
                sequence_number: TEST_SEQUENCE_NUMBER,
                client: expected_client.clone(),
                completer,
            })
            .then(|res| {
                res.expect("send request");
                waiter
            });
        let waiter_result = futures::select! {
            () = root_handler(interfaces_request_stream).fuse() => {
                unreachable!("root handler should not return")
            },
            res = fut.fuse() => res.unwrap(),
            err = event_loop_fut => unreachable!("eventloop should not return: {err:?}"),
        };
        assert_eq!(&other_sink.take_messages()[..], &[]);
        TestRequestResult { messages: expected_sink.take_messages(), waiter_result }
    }

    #[fuchsia::test]
    async fn test_get_link() {
        let TestRequestResult { mut messages, waiter_result } = test_request(
            RequestArgs::Link(LinkRequestArgs::Get(GetLinkArgs::Dump)),
            expect_no_root_requests,
        )
        .await;
        sort_link_messages(&mut messages);
        assert_eq!(waiter_result, Ok(()));
        assert_eq!(
            messages,
            [
                create_netlink_link_message(
                    LO_INTERFACE_ID,
                    ARPHRD_LOOPBACK,
                    IFF_UP | IFF_RUNNING | IFF_LOOPBACK,
                    create_nlas(LO_NAME.to_string(), ARPHRD_LOOPBACK, true),
                )
                .into_rtnl_new_link(TEST_SEQUENCE_NUMBER),
                create_netlink_link_message(
                    ETH_INTERFACE_ID,
                    ARPHRD_ETHER,
                    0,
                    create_nlas(ETH_NAME.to_string(), ARPHRD_ETHER, false),
                )
                .into_rtnl_new_link(TEST_SEQUENCE_NUMBER),
            ],
        );
    }

    #[test_case(Some(IpVersion::V4); "v4")]
    #[test_case(Some(IpVersion::V6); "v6")]
    #[test_case(None; "all")]
    #[fuchsia::test]
    async fn test_get_addr(ip_version_filter: Option<IpVersion>) {
        pretty_assertions::assert_eq!(
            test_request(
                RequestArgs::Address(AddressRequestArgs::Get(GetAddressArgs::Dump {
                    ip_version_filter
                })),
                expect_no_root_requests,
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
                                create_address_message(
                                    id.try_into().unwrap(),
                                    addr,
                                    name.to_string(),
                                    IFA_F_PERMANENT,
                                )
                                .to_rtnl_new_addr(TEST_SEQUENCE_NUMBER)
                            })
                    })
                    .flatten()
                    .collect(),
                waiter_result: Ok(()),
            },
        );
    }

    // AddrSubnetEither does not have any const methods so we need a method.
    fn addr_subnet_v4() -> AddrSubnetEither {
        net_addr_subnet!("192.0.2.1/24")
    }

    // AddrSubnetEither does not have any const methods so we need a method.
    fn addr_subnet_v6() -> AddrSubnetEither {
        net_addr_subnet!("2001:db8::1/32")
    }

    /// Tests RTM_NEWADDR and RTM_DEL_ADDR when the interface is removed,
    /// indicated by the closure of the admin Control's server-end.
    #[test_case(
        addr_subnet_v4(),
        None,
        true; "v4_no_terminal_new")]
    #[test_case(
        addr_subnet_v6(),
        None,
        true; "v6_no_terminal_new")]
    #[test_case(
        addr_subnet_v4(),
        Some(InterfaceRemovedReason::PortClosed),
        true; "v4_port_closed_terminal_new")]
    #[test_case(
        addr_subnet_v6(),
        Some(InterfaceRemovedReason::PortClosed),
        true; "v6_port_closed_terminal_new")]
    #[test_case(
        addr_subnet_v4(),
        Some(InterfaceRemovedReason::User),
        true; "v4_user_terminal_new")]
    #[test_case(
        addr_subnet_v6(),
        Some(InterfaceRemovedReason::User),
        true; "v6_user_terminal_new")]
    #[test_case(
        addr_subnet_v4(),
        None,
        false; "v4_no_terminal_del")]
    #[test_case(
        addr_subnet_v6(),
        None,
        false; "v6_no_terminal_del")]
    #[test_case(
        addr_subnet_v4(),
        Some(InterfaceRemovedReason::PortClosed),
        false; "v4_port_closed_terminal_del")]
    #[test_case(
        addr_subnet_v6(),
        Some(InterfaceRemovedReason::PortClosed),
        false; "v6_port_closed_terminal_del")]
    #[test_case(
        addr_subnet_v4(),
        Some(InterfaceRemovedReason::User),
        false; "v4_user_terminal_del")]
    #[test_case(
        addr_subnet_v6(),
        Some(InterfaceRemovedReason::User),
        false; "v6_user_terminal_del")]
    #[fuchsia::test]
    async fn test_new_del_addr_interface_removed(
        address: AddrSubnetEither,
        removal_reason: Option<InterfaceRemovedReason>,
        is_new: bool,
    ) {
        let interface_id = LO_INTERFACE_ID.try_into().unwrap();
        pretty_assertions::assert_eq!(
            test_request(
                if is_new {
                    RequestArgs::Address(AddressRequestArgs::New(NewAddressArgs {
                        address,
                        interface_id,
                    }))
                } else {
                    RequestArgs::Address(AddressRequestArgs::Del(DelAddressArgs {
                        address,
                        interface_id,
                    }))
                },
                |interfaces_request_stream| async {
                    interfaces_request_stream
                        .for_each(|req| {
                            futures::future::ready(match req.unwrap() {
                                fnet_root::InterfacesRequest::GetAdmin {
                                    id,
                                    control,
                                    control_handle: _,
                                } => {
                                    pretty_assertions::assert_eq!(id, LO_INTERFACE_ID);
                                    let control = control.into_stream().unwrap();
                                    let control = control.control_handle();
                                    if let Some(reason) = removal_reason {
                                        control.send_on_interface_removed(reason).unwrap()
                                    }
                                    control.shutdown();
                                }
                            })
                        })
                        .await
                },
            )
            .await,
            TestRequestResult {
                messages: Vec::new(),
                waiter_result: Err(RequestError::UnrecognizedInterface),
            },
        )
    }

    /// A test helper that calls the callback with a
    /// [`fnet_interfaces_admin::ControlRequest`] as they arrive.
    async fn test_interface_request<
        Fut: Future<Output = ()>,
        F: FnMut(fnet_interfaces_admin::ControlRequest) -> Fut,
    >(
        address: AddrSubnetEither,
        is_new: bool,
        mut control_request_handler: F,
    ) -> TestRequestResult {
        let interface_id = ETH_INTERFACE_ID.try_into().unwrap();
        test_request(
            if is_new {
                RequestArgs::Address(AddressRequestArgs::New(NewAddressArgs {
                    address,
                    interface_id,
                }))
            } else {
                RequestArgs::Address(AddressRequestArgs::Del(DelAddressArgs {
                    address,
                    interface_id,
                }))
            },
            |interfaces_request_stream| async {
                interfaces_request_stream
                    .then(|req| {
                        futures::future::ready(match req.unwrap() {
                            fnet_root::InterfacesRequest::GetAdmin {
                                id,
                                control,
                                control_handle: _,
                            } => {
                                pretty_assertions::assert_eq!(id, ETH_INTERFACE_ID);
                                control.into_stream().unwrap()
                            }
                        })
                    })
                    .flatten()
                    .for_each(|req| control_request_handler(req.unwrap()))
                    .await
            },
        )
        .await
    }

    /// An RTM_NEWADDR test helper that calls the callback with a stream of ASP
    /// requests.
    async fn test_new_addr_asp_helper<
        Fut: Future<Output = ()>,
        F: Fn(fnet_interfaces_admin::AddressStateProviderRequestStream) -> Fut,
    >(
        address: AddrSubnetEither,
        asp_handler: F,
    ) -> TestRequestResult {
        test_interface_request(address, true, |req| async {
            match req {
                fnet_interfaces_admin::ControlRequest::AddAddress {
                    address: got_address,
                    parameters,
                    address_state_provider,
                    control_handle: _,
                } => {
                    pretty_assertions::assert_eq!(got_address, address.into_ext());
                    pretty_assertions::assert_eq!(
                        parameters,
                        fnet_interfaces_admin::AddressParameters::default()
                    );
                    asp_handler(address_state_provider.into_stream().unwrap()).await
                }
                req => panic!("unexpected request {req:?}"),
            }
        })
        .await
    }

    /// Tests RTM_NEWADDR when the ASP is dropped immediately (doesn't handle
    /// any request).
    #[test_case(addr_subnet_v4(); "v4")]
    #[test_case(addr_subnet_v6(); "v6")]
    #[fuchsia::test]
    async fn test_new_addr_drop_asp_immediately(address: AddrSubnetEither) {
        pretty_assertions::assert_eq!(
            test_new_addr_asp_helper(address, |_asp_request_stream| async {}).await,
            TestRequestResult {
                messages: Vec::new(),
                waiter_result: Err(RequestError::UnrecognizedInterface),
            },
        )
    }

    /// RTM_NEWADDR test helper that exercises the ASP being closed with a
    /// terminal event.
    async fn test_new_addr_failed_helper(
        address: AddrSubnetEither,
        reason: AddressRemovalReason,
    ) -> TestRequestResult {
        test_new_addr_asp_helper(address, |asp_request_stream| async move {
            asp_request_stream.control_handle().send_on_address_removed(reason).unwrap()
        })
        .await
    }

    /// Tests RTM_NEWADDR when the ASP is closed with an unexpected terminal
    /// event.
    #[test_case(
        addr_subnet_v4(),
        AddressRemovalReason::DadFailed; "v4_dad_failed")]
    #[test_case(
        addr_subnet_v6(),
        AddressRemovalReason::DadFailed; "v6_dad_failed")]
    #[test_case(
        addr_subnet_v4(),
        AddressRemovalReason::InterfaceRemoved; "v4_interface_removed")]
    #[test_case(
        addr_subnet_v6(),
        AddressRemovalReason::InterfaceRemoved; "v6_interface_removed")]
    #[test_case(
        addr_subnet_v4(),
        AddressRemovalReason::UserRemoved; "v4_user_removed")]
    #[test_case(
        addr_subnet_v6(),
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
        addr_subnet_v4(),
        AddressRemovalReason::Invalid,
        RequestError::InvalidRequest; "v4_invalid")]
    #[test_case(
        addr_subnet_v6(),
        AddressRemovalReason::Invalid,
        RequestError::InvalidRequest; "v6_invalid")]
    #[test_case(
        addr_subnet_v4(),
        AddressRemovalReason::AlreadyAssigned,
        RequestError::AlreadyExists; "v4_exists")]
    #[test_case(
        addr_subnet_v6(),
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
            TestRequestResult { messages: Vec::new(), waiter_result: Err(expected_error) },
        )
    }

    /// An RTM_NEWADDR test helper that calls the callback with a stream of ASP
    /// requests after the Detach request is handled.
    async fn test_new_addr_asp_detach_handled_helper<
        Fut: Future<Output = ()>,
        F: Fn(fnet_interfaces_admin::AddressStateProviderRequestStream) -> Fut,
    >(
        address: AddrSubnetEither,
        asp_handler: F,
    ) -> TestRequestResult {
        test_new_addr_asp_helper(address, |mut asp_request_stream| async {
            let _: fnet_interfaces_admin::AddressStateProviderControlHandle = asp_request_stream
                .next()
                .await
                .expect("eventloop uses ASP before dropping")
                .expect("unexpected error while waiting for Detach request")
                .into_detach()
                .expect("eventloop makes detach request immediately");
            asp_handler(asp_request_stream).await
        })
        .await
    }

    /// Test RTM_NEWADDR when the ASP is dropped immediately after handling the
    /// Detach request (no assignment state update or terminal event).
    #[test_case(addr_subnet_v4(); "v4")]
    #[test_case(addr_subnet_v6(); "v6")]
    #[fuchsia::test]
    async fn test_new_addr_drop_asp_after_detach(address: AddrSubnetEither) {
        pretty_assertions::assert_eq!(
            test_new_addr_asp_detach_handled_helper(address, |_asp_stream| async {}).await,
            TestRequestResult {
                messages: Vec::new(),
                waiter_result: Err(RequestError::UnrecognizedInterface),
            },
        )
    }

    /// Test RTM_NEWADDR when the ASP yields an assignment state update.
    #[test_case(addr_subnet_v4(); "v4")]
    #[test_case(addr_subnet_v6(); "v6")]
    #[fuchsia::test]
    async fn test_new_addr_with_state_update(address: AddrSubnetEither) {
        pretty_assertions::assert_eq!(
            test_new_addr_asp_detach_handled_helper(address, |mut asp_request_stream| async move {
                let responder = asp_request_stream
                    .next()
                    .await
                    .expect("eventloop watches for address assignment state before dropping ASP")
                    .expect("unexpected error while waiting for event from event loop")
                    .into_watch_address_assignment_state()
                    .expect("eventloop only makes WatchAddressAssignmentState request");
                responder.send(AddressAssignmentState::Assigned).unwrap();
            })
            .await,
            TestRequestResult { messages: Vec::new(), waiter_result: Ok(()) },
        )
    }

    /// Test RTM_DELADDR when the interface is closed with an unexpected reaosn.
    #[test_case(
        addr_subnet_v4(),
        InterfaceRemovedReason::DuplicateName; "v4_duplicate_name")]
    #[test_case(
        addr_subnet_v6(),
        InterfaceRemovedReason::DuplicateName; "v6_duplicate_name")]
    #[test_case(
        addr_subnet_v4(),
        InterfaceRemovedReason::PortAlreadyBound; "v4_port_already_bound")]
    #[test_case(
        addr_subnet_v6(),
        InterfaceRemovedReason::PortAlreadyBound; "v6_port_already_bound")]
    #[test_case(
        addr_subnet_v4(),
        InterfaceRemovedReason::BadPort; "v4_bad_port")]
    #[test_case(
        addr_subnet_v6(),
        InterfaceRemovedReason::BadPort; "v6_bad_port")]
    #[should_panic(expected = "unexpected interface removed reason")]
    #[fuchsia::test]
    async fn test_del_addr_interface_closed_unexpected_reason(
        address: AddrSubnetEither,
        removal_reason: InterfaceRemovedReason,
    ) {
        let _: TestRequestResult = test_interface_request(address, false, |req| async {
            match req {
                fnet_interfaces_admin::ControlRequest::RemoveAddress {
                    address: got_address,
                    responder,
                } => {
                    pretty_assertions::assert_eq!(got_address, address.into_ext());
                    let control_handle = responder.control_handle();
                    control_handle.send_on_interface_removed(removal_reason).unwrap();
                    control_handle.shutdown();
                }
                req => panic!("unexpected request {req:?}"),
            }
        })
        .await;
    }

    /// Test RTM_DELADDR with all interesting responses to remove address.
    #[test_case(
        addr_subnet_v4(),
        Ok(true),
        Ok(()); "v4_did_remove")]
    #[test_case(
        addr_subnet_v6(),
        Ok(true),
        Ok(()); "v6_did_remove")]
    #[test_case(
        addr_subnet_v4(),
        Ok(false),
        Err(RequestError::NotFound); "v4_did_not_remove")]
    #[test_case(
        addr_subnet_v6(),
        Ok(false),
        Err(RequestError::NotFound); "v6_did_not_remove")]
    #[test_case(
        addr_subnet_v4(),
        Err(fnet_interfaces_admin::ControlRemoveAddressError::unknown()),
        Err(RequestError::InvalidRequest); "v4_unrecognized_error")]
    #[test_case(
        addr_subnet_v6(),
        Err(fnet_interfaces_admin::ControlRemoveAddressError::unknown()),
        Err(RequestError::InvalidRequest); "v6_unrecognized_error")]
    #[fuchsia::test]
    async fn test_del_addr(
        address: AddrSubnetEither,
        response: Result<bool, fnet_interfaces_admin::ControlRemoveAddressError>,
        waiter_result: Result<(), RequestError>,
    ) {
        pretty_assertions::assert_eq!(
            test_interface_request(address, false, |req| async {
                match req {
                    fnet_interfaces_admin::ControlRequest::RemoveAddress {
                        address: got_address,
                        responder,
                    } => {
                        pretty_assertions::assert_eq!(got_address, address.into_ext());
                        responder.send(response).unwrap();
                    }
                    req => panic!("unexpected request {req:?}"),
                }
            },)
            .await,
            TestRequestResult { messages: Vec::new(), waiter_result },
        )
    }
}
