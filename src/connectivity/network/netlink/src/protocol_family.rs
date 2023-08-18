// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A module for managing protocol-specific aspects of Netlink.

use netlink_packet_core::{NetlinkMessage, NetlinkPayload, NetlinkSerializable};

use std::fmt::Debug;

// TODO(https://github.com/rust-lang/rust/issues/91611): Replace this with
// #![feature(async_fn_in_trait)] once it supports `Send` bounds. See
// https://blog.rust-lang.org/inside-rust/2023/05/03/stabilizing-async-fn-in-trait.html.
use async_trait::async_trait;

use crate::{
    client::{ExternalClient, InternalClient},
    logging::{log_debug, log_warn},
    messaging::Sender,
    multicast_groups::{
        InvalidLegacyGroupsError, InvalidModernGroupError, LegacyGroups, ModernGroup,
        MulticastCapableNetlinkFamily,
    },
};

/// A type representing a Netlink Protocol Family.
pub(crate) trait ProtocolFamily:
    MulticastCapableNetlinkFamily + Send + Sized + 'static
{
    /// The message type associated with the protocol family.
    type InnerMessage: Clone + Debug + NetlinkSerializable + Send + 'static;
    /// The implementation for handling requests from this protocol family.
    type RequestHandler<S: Sender<Self::InnerMessage>>: NetlinkFamilyRequestHandler<Self, S>;

    const NAME: &'static str;
}

#[async_trait]
/// A request handler implementation for a particular Netlink protocol family.
pub(crate) trait NetlinkFamilyRequestHandler<F: ProtocolFamily, S: Sender<F::InnerMessage>>:
    Clone + Send + 'static
{
    /// Handles the given request and generates the associated response(s).
    async fn handle_request(
        &mut self,
        req: NetlinkMessage<F::InnerMessage>,
        client: &mut InternalClient<F, S>,
    );
}

pub mod route {
    //! This module implements the Route Netlink Protocol Family.

    use super::*;

    use std::{
        fmt::Display,
        num::{NonZeroU32, NonZeroU64},
    };

    use fidl_fuchsia_net_routes_ext as fnet_routes_ext;

    use either::Either;
    use futures::{
        channel::{mpsc, oneshot},
        sink::SinkExt as _,
    };
    use net_types::{
        ip::{
            AddrSubnetEither, AddrSubnetError, Ip, IpAddr, IpInvariant, IpVersion, Ipv4, Ipv6,
            Subnet,
        },
        SpecifiedAddr, SpecifiedAddress,
    };

    use crate::{
        interfaces,
        netlink_packet::{self, errno::Errno, ip_addr_from_bytes},
        routes,
        rules::{RuleRequest, RuleRequestArgs, RuleRequestHandler, RuleTable},
    };

    use netlink_packet_core::{NetlinkHeader, NLM_F_ACK, NLM_F_DUMP, NLM_F_REPLACE};
    use netlink_packet_route::{
        rtnl::{
            address::{nlas::Nla as AddressNla, AddressMessage},
            constants::{
                AF_INET, AF_INET6, AF_UNSPEC, IFA_F_NOPREFIXROUTE, RTNLGRP_DCB,
                RTNLGRP_DECNET_IFADDR, RTNLGRP_DECNET_ROUTE, RTNLGRP_DECNET_RULE,
                RTNLGRP_IPV4_IFADDR, RTNLGRP_IPV4_MROUTE, RTNLGRP_IPV4_MROUTE_R,
                RTNLGRP_IPV4_NETCONF, RTNLGRP_IPV4_ROUTE, RTNLGRP_IPV4_RULE, RTNLGRP_IPV6_IFADDR,
                RTNLGRP_IPV6_IFINFO, RTNLGRP_IPV6_MROUTE, RTNLGRP_IPV6_MROUTE_R,
                RTNLGRP_IPV6_NETCONF, RTNLGRP_IPV6_PREFIX, RTNLGRP_IPV6_ROUTE, RTNLGRP_IPV6_RULE,
                RTNLGRP_LINK, RTNLGRP_MDB, RTNLGRP_MPLS_NETCONF, RTNLGRP_MPLS_ROUTE,
                RTNLGRP_ND_USEROPT, RTNLGRP_NEIGH, RTNLGRP_NONE, RTNLGRP_NOP2, RTNLGRP_NOP4,
                RTNLGRP_NOTIFY, RTNLGRP_NSID, RTNLGRP_PHONET_IFADDR, RTNLGRP_PHONET_ROUTE,
                RTNLGRP_TC, RTN_UNICAST, RT_TABLE_COMPAT, RT_TABLE_MAIN,
            },
            route::{nlas::Nla as RouteNla, RouteMessage},
            LinkMessage, RtnlMessage,
        },
        IFF_UP,
    };

    /// An implementation of the Netlink Route protocol family.
    pub(crate) enum NetlinkRoute {}

    impl MulticastCapableNetlinkFamily for NetlinkRoute {
        fn is_valid_group(ModernGroup(group): &ModernGroup) -> bool {
            match *group {
                RTNLGRP_DCB
                | RTNLGRP_DECNET_IFADDR
                | RTNLGRP_DECNET_ROUTE
                | RTNLGRP_DECNET_RULE
                | RTNLGRP_IPV4_IFADDR
                | RTNLGRP_IPV4_MROUTE
                | RTNLGRP_IPV4_MROUTE_R
                | RTNLGRP_IPV4_NETCONF
                | RTNLGRP_IPV4_ROUTE
                | RTNLGRP_IPV4_RULE
                | RTNLGRP_IPV6_IFADDR
                | RTNLGRP_IPV6_IFINFO
                | RTNLGRP_IPV6_MROUTE
                | RTNLGRP_IPV6_MROUTE_R
                | RTNLGRP_IPV6_NETCONF
                | RTNLGRP_IPV6_PREFIX
                | RTNLGRP_IPV6_ROUTE
                | RTNLGRP_IPV6_RULE
                | RTNLGRP_LINK
                | RTNLGRP_MDB
                | RTNLGRP_MPLS_NETCONF
                | RTNLGRP_MPLS_ROUTE
                | RTNLGRP_ND_USEROPT
                | RTNLGRP_NEIGH
                | RTNLGRP_NONE
                | RTNLGRP_NOP2
                | RTNLGRP_NOP4
                | RTNLGRP_NOTIFY
                | RTNLGRP_NSID
                | RTNLGRP_PHONET_IFADDR
                | RTNLGRP_PHONET_ROUTE
                | RTNLGRP_TC => true,
                _ => false,
            }
        }
    }

    impl ProtocolFamily for NetlinkRoute {
        type InnerMessage = RtnlMessage;
        type RequestHandler<S: Sender<Self::InnerMessage>> =
            NetlinkRouteRequestHandler<S, RuleTable>;

        const NAME: &'static str = "NETLINK_ROUTE";
    }

    #[derive(Clone)]
    pub(crate) struct NetlinkRouteRequestHandler<
        S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>,
        R: RuleRequestHandler<S>,
    > {
        pub(crate) interfaces_request_sink: mpsc::Sender<interfaces::Request<S>>,
        pub(crate) v4_routes_request_sink: mpsc::Sender<routes::Request<S, Ipv4>>,
        pub(crate) v6_routes_request_sink: mpsc::Sender<routes::Request<S, Ipv6>>,
        pub(crate) rules_request_handler: R,
    }

    struct ExtractedAddressRequest {
        address_and_interface_id: interfaces::AddressAndInterfaceArgs,
        addr_flags: u32,
    }

    fn extract_if_id_and_addr_from_addr_message(
        message: &AddressMessage,
        client: &impl Display,
        req: &RtnlMessage,
        // `true` for new address requests; `false` for delete address requests.
        is_new: bool,
    ) -> Result<Option<ExtractedAddressRequest>, Errno> {
        let kind = if is_new { "new" } else { "del" };

        let interface_id = match NonZeroU32::new(message.header.index) {
            Some(interface_id) => interface_id,
            None => {
                log_debug!(
                    "unspecified interface ID in address {} request from {}: {:?}",
                    kind,
                    client,
                    req,
                );
                return Err(Errno::EINVAL);
            }
        };

        let mut address_bytes = None;
        let mut local_bytes = None;
        let mut addr_flags = None;
        message.nlas.iter().for_each(|nla| match nla {
            AddressNla::Address(bytes) => address_bytes = Some(bytes),
            AddressNla::Local(bytes) => local_bytes = Some(bytes),
            AddressNla::Flags(flags) => addr_flags = Some(*flags),
            nla => {
                log_warn!(
                    "unexpected Address NLA in {} request from {}: {:?}; req = {:?}",
                    kind,
                    client,
                    nla,
                    req,
                );
            }
        });

        // Linux supports the notion of a "peer" address which is used for
        // pointtopoint interfaces. Fuchsia does not support this so we do not
        // allow different-valued `IFA_LOCAL` and `IFA_ADDRESS` values.
        //
        // Per https://www.man7.org/linux/man-pages/man8/ip-address.8.html,
        //
        //   ip address add - add new protocol address.
        //       dev IFNAME
        //            the name of the device to add the address to.
        //
        //       local ADDRESS (default)
        //            the address of the interface. The format of the address
        //            depends on the protocol. It is a dotted quad for IP and a
        //            sequence of hexadecimal halfwords separated by colons for
        //            IPv6. The ADDRESS may be followed by a slash and a decimal
        //            number which encodes the network prefix length.
        //
        //       peer ADDRESS
        //            the address of the remote endpoint for pointopoint
        //            interfaces. Again, the ADDRESS may be followed by a slash
        //            and a decimal number, encoding the network prefix length.
        //            If a peer address is specified, the local address cannot
        //            have a prefix length. The network prefix is associated
        //            with the peer rather than with the local address.
        //
        //   ...
        //
        //   ip address delete - delete protocol address
        //       Arguments: coincide with the arguments of ip addr add. The
        //       device name is a required argument. The rest are optional. If
        //       no arguments are given, the first address is deleted.
        //
        // Note that when only one of `IFA_LOCAL` or `IFA_ADDRESS` is included
        // in a message, it is treated as the "local" address on the interface
        // to be added/removed. When both are included, `IFA_LOCAL` is treated
        // as the "local" address and `IFA_ADDRESS` is treated as the "peer".
        // TODO(https://fxbug.dev/129502): Support peer addresses.
        let address_bytes = match (local_bytes, address_bytes) {
            (Some(local), Some(address)) => {
                if local == address {
                    address
                } else {
                    log_debug!(
                    "got different `IFA_ADDRESS` and `IFA_LOCAL` values for {} address request from {}: {:?}",
                    kind, client, req,
                );
                    return Err(Errno::ENOTSUP);
                }
            }
            (Some(bytes), None) | (None, Some(bytes)) => bytes,
            (None, None) => {
                log_debug!(
                    "missing `IFA_ADDRESS` and `IFA_LOCAL` in address {} request from {}: {:?}",
                    kind,
                    client,
                    req,
                );
                return Err(Errno::EINVAL);
            }
        };

        let addr = match message.header.family.into() {
            AF_INET => {
                let addr = ip_addr_from_bytes::<Ipv4>(address_bytes)?;
                if !addr.is_specified() {
                    // Linux treats adding the unspecified IPv4 address as a
                    // no-op.
                    return Ok(None);
                }
                IpAddr::V4(addr)
            }
            AF_INET6 => {
                let addr = ip_addr_from_bytes::<Ipv6>(address_bytes)?;
                if !addr.is_specified() {
                    // Linux returns this error when adding the unspecified IPv6
                    // address.
                    return Err(Errno::EADDRNOTAVAIL);
                }
                IpAddr::V6(addr)
            }
            family => {
                log_debug!(
                    "invalid address family ({}) in new address \
                    request from {}: {:?}",
                    family,
                    client,
                    req
                );
                return Err(Errno::EINVAL);
            }
        };

        let address = match AddrSubnetEither::new(addr, message.header.prefix_len) {
            Ok(address) => address,
            Err(
                AddrSubnetError::PrefixTooLong
                | AddrSubnetError::NotUnicastInSubnet
                | AddrSubnetError::InvalidWitness,
            ) => {
                log_debug!(
                    "invalid address in address {} request from {}: {:?}",
                    kind,
                    client,
                    req
                );
                return Err(Errno::EINVAL);
            }
        };

        Ok(Some(ExtractedAddressRequest {
            address_and_interface_id: interfaces::AddressAndInterfaceArgs { address, interface_id },
            addr_flags: addr_flags.unwrap_or_else(|| message.header.flags.into()),
        }))
    }

    /// Constructs the appropriate [`GetLinkArgs`] for this GetLink request.
    fn to_get_link_args(
        link_msg: LinkMessage,
        is_dump: bool,
    ) -> Result<interfaces::GetLinkArgs, Errno> {
        // NB: In the case where the request is "malformed" and specifies
        // multiple fields, Linux prefers the dump flag over the link index, and
        // prefers the link index over the link_name.
        if is_dump {
            return Ok(interfaces::GetLinkArgs::Dump);
        }
        if let Ok(link_id) = <u32 as TryInto<NonZeroU32>>::try_into(link_msg.header.index) {
            return Ok(interfaces::GetLinkArgs::Get(interfaces::LinkSpecifier::Index(link_id)));
        }
        if let Some(name) = link_msg.nlas.into_iter().find_map(|nla| match nla {
            netlink_packet_route::rtnl::link::nlas::Nla::IfName(name) => Some(name),
            nla => {
                log_debug!("ignoring unexpected NLA in GetLink request: {:?}", nla);
                None
            }
        }) {
            return Ok(interfaces::GetLinkArgs::Get(interfaces::LinkSpecifier::Name(name)));
        }
        return Err(Errno::EINVAL);
    }

    /// Constructs the appropriate [`SetLinkArgs`] for this SetLink request.
    fn to_set_link_args(link_msg: LinkMessage) -> Result<interfaces::SetLinkArgs, Errno> {
        let link_id = NonZeroU32::new(link_msg.header.index);
        let link_name = link_msg.nlas.into_iter().find_map(|nla| match nla {
            netlink_packet_route::rtnl::link::nlas::Nla::IfName(name) => Some(name),
            nla => {
                log_debug!("ignoring unexpected NLA in SetLink request: {:?}", nla);
                None
            }
        });
        let link = match (link_id, link_name) {
            (Some(id), None) => interfaces::LinkSpecifier::Index(id),
            (None, Some(name)) => interfaces::LinkSpecifier::Name(name),
            (None, None) => return Err(Errno::EINVAL),
            // NB: If both the index and name are specified, Linux returns EBUSY
            // rather than EINVAL. Do the same here for conformance.
            (Some(_id), Some(_name)) => return Err(Errno::EBUSY),
        };
        // `change_mask` specifies which flags should be updated, while `flags`
        // specifies whether the value should be set/unset.
        let enable: Option<bool> = ((link_msg.header.change_mask & IFF_UP) == IFF_UP)
            .then_some((link_msg.header.flags & IFF_UP) != 0);

        let unsupported_changes = link_msg.header.change_mask & !IFF_UP;
        if unsupported_changes != 0 {
            log_warn!(
                "ignoring unsupported changes in SetLink request: {:#X}",
                unsupported_changes
            );
        }

        Ok(interfaces::SetLinkArgs { link, enable })
    }

    #[derive(Debug)]
    enum ExtractedRouteRequest<I: Ip> {
        // A gateway or direct route.
        Unicast(ExtractedUnicastRouteRequest<I>),
    }
    /// Route request information from a route of type `RTN_UNICAST`.
    /// Fields other than subnet marked as optional to allow for translation
    /// to [`UnicastRouteArgs`] and [`UnicastDelRouteArgs`].
    #[derive(Debug)]
    struct ExtractedUnicastRouteRequest<I: Ip> {
        subnet: Subnet<I::Addr>,
        outbound_interface: Option<NonZeroU64>,
        next_hop: Option<SpecifiedAddr<I::Addr>>,
        priority: Option<NonZeroU32>,
        table: NonZeroU32,
    }

    // Extracts unicast route information from a request.
    // Returns an error if the request is malformed. This error
    // should be returned to the client.
    fn extract_data_from_unicast_route_message<I: Ip>(
        message: &RouteMessage,
        client: &impl Display,
        req: &RtnlMessage,
        kind: &str,
    ) -> Result<ExtractedRouteRequest<I>, Errno> {
        let destination_prefix_len = message.header.destination_prefix_length;
        let mut table: u32 = message.header.table.into();

        let mut destination_bytes = None;
        let mut outbound_interface = None;
        let mut next_hop_bytes = None;
        let mut priority = None;
        message.nlas.iter().for_each(|nla| match nla {
            RouteNla::Destination(bytes) => destination_bytes = Some(bytes),
            RouteNla::Oif(id) => outbound_interface = Some(id),
            RouteNla::Gateway(bytes) => next_hop_bytes = Some(bytes),
            RouteNla::Priority(num) => priority = Some(*num),
            RouteNla::Table(num) => {
                // When the table is set to `RT_TABLE_COMPAT`, the table id is greater than
                // `u8::MAX` and cannot be represented in the header u8. The actual table
                // number is stored in the `RTA_TABLE` NLA.
                // We expect to see the NLA if the value in the header is `RT_TABLE_COMPAT`,
                // although we will use it if it is present, regardless of if `RT_TABLE_COMPAT`
                // is specified.
                if message.header.table != RT_TABLE_COMPAT {
                    log_debug!(
                        "`RTA_TABLE` is expected only when table in header is `RT_TABLE_COMPAT`, \
                        but it was {}. Using provided value of {} in the NLA",
                        message.header.table,
                        table
                    );
                }
                table = *num
            }
            nla => {
                log_warn!(
                    "ignoring unexpected Route NLA in {} request from {}: {:?}; req = {:?}",
                    kind,
                    client,
                    nla,
                    req,
                );
            }
        });

        let outbound_interface =
            outbound_interface.map(|id| NonZeroU64::new((*id).into())).flatten();
        let priority = priority.map(NonZeroU32::new).flatten();
        let table =
            NonZeroU32::new(table).unwrap_or(NonZeroU32::new(RT_TABLE_MAIN.into()).unwrap());

        let destination_addr = match destination_bytes {
            Some(bytes) => ip_addr_from_bytes::<I>(bytes)?,
            None => {
                // Use the unspecified address if there wasn't a destination NLA present
                // and the prefix len is 0.
                if destination_prefix_len != 0 {
                    log_warn!(
                        "rejecting route {} request with prefix length {} and missing `RTA_DST` \
                    from {}: {:?}",
                        kind,
                        destination_prefix_len,
                        client,
                        req
                    );
                    return Err(Errno::EINVAL);
                }
                I::UNSPECIFIED_ADDRESS
            }
        };

        let next_hop = match next_hop_bytes {
            Some(bytes) => {
                // Linux ignores the provided nexthop if it is the default route. To conform
                // to Linux expectations, `SpecifiedAddr::new()` becomes `None` when the addr
                // is unspecified.
                ip_addr_from_bytes::<I>(bytes).map(|addr| SpecifiedAddr::new(addr))?
            }
            None => None,
        };

        let extracted_route_request = match Subnet::new(destination_addr, destination_prefix_len) {
            Ok(subnet) => ExtractedRouteRequest::Unicast(ExtractedUnicastRouteRequest {
                subnet,
                outbound_interface,
                next_hop,
                priority,
                table,
            }),
            Err(e) => {
                log_warn!(
                    "{:?} subnet ({}) in route {} request from {}: {:?}",
                    e,
                    destination_addr,
                    kind,
                    client,
                    req,
                );
                return Err(Errno::EINVAL);
            }
        };

        Ok(extracted_route_request)
    }

    // Translates `RouteMessage` to `RouteRequestArgs::New`.
    //
    // `RouteRequestArgs::New` requires all fields except for next_hop, so optional
    // fields from `ExtractedRouteRequest` are converted to the defaults expected
    // by Netstack.
    fn to_new_route_args<I: Ip>(
        message: &RouteMessage,
        client: &impl Display,
        req: &RtnlMessage,
    ) -> Result<routes::RouteRequestArgs<I>, Errno> {
        let extracted_request =
            extract_data_from_unicast_route_message::<I>(message, client, req, "new")?;

        let ExtractedRouteRequest::Unicast(ExtractedUnicastRouteRequest {
            subnet,
            outbound_interface,
            next_hop,
            priority,
            table,
        }) = extracted_request;

        // If the metric NLA is not specified, there are default values that
        // should be supplied to match Linux expectations.
        //
        // Per https://www.man7.org/linux/man-pages/man8/route.8.html,
        //
        //   ip route - show / manipulate the IP routing table.
        //
        //       metric M
        //            set the metric field in the routing table (used by routing
        //            daemons) to M. If this option is not specified the metric
        //            for inet6 (IPv6) address family defaults to '1', for inet
        //            (IPv4) it defaults to '0'. You should always specify an
        //            explicit metric value to not rely on those defaults - they
        //            also differ from iproute2.
        let priority: u32 = match priority {
            Some(priority) => priority.into(),
            None => {
                let IpInvariant(priority) = I::map_ip((), |()| IpInvariant(0), |()| IpInvariant(1));
                priority
            }
        };

        let outbound_interface = outbound_interface.map(NonZeroU64::get).ok_or_else(|| {
            // TODO(https://issues.fuchsia.dev/292103361): Resolve destination
            // IP to find interface index if it is not provided explicitly.
            log_warn!(
                "unsupported request: missing `RTA_OIF` in new route request from {}: {:?}",
                client,
                req
            );
            Errno::ENOTSUP
        })?;

        Ok(routes::RouteRequestArgs::New(routes::NewRouteArgs::Unicast(
            routes::UnicastNewRouteArgs {
                subnet,
                target: fnet_routes_ext::RouteTarget { outbound_interface, next_hop },
                priority,
                table: table.into(),
            },
        )))
    }

    // Translates `RouteMessage` to `RouteRequestArgs::Del`.
    fn to_del_route_args<I: Ip>(
        message: &RouteMessage,
        client: &impl Display,
        req: &RtnlMessage,
    ) -> Result<routes::RouteRequestArgs<I>, Errno> {
        let extracted_request =
            extract_data_from_unicast_route_message::<I>(message, client, req, "del")?;

        let ExtractedRouteRequest::Unicast(ExtractedUnicastRouteRequest {
            subnet,
            outbound_interface,
            next_hop,
            priority,
            table,
        }) = extracted_request;

        Ok(routes::RouteRequestArgs::Del(routes::DelRouteArgs::Unicast(
            routes::UnicastDelRouteArgs { subnet, outbound_interface, next_hop, priority, table },
        )))
    }

    #[async_trait]
    impl<S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>, R: RuleRequestHandler<S>>
        NetlinkFamilyRequestHandler<NetlinkRoute, S> for NetlinkRouteRequestHandler<S, R>
    {
        async fn handle_request(
            &mut self,
            req: NetlinkMessage<RtnlMessage>,
            client: &mut InternalClient<NetlinkRoute, S>,
        ) {
            let Self {
                interfaces_request_sink,
                v4_routes_request_sink,
                v6_routes_request_sink,
                rules_request_handler,
            } = self;

            let (req_header, payload) = req.into_parts();
            let req = match payload {
                NetlinkPayload::InnerMessage(p) => p,
                p => {
                    log_warn!(
                        "Ignoring request from client {} with unexpected payload: {:?}",
                        client,
                        p
                    );
                    return;
                }
            };

            let is_dump = req_header.flags & NLM_F_DUMP == NLM_F_DUMP;
            let is_replace = req_header.flags & NLM_F_REPLACE == NLM_F_REPLACE;
            let expects_ack = req_header.flags & NLM_F_ACK == NLM_F_ACK;

            use RtnlMessage::*;
            match req {
                GetLink(link_msg) => {
                    let (completer, waiter) = oneshot::channel();
                    let args = match to_get_link_args(link_msg, is_dump) {
                        Ok(args) => args,
                        Err(e) => {
                            log_debug!("received invalid `GetLink` request from {}", client);
                            client.send_unicast(netlink_packet::new_error(Err(e), req_header));
                            return;
                        }
                    };
                    interfaces_request_sink.send(
                        interfaces::Request{
                        args: interfaces::RequestArgs::Link(interfaces::LinkRequestArgs::Get(args)),
                        sequence_number: req_header.sequence_number,
                        client: client.clone(),
                        completer,
                    }).await.expect("interface event loop should never terminate");
                    match waiter
                        .await
                        .expect("interfaces event loop should have handled the request") {
                            Ok(()) => if is_dump {
                                client.send_unicast(netlink_packet::new_done(req_header))
                            } else if expects_ack {
                                client.send_unicast(netlink_packet::new_error(Ok(()), req_header))
                            }
                            Err(e) => client.send_unicast(
                                netlink_packet::new_error(Err(e.into_errno()), req_header)),
                        }
                }
                SetLink(link_msg) => {
                    let args = match to_set_link_args(link_msg) {
                        Ok(args) => args,
                        Err(e) => {
                            log_debug!("received invalid `SetLink` request from {}", client);
                            client.send_unicast(netlink_packet::new_error(Err(e), req_header));
                            return;
                        }
                    };
                    let (completer, waiter) = oneshot::channel();
                    interfaces_request_sink.send(
                        interfaces::Request{
                        args: interfaces::RequestArgs::Link(interfaces::LinkRequestArgs::Set(args)),
                        sequence_number: req_header.sequence_number,
                        client: client.clone(),
                        completer,
                    }).await.expect("interface event loop should never terminate");
                    match waiter
                        .await
                        .expect("interfaces event loop should have handled the request") {
                            Ok(()) => if expects_ack {
                                client.send_unicast(netlink_packet::new_error(Ok(()), req_header))
                            }
                            Err(e) => client.send_unicast(
                                netlink_packet::new_error(Err(e.into_errno()), req_header)),
                        }
                }
                GetAddress(ref message) if is_dump => {
                    let ip_version_filter = match message.header.family.into() {
                        AF_UNSPEC => None,
                        AF_INET => Some(IpVersion::V4),
                        AF_INET6 => Some(IpVersion::V6),
                        family => {
                            log_debug!(
                                "invalid address family ({}) in address dump request from {}: {:?}",
                                family, client, req,
                            );
                            client.send_unicast(
                                netlink_packet::new_error(Err(Errno::EINVAL), req_header));
                            return;
                        }
                    };

                    let (completer, waiter) = oneshot::channel();
                    interfaces_request_sink.send(interfaces::Request {
                        args: interfaces::RequestArgs::Address(
                            interfaces::AddressRequestArgs::Get(
                                interfaces::GetAddressArgs::Dump {
                                    ip_version_filter,
                                },
                            ),
                        ),
                        sequence_number: req_header.sequence_number,
                        client: client.clone(),
                        completer,
                    }).await.expect("interface event loop should never terminate");
                    waiter
                        .await
                        .expect("interfaces event loop should have handled the request")
                        .expect("addr dump requests are infallible");
                    client.send_unicast(netlink_packet::new_done(req_header))
                }
                NewAddress(ref message) => {
                    let extracted_request = match extract_if_id_and_addr_from_addr_message(
                        message,
                        client,
                        &req,
                        true,
                    ) {
                        Ok(o) => o,
                        Err(e) => {
                            return client.send_unicast(netlink_packet::new_error(Err(e), req_header));
                        }
                    };
                    let result = if let Some(ExtractedAddressRequest {
                        address_and_interface_id,
                        addr_flags,
                    }) = extracted_request {
                        let (completer, waiter) = oneshot::channel();
                        let add_subnet_route = addr_flags & IFA_F_NOPREFIXROUTE != IFA_F_NOPREFIXROUTE;
                        interfaces_request_sink.send(interfaces::Request {
                            args: interfaces::RequestArgs::Address(
                                interfaces::AddressRequestArgs::New(
                                    interfaces::NewAddressArgs {
                                        address_and_interface_id,
                                        add_subnet_route,
                                    },
                                ),
                            ),
                            sequence_number: req_header.sequence_number,
                            client: client.clone(),
                            completer,
                        }).await.expect("interface event loop should never terminate");
                        waiter
                            .await
                            .expect("interfaces event loop should have handled the request")
                    } else {
                        Ok(())
                    };

                    match result {
                        Ok(()) => if expects_ack {
                            client.send_unicast(netlink_packet::new_error(Ok(()), req_header))
                        },
                        Err(e) => client.send_unicast(
                            netlink_packet::new_error(Err(e.into_errno()), req_header)),
                    }
                }
                DelAddress(ref message) => {
                    let extracted_request = match extract_if_id_and_addr_from_addr_message(
                        message,
                        client,
                        &req,
                        false,
                    ) {
                        Ok(o) => o,
                        Err(e) => {
                            return client.send_unicast(netlink_packet::new_error(Err(e), req_header));
                        }
                    };

                    let result = if let Some(ExtractedAddressRequest {
                        address_and_interface_id,
                        addr_flags: _,
                    }) = extracted_request {
                        let (completer, waiter) = oneshot::channel();
                        interfaces_request_sink.send(interfaces::Request {
                            args: interfaces::RequestArgs::Address(
                                interfaces::AddressRequestArgs::Del(
                                    interfaces::DelAddressArgs {
                                        address_and_interface_id,
                                    },
                                ),
                            ),
                            sequence_number: req_header.sequence_number,
                            client: client.clone(),
                            completer,
                        }).await.expect("interface event loop should never terminate");
                        waiter
                            .await
                            .expect("interfaces event loop should have handled the request")
                    } else {
                        Ok(())
                    };
                    match result {
                        Ok(()) => if expects_ack {
                            client.send_unicast(netlink_packet::new_error(Ok(()), req_header))
                        },
                        Err(e) => client.send_unicast(
                            netlink_packet::new_error(Err(e.into_errno()), req_header))
                    }
                }
                GetRoute(ref message) if is_dump => {
                    match message.header.address_family.into() {
                        AF_UNSPEC => {
                            // V4 routes are requested prior to V6 routes to conform
                            // with `ip list` output.
                            process_routes_worker_request::<_, Ipv4>(
                                v4_routes_request_sink,
                                client,
                                req_header,
                                routes::RouteRequestArgs::Get(
                                    routes::GetRouteArgs::Dump,
                                ),
                            ).await
                            .expect("route dump requests are infallible");
                        process_routes_worker_request::<_, Ipv6>(
                                v6_routes_request_sink,
                                client,
                                req_header,
                                routes::RouteRequestArgs::Get(
                                    routes::GetRouteArgs::Dump,
                                ),
                            ).await
                            .expect("route dump requests are infallible");
                        },
                        AF_INET => {
                            process_routes_worker_request::<_, Ipv4>(
                                v4_routes_request_sink,
                                client,
                                req_header,
                                routes::RouteRequestArgs::Get(
                                    routes::GetRouteArgs::Dump,
                                ),
                            ).await
                            .expect("route dump requests are infallible");
                        },
                        AF_INET6 => {
                            process_routes_worker_request::<_, Ipv6>(
                                v6_routes_request_sink,
                                client,
                                req_header,
                                routes::RouteRequestArgs::Get(
                                    routes::GetRouteArgs::Dump,
                                ),
                            ).await
                            .expect("route dump requests are infallible");
                        },
                        family => {
                            log_debug!(
                                "invalid address family ({}) in route dump request from {}: {:?}",
                                family,
                                client,
                                req
                            );
                            client.send_unicast(
                                netlink_packet::new_error(Err(Errno::EINVAL), req_header));
                            return;
                        }
                    };

                    client.send_unicast(netlink_packet::new_done(req_header))
                }
                GetRule(msg) => {
                    if !is_dump {
                        client.send_unicast(
                            netlink_packet::new_error(Err(Errno::ENOTSUP), req_header)
                        );
                        return;
                    }
                    let ip_versions = match msg.header.family.into() {
                        AF_INET => Either::Left(std::iter::once(IpVersion::V4)),
                        AF_INET6 => Either::Left(std::iter::once(IpVersion::V6)),
                        AF_UNSPEC => Either::Right([IpVersion::V4, IpVersion::V6].into_iter()),
                        family => {
                            client.send_unicast(
                                netlink_packet::new_error(Err(Errno::EAFNOSUPPORT), req_header)
                            );
                            log_debug!("received RTM_GETRULE req from {} with invalid address \
                                family ({}): {:?}", client, family, msg);
                            return;
                        }
                    };
                    for ip_version in ip_versions.into_iter() {
                        let request = RuleRequest {
                                args: RuleRequestArgs::DumpRules,
                                ip_version,
                                sequence_number: req_header.sequence_number,
                                client: client.clone(),
                        };
                        match rules_request_handler.handle_request(request) {
                            Ok(()) => {},
                            Err(e) => {
                                client.send_unicast(netlink_packet::new_error(Err(e), req_header));
                                return;
                            }
                        }
                    }
                    client.send_unicast(netlink_packet::new_done(req_header))
                }
                NewRule(msg) => {
                    if is_replace {
                        log_warn!("unimplemented: RTM_NEWRULE requests with NLM_F_REPLACE set.");
                        client.send_unicast(
                            netlink_packet::new_error(Err(Errno::ENOTSUP), req_header)
                        );
                        return;
                    }
                    let ip_version = match msg.header.family.into() {
                        AF_INET => IpVersion::V4,
                        AF_INET6 => IpVersion::V6,
                        family => {
                            log_debug!("received RTM_NEWRULE req from {} with invalid address \
                                family ({}): {:?}", client, family, msg);
                            client.send_unicast(
                                netlink_packet::new_error(Err(Errno::EAFNOSUPPORT), req_header)
                            );
                            return;
                        }
                    };
                    let request = RuleRequest {
                            args: RuleRequestArgs::New(msg),
                            ip_version,
                            sequence_number: req_header.sequence_number,
                            client: client.clone(),
                    };
                    match rules_request_handler.handle_request(request) {
                        Ok(()) => if expects_ack {
                            client.send_unicast(netlink_packet::new_error(Ok(()), req_header))
                        },
                        Err(e) => client.send_unicast(
                            netlink_packet::new_error(Err(e), req_header)
                        ),
                    }
                }
                DelRule(msg) => {
                    let ip_version = match msg.header.family.into() {
                        AF_INET => IpVersion::V4,
                        AF_INET6 => IpVersion::V6,
                        family => {
                            log_debug!("received RTM_DELRULE req from {} with invalid address \
                                family ({}): {:?}", client, family, msg);
                            client.send_unicast(
                                netlink_packet::new_error(Err(Errno::EAFNOSUPPORT), req_header)
                            );
                            return;
                        }
                    };
                    let request = RuleRequest {
                            args: RuleRequestArgs::Del(msg),
                            ip_version,
                            sequence_number: req_header.sequence_number,
                            client: client.clone(),
                    };
                    match rules_request_handler.handle_request(request) {
                        Ok(()) => if expects_ack {
                            client.send_unicast(netlink_packet::new_error(Ok(()), req_header))
                        },
                        Err(e) => client.send_unicast(
                            netlink_packet::new_error(Err(e), req_header)
                        ),
                    }
                }
                NewRoute(ref message) => {
                    // TODO(https://issues.fuchsia.dev/290803327): Emulate REPLACE by
                    // dispatching a delete then add request to Netstack.
                    let is_replace = req_header.flags & NLM_F_REPLACE == NLM_F_REPLACE;
                    if is_replace {
                        log_warn!("unsupported request type: NLM_F_REPLACE flag present in new \
                            route request from {}: {:?}", client, req);
                        client.send_unicast(
                            netlink_packet::new_error(Err(Errno::ENOTSUP), req_header));
                        return;
                    }

                    if message.header.kind != RTN_UNICAST {
                        log_warn!("unsupported request type: {} route present in new route \
                            request from {}: {:?}, only `RTN_UNICAST` is supported",
                            message.header.kind, client, req);
                        client.send_unicast(
                            netlink_packet::new_error(Err(Errno::ENOTSUP), req_header));
                        return;
                    }

                    let result = match message.header.address_family.into() {
                        AF_INET => {
                            match to_new_route_args::<Ipv4>(message, client, &req) {
                                Ok(req) => {
                                    process_routes_worker_request::<_, Ipv4>(
                                        v4_routes_request_sink,
                                        client,
                                        req_header,
                                        req,
                                    ).await
                                },
                                Err(e) => {
                                    return client.send_unicast(
                                        netlink_packet::new_error(Err(e), req_header)
                                    );
                                }
                            }
                        },
                        AF_INET6 => {
                            match to_new_route_args::<Ipv6>(message, client, &req) {
                                Ok(req) => {
                                    process_routes_worker_request::<_, Ipv6>(
                                        v6_routes_request_sink,
                                        client,
                                        req_header,
                                        req,
                                    ).await
                                },
                                Err(e) => {
                                    return client.send_unicast(
                                        netlink_packet::new_error(Err(e), req_header)
                                    );
                                }
                            }
                        },
                        family => {
                            log_debug!("invalid address family ({}) in new route \
                                request from {}: {:?}", family, client, req);
                            return client.send_unicast(
                                netlink_packet::new_error(Err(Errno::EINVAL), req_header)
                            );
                        }
                    };

                    match result {
                        Ok(()) => if expects_ack {
                            client.send_unicast(netlink_packet::new_error(Ok(()), req_header))
                        },
                        Err(e) => client.send_unicast(
                            netlink_packet::new_error(Err(e.into_errno()), req_header)),
                    }
                }
                DelRoute(ref message) => {
                    if message.header.kind != RTN_UNICAST {
                        log_warn!("unsupported request type: {} route present in new route \
                            request from {}: {:?}, only `RTN_UNICAST` is supported",
                            message.header.kind, client, req);
                        client.send_unicast(
                            netlink_packet::new_error(Err(Errno::ENOTSUP), req_header));
                        return;
                    }

                    let result = match message.header.address_family.into() {
                        AF_INET => match to_del_route_args::<Ipv4>(message, client, &req) {
                            Ok(req) => {
                                process_routes_worker_request::<_, Ipv4>(
                                    v4_routes_request_sink,
                                    client,
                                    req_header,
                                    req,
                                ).await
                            },
                            Err(e) => {
                                return client.send_unicast(
                                    netlink_packet::new_error(Err(e), req_header)
                                );
                            }
                        },
                        AF_INET6 => match to_del_route_args::<Ipv6>(message, client, &req) {
                            Ok(req) => {
                                process_routes_worker_request::<_, Ipv6>(
                                    v6_routes_request_sink,
                                    client,
                                    req_header,
                                    req,
                                ).await
                            },
                            Err(e) => {
                                return client.send_unicast(
                                netlink_packet::new_error(Err(e), req_header)
                                );
                            }
                        },
                        family => {
                            log_debug!("invalid address family ({}) in new route \
                                request from {}: {:?}", family, client, req);
                             return client.send_unicast(
                                 netlink_packet::new_error(Err(Errno::EINVAL), req_header)
                             );
                        }
                    };

                    match result {
                        Ok(()) => if expects_ack {
                            client.send_unicast(netlink_packet::new_error(Ok(()), req_header))
                        },
                        Err(e) => client.send_unicast(
                            netlink_packet::new_error(Err(e.into_errno()), req_header)
                        ),
                    }
                }
                NewLink(_)
                | DelLink(_)
                | NewLinkProp(_)
                | DelLinkProp(_)
                | NewNeighbourTable(_)
                | SetNeighbourTable(_)
                | NewTrafficClass(_)
                | DelTrafficClass(_)
                | NewTrafficFilter(_)
                | DelTrafficFilter(_)
                | NewTrafficChain(_)
                | DelTrafficChain(_)
                | NewNsId(_)
                | DelNsId(_)
                // TODO(https://issues.fuchsia.dev/285127790): Implement NewNeighbour.
                | NewNeighbour(_)
                // TODO(https://issues.fuchsia.dev/285127790): Implement DelNeighbour.
                | DelNeighbour(_)
                // TODO(https://issues.fuchsia.dev/283137907): Implement NewQueueDiscipline.
                | NewQueueDiscipline(_)
                // TODO(https://issues.fuchsia.dev/283137907): Implement DelQueueDiscipline.
                | DelQueueDiscipline(_) => {
                    if expects_ack {
                        log_warn!(
                            "Received unsupported NETLINK_ROUTE request; responding with an Ack: {:?}",
                            req,
                        );
                        client.send_unicast(netlink_packet::new_error(Ok(()), req_header))
                    } else {
                        log_warn!(
                            "Received unsupported NETLINK_ROUTE request that does not expect an Ack: {:?}",
                            req,
                        )
                    }
                }
                GetNeighbourTable(_)
                | GetTrafficClass(_)
                | GetTrafficFilter(_)
                | GetTrafficChain(_)
                | GetNsId(_)
                // TODO(https://issues.fuchsia.dev/285127384): Implement GetNeighbour.
                | GetNeighbour(_)
                // TODO(https://issues.fuchsia.dev/278565021): Implement GetAddress.
                | GetAddress(_)
                // Non-dump GetRoute is not currently necessary for our use.
                | GetRoute(_)
                // TODO(https://issues.fuchsia.dev/283137907): Implement GetQueueDiscipline.
                | GetQueueDiscipline(_) => {
                    if is_dump {
                        log_warn!(
                            "Received unsupported NETLINK_ROUTE DUMP request; responding with Done: {:?}",
                            req
                        );
                        client.send_unicast(netlink_packet::new_done(req_header))
                    } else if expects_ack {
                        log_warn!(
                            "Received unsupported NETLINK_ROUTE GET request: responding with Ack {:?}",
                            req
                        );
                        client.send_unicast(netlink_packet::new_error(Ok(()), req_header))
                    } else {
                        log_warn!(
                            "Received unsupported NETLINK_ROUTE GET request that does not expect an Ack {:?}",
                            req
                        )
                    }
                },
                req => panic!("unexpected RtnlMessage: {:?}", req),
            }
        }
    }

    // Dispatch a route request to the given v4 or v6 Routes sink.
    async fn process_routes_worker_request<
        S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>,
        I: Ip,
    >(
        sink: &mut mpsc::Sender<routes::Request<S, I>>,
        client: &mut InternalClient<NetlinkRoute, S>,
        req_header: NetlinkHeader,
        route_request: routes::RouteRequestArgs<I>,
    ) -> Result<(), routes::RequestError> {
        let (completer, waiter) = oneshot::channel();
        sink.send(routes::Request {
            args: routes::RequestArgs::Route(route_request),
            sequence_number: req_header.sequence_number,
            client: client.clone(),
            completer,
        })
        .await
        .expect("route event loop should never terminate");
        waiter.await.expect("routes event loop should have handled the request")
    }

    /// A connection to the Route Netlink Protocol family.
    pub struct NetlinkRouteClient(pub(crate) ExternalClient<NetlinkRoute>);

    impl NetlinkRouteClient {
        /// Sets the PID assigned to the client.
        pub fn set_pid(&self, pid: NonZeroU32) {
            let NetlinkRouteClient(client) = self;
            client.set_port_number(pid)
        }

        /// Adds the given multicast group membership.
        pub fn add_membership(&self, group: ModernGroup) -> Result<(), InvalidModernGroupError> {
            let NetlinkRouteClient(client) = self;
            client.add_membership(group)
        }

        /// Deletes the given multicast group membership.
        pub fn del_membership(&self, group: ModernGroup) -> Result<(), InvalidModernGroupError> {
            let NetlinkRouteClient(client) = self;
            client.del_membership(group)
        }

        /// Sets the legacy multicast group memberships.
        pub fn set_legacy_memberships(
            &self,
            legacy_memberships: LegacyGroups,
        ) -> Result<(), InvalidLegacyGroupsError> {
            let NetlinkRouteClient(client) = self;
            client.set_legacy_memberships(legacy_memberships)
        }
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;

    use netlink_packet_core::NetlinkHeader;

    pub(crate) const LEGACY_GROUP1: u32 = 0x00000001;
    pub(crate) const LEGACY_GROUP2: u32 = 0x00000002;
    pub(crate) const LEGACY_GROUP3: u32 = 0x00000004;
    pub(crate) const INVALID_LEGACY_GROUP: u32 = 0x00000008;
    pub(crate) const MODERN_GROUP1: ModernGroup = ModernGroup(1);
    pub(crate) const MODERN_GROUP2: ModernGroup = ModernGroup(2);
    pub(crate) const MODERN_GROUP3: ModernGroup = ModernGroup(3);
    pub(crate) const INVALID_MODERN_GROUP: ModernGroup = ModernGroup(4);

    #[derive(Debug)]
    pub(crate) enum FakeProtocolFamily {}

    impl MulticastCapableNetlinkFamily for FakeProtocolFamily {
        fn is_valid_group(group: &ModernGroup) -> bool {
            match *group {
                MODERN_GROUP1 | MODERN_GROUP2 | MODERN_GROUP3 => true,
                _ => false,
            }
        }
    }

    pub(crate) fn new_fake_netlink_message() -> NetlinkMessage<FakeNetlinkInnerMessage> {
        NetlinkMessage::new(
            NetlinkHeader::default(),
            NetlinkPayload::InnerMessage(FakeNetlinkInnerMessage),
        )
    }

    #[derive(Clone, Debug, Default, PartialEq)]
    pub(crate) struct FakeNetlinkInnerMessage;

    impl NetlinkSerializable for FakeNetlinkInnerMessage {
        fn message_type(&self) -> u16 {
            u16::MAX
        }

        fn buffer_len(&self) -> usize {
            0
        }

        fn serialize(&self, _buffer: &mut [u8]) {}
    }

    /// Handler of [`FakeNetlinkInnerMessage`] requests.
    ///
    /// Reflects the given request back as the response.
    #[derive(Clone)]
    pub(crate) struct FakeNetlinkRequestHandler;

    #[async_trait]
    impl<S: Sender<FakeNetlinkInnerMessage>> NetlinkFamilyRequestHandler<FakeProtocolFamily, S>
        for FakeNetlinkRequestHandler
    {
        async fn handle_request(
            &mut self,
            req: NetlinkMessage<FakeNetlinkInnerMessage>,
            client: &mut InternalClient<FakeProtocolFamily, S>,
        ) {
            client.send_unicast(req)
        }
    }

    impl ProtocolFamily for FakeProtocolFamily {
        type InnerMessage = FakeNetlinkInnerMessage;
        type RequestHandler<S: Sender<Self::InnerMessage>> = FakeNetlinkRequestHandler;

        const NAME: &'static str = "FAKE_PROTOCOL_FAMILY";
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use std::{
        collections::VecDeque,
        num::{NonZeroU32, NonZeroU64},
        sync::{Arc, Mutex},
    };

    use fidl_fuchsia_net_routes_ext as fnet_routes_ext;

    use assert_matches::assert_matches;
    use futures::{channel::mpsc, future::FutureExt as _, stream::StreamExt as _};
    use net_declare::{net_addr_subnet, net_ip_v4, net_ip_v6, net_subnet_v4, net_subnet_v6};
    use net_types::{
        ip::{
            AddrSubnetEither, GenericOverIp, Ip, IpInvariant, IpVersion, Ipv4, Ipv4Addr, Ipv6,
            Ipv6Addr, Subnet,
        },
        SpecifiedAddr, Witness as _,
    };
    use netlink_packet_core::{NetlinkHeader, NLM_F_ACK, NLM_F_DUMP, NLM_F_REPLACE};
    use netlink_packet_route::{
        rtnl::address::nlas::Nla as AddressNla, rtnl::route::nlas::Nla as RouteNla, AddressMessage,
        LinkMessage, RouteMessage, RtnlMessage, RuleMessage, TcMessage, AF_INET, AF_INET6,
        AF_PACKET, AF_UNSPEC, IFA_F_NOPREFIXROUTE, IFF_UP, RTN_MULTICAST, RTN_UNICAST,
        RT_TABLE_COMPAT, RT_TABLE_MAIN,
    };
    use test_case::test_case;

    use crate::{
        interfaces,
        messaging::testutil::{FakeSender, SentMessage},
        netlink_packet::{self, errno::Errno},
        protocol_family::route::{NetlinkRoute, NetlinkRouteRequestHandler},
        routes,
        rules::{RuleRequest, RuleRequestArgs, RuleRequestHandler, RuleTable},
    };

    enum ExpectedResponse {
        Ack,
        Error(Errno),
        Done,
    }

    fn header_with_flags(flags: u16) -> NetlinkHeader {
        let mut header = NetlinkHeader::default();
        header.flags = flags;
        header
    }

    /// Tests that unhandled requests are treated as a no-op.
    ///
    /// Get requests are responded to with a Done message if the dump flag
    /// is set, an Ack message if the ack flag is set or nothing. New/Del
    /// requests are responded to with an Ack message if the ack flag is set
    /// or nothing.
    #[test_case(
        RtnlMessage::GetTrafficChain,
        0,
        None; "get_with_no_flags")]
    #[test_case(
        RtnlMessage::GetTrafficChain,
        NLM_F_ACK,
        Some(ExpectedResponse::Ack); "get_with_ack_flag")]
    #[test_case(
        RtnlMessage::GetTrafficChain,
        NLM_F_DUMP,
        Some(ExpectedResponse::Done); "get_with_dump_flag")]
    #[test_case(
        RtnlMessage::GetTrafficChain,
        NLM_F_ACK | NLM_F_DUMP,
        Some(ExpectedResponse::Done); "get_with_ack_and_dump_flag")]
    #[test_case(
        RtnlMessage::NewTrafficChain,
        0,
        None; "new_with_no_flags")]
    #[test_case(
        RtnlMessage::NewTrafficChain,
        NLM_F_DUMP,
        None; "new_with_dump_flag")]
    #[test_case(
        RtnlMessage::NewTrafficChain,
        NLM_F_ACK,
        Some(ExpectedResponse::Ack); "new_with_ack_flag")]
    #[test_case(
        RtnlMessage::NewTrafficChain,
        NLM_F_ACK | NLM_F_DUMP,
        Some(ExpectedResponse::Ack); "new_with_ack_and_dump_flags")]
    #[test_case(
        RtnlMessage::DelTrafficChain,
        0,
        None; "del_with_no_flags")]
    #[test_case(
        RtnlMessage::DelTrafficChain,
        NLM_F_DUMP,
        None; "del_with_dump_flag")]
    #[test_case(
        RtnlMessage::DelTrafficChain,
        NLM_F_ACK,
        Some(ExpectedResponse::Ack); "del_with_ack_flag")]
    #[test_case(
        RtnlMessage::DelTrafficChain,
        NLM_F_ACK | NLM_F_DUMP,
        Some(ExpectedResponse::Ack); "del_with_ack_and_dump_flags")]
    #[fuchsia::test]
    async fn test_handle_unsupported_request_response(
        tc_fn: fn(TcMessage) -> RtnlMessage,
        flags: u16,
        expected_response: Option<ExpectedResponse>,
    ) {
        let (interfaces_request_sink, _interfaces_request_stream) = mpsc::channel(0);
        let (v4_routes_request_sink, _v4_routes_request_stream) = mpsc::channel(0);
        let (v6_routes_request_sink, _v6_routes_request_stream) = mpsc::channel(0);

        let mut handler = NetlinkRouteRequestHandler::<FakeSender<_>, _> {
            interfaces_request_sink,
            v4_routes_request_sink,
            v6_routes_request_sink,
            rules_request_handler: RuleTable::new(),
        };

        let (mut client_sink, mut client) = crate::client::testutil::new_fake_client::<NetlinkRoute>(
            crate::client::testutil::CLIENT_ID_1,
            &[],
        );

        let header = header_with_flags(flags);

        handler
            .handle_request(
                NetlinkMessage::new(
                    header,
                    NetlinkPayload::InnerMessage(tc_fn(TcMessage::default())),
                ),
                &mut client,
            )
            .await;

        match expected_response {
            Some(ExpectedResponse::Ack) => {
                assert_eq!(
                    client_sink.take_messages(),
                    [SentMessage::unicast(netlink_packet::new_error(Ok(()), header))]
                )
            }
            Some(ExpectedResponse::Error(e)) => {
                assert_eq!(
                    client_sink.take_messages(),
                    [SentMessage::unicast(netlink_packet::new_error(Err(e), header))]
                )
            }
            Some(ExpectedResponse::Done) => {
                assert_eq!(
                    client_sink.take_messages(),
                    [SentMessage::unicast(netlink_packet::new_done(header))]
                )
            }
            None => {
                assert_eq!(client_sink.take_messages(), [])
            }
        }
    }

    struct RequestAndResponse<R> {
        request: R,
        response: Result<(), interfaces::RequestError>,
    }

    async fn test_request(
        request: NetlinkMessage<RtnlMessage>,
        req_and_resp: Option<RequestAndResponse<interfaces::RequestArgs>>,
    ) -> Vec<SentMessage<RtnlMessage>> {
        let (mut client_sink, mut client) = crate::client::testutil::new_fake_client::<NetlinkRoute>(
            crate::client::testutil::CLIENT_ID_1,
            &[],
        );

        let (interfaces_request_sink, mut interfaces_request_stream) = mpsc::channel(0);
        let (v4_routes_request_sink, _v4_routes_request_stream) = mpsc::channel(0);
        let (v6_routes_request_sink, _v6_routes_request_stream) = mpsc::channel(0);

        let mut handler = NetlinkRouteRequestHandler::<FakeSender<_>, _> {
            interfaces_request_sink,
            v4_routes_request_sink,
            v6_routes_request_sink,
            rules_request_handler: RuleTable::new(),
        };

        let ((), ()) = futures::future::join(handler.handle_request(request, &mut client), async {
            let next = interfaces_request_stream.next();
            match req_and_resp {
                Some(RequestAndResponse { request, response }) => {
                    let interfaces::Request { args, sequence_number: _, client: _, completer } =
                        next.await.expect("handler should send request");
                    assert_eq!(args, request);
                    completer.send(response).expect("handler should be alive");
                }
                None => assert_matches!(next.now_or_never(), None),
            }
        })
        .await;

        client_sink.take_messages()
    }

    const FAKE_INTERFACE_ID: u32 = 1;
    const FAKE_INTERFACE_NAME: &str = "interface";

    /// Test RTM_GETLINK.
    #[test_case(
        0,
        0,
        None,
        None,
        Ok(()),
        Some(ExpectedResponse::Error(Errno::EINVAL)); "no_specifiers")]
    #[test_case(
        NLM_F_DUMP,
        0,
        None,
        Some(interfaces::GetLinkArgs::Dump),
        Ok(()),
        Some(ExpectedResponse::Done); "dump")]
    #[test_case(
        NLM_F_DUMP | NLM_F_ACK,
        0,
        None,
        Some(interfaces::GetLinkArgs::Dump),
        Ok(()),
        Some(ExpectedResponse::Done); "dump_with_ack")]
    #[test_case(
        NLM_F_DUMP,
        FAKE_INTERFACE_ID,
        None,
        Some(interfaces::GetLinkArgs::Dump),
        Ok(()),
        Some(ExpectedResponse::Done); "dump_with_id")]
    #[test_case(
        NLM_F_DUMP,
        FAKE_INTERFACE_ID,
        Some(FAKE_INTERFACE_NAME),
        Some(interfaces::GetLinkArgs::Dump),
        Ok(()),
        Some(ExpectedResponse::Done); "dump_with_id_and_name")]
    #[test_case(
        0,
        FAKE_INTERFACE_ID,
        None,
        Some(interfaces::GetLinkArgs::Get(interfaces::LinkSpecifier::Index(
            NonZeroU32::new(FAKE_INTERFACE_ID).unwrap()))),
        Ok(()),
        None; "id")]
    #[test_case(
        NLM_F_ACK,
        FAKE_INTERFACE_ID,
        None,
        Some(interfaces::GetLinkArgs::Get(interfaces::LinkSpecifier::Index(
            NonZeroU32::new(FAKE_INTERFACE_ID).unwrap()))),
        Ok(()),
        Some(ExpectedResponse::Ack); "id_with_ack")]
    #[test_case(
        0,
        FAKE_INTERFACE_ID,
        Some(FAKE_INTERFACE_NAME),
        Some(interfaces::GetLinkArgs::Get(interfaces::LinkSpecifier::Index(
            NonZeroU32::new(FAKE_INTERFACE_ID).unwrap()))),
        Ok(()),
        None; "id_with_name")]
    #[test_case(
        0,
        0,
        Some(FAKE_INTERFACE_NAME),
        Some(interfaces::GetLinkArgs::Get(interfaces::LinkSpecifier::Name(
            FAKE_INTERFACE_NAME.to_string()))),
        Ok(()),
        None; "name")]
    #[test_case(
        NLM_F_ACK,
        0,
        Some(FAKE_INTERFACE_NAME),
        Some(interfaces::GetLinkArgs::Get(interfaces::LinkSpecifier::Name(
            FAKE_INTERFACE_NAME.to_string()))),
        Ok(()),
        Some(ExpectedResponse::Ack); "name_with_ack")]
    #[test_case(
        0,
        FAKE_INTERFACE_ID,
        None,
        Some(interfaces::GetLinkArgs::Get(interfaces::LinkSpecifier::Index(
            NonZeroU32::new(FAKE_INTERFACE_ID).unwrap()))),
        Err(interfaces::RequestError::UnrecognizedInterface),
        Some(ExpectedResponse::Error(Errno::ENODEV)); "id_not_found")]
    #[test_case(
        0,
        0,
        Some(FAKE_INTERFACE_NAME),
        Some(interfaces::GetLinkArgs::Get(interfaces::LinkSpecifier::Name(
            FAKE_INTERFACE_NAME.to_string()))),
        Err(interfaces::RequestError::UnrecognizedInterface),
        Some(ExpectedResponse::Error(Errno::ENODEV)); "name_not_found")]
    #[fuchsia::test]
    async fn test_get_link(
        flags: u16,
        link_id: u32,
        link_name: Option<&str>,
        expected_request_args: Option<interfaces::GetLinkArgs>,
        interfaces_worker_result: Result<(), interfaces::RequestError>,
        expected_response: Option<ExpectedResponse>,
    ) {
        let header = header_with_flags(flags);
        let mut link_message = LinkMessage::default();
        link_message.header.index = link_id;
        link_message.nlas = link_name
            .map(|n| netlink_packet_route::rtnl::link::nlas::Nla::IfName(n.to_string()))
            .into_iter()
            .collect();

        pretty_assertions::assert_eq!(
            test_request(
                NetlinkMessage::new(
                    header,
                    NetlinkPayload::InnerMessage(RtnlMessage::GetLink(link_message)),
                ),
                expected_request_args.map(|a| RequestAndResponse {
                    request: interfaces::RequestArgs::Link(interfaces::LinkRequestArgs::Get(a)),
                    response: interfaces_worker_result,
                }),
            )
            .await,
            expected_response
                .into_iter()
                .map(|expected_response| {
                    SentMessage::unicast(match expected_response {
                        ExpectedResponse::Ack => netlink_packet::new_error(Ok(()), header),
                        ExpectedResponse::Error(e) => netlink_packet::new_error(Err(e), header),
                        ExpectedResponse::Done => netlink_packet::new_done(header),
                    })
                })
                .collect::<Vec<_>>(),
        )
    }

    #[test_case(
        0,
        0,
        None,
        0,
        0,
        None,
        Ok(()),
        Some(ExpectedResponse::Error(Errno::EINVAL)); "interface_not_specified")]
    #[test_case(
        0,
        FAKE_INTERFACE_ID,
        Some(FAKE_INTERFACE_NAME),
        0,
        0,
        None,
        Ok(()),
        Some(ExpectedResponse::Error(Errno::EBUSY)); "name_and_id")]
    #[test_case(
        0,
        FAKE_INTERFACE_ID,
        None,
        0,
        0,
        Some(interfaces::SetLinkArgs{
            link: interfaces::LinkSpecifier::Index(NonZeroU32::new(FAKE_INTERFACE_ID).unwrap()),
            enable: None,
        }),
        Ok(()),
        None; "no_change_by_id")]
    #[test_case(
        0,
        0,
        Some(FAKE_INTERFACE_NAME),
        0,
        0,
        Some(interfaces::SetLinkArgs{
            link: interfaces::LinkSpecifier::Name(FAKE_INTERFACE_NAME.to_string()),
            enable: None,
        }),
        Ok(()),
        None; "no_change_by_name")]
    #[test_case(
        NLM_F_ACK,
        0,
        Some(FAKE_INTERFACE_NAME),
        0,
        0,
        Some(interfaces::SetLinkArgs{
            link: interfaces::LinkSpecifier::Name(FAKE_INTERFACE_NAME.to_string()),
            enable: None,
        }),
        Ok(()),
        Some(ExpectedResponse::Ack); "no_change_ack")]
    #[test_case(
        0,
        0,
        Some(FAKE_INTERFACE_NAME),
        IFF_UP,
        IFF_UP,
        Some(interfaces::SetLinkArgs{
            link: interfaces::LinkSpecifier::Name(FAKE_INTERFACE_NAME.to_string()),
            enable: Some(true),
        }),
        Ok(()),
        None; "enable")]
    #[test_case(
        NLM_F_ACK,
        0,
        Some(FAKE_INTERFACE_NAME),
        IFF_UP,
        IFF_UP,
        Some(interfaces::SetLinkArgs{
            link: interfaces::LinkSpecifier::Name(FAKE_INTERFACE_NAME.to_string()),
            enable: Some(true),
        }),
        Ok(()),
        Some(ExpectedResponse::Ack); "enable_ack")]
    #[test_case(
        NLM_F_ACK,
        0,
        Some(FAKE_INTERFACE_NAME),
        IFF_UP,
        IFF_UP,
        Some(interfaces::SetLinkArgs{
            link: interfaces::LinkSpecifier::Name(FAKE_INTERFACE_NAME.to_string()),
            enable: Some(true),
        }),
        Err(interfaces::RequestError::UnrecognizedInterface),
        Some(ExpectedResponse::Error(Errno::ENODEV)); "enable_error")]
    #[test_case(
        0,
        0,
        Some(FAKE_INTERFACE_NAME),
        0,
        IFF_UP,
        Some(interfaces::SetLinkArgs{
            link: interfaces::LinkSpecifier::Name(FAKE_INTERFACE_NAME.to_string()),
            enable: Some(false),
        }),
        Ok(()),
        None; "disable")]
    #[test_case(
        NLM_F_ACK,
        0,
        Some(FAKE_INTERFACE_NAME),
        0,
        IFF_UP,
        Some(interfaces::SetLinkArgs{
            link: interfaces::LinkSpecifier::Name(FAKE_INTERFACE_NAME.to_string()),
            enable: Some(false),
        }),
        Ok(()),
        Some(ExpectedResponse::Ack); "disable_ack")]
    #[test_case(
        NLM_F_ACK,
        0,
        Some(FAKE_INTERFACE_NAME),
        0,
        IFF_UP,
        Some(interfaces::SetLinkArgs{
            link: interfaces::LinkSpecifier::Name(FAKE_INTERFACE_NAME.to_string()),
            enable: Some(false),
        }),
        Err(interfaces::RequestError::UnrecognizedInterface),
        Some(ExpectedResponse::Error(Errno::ENODEV)); "disable_error")]
    #[fuchsia::test]
    async fn test_set_link(
        flags: u16,
        link_id: u32,
        link_name: Option<&str>,
        link_flags: u32,
        change_mask: u32,
        expected_request_args: Option<interfaces::SetLinkArgs>,
        interfaces_worker_result: Result<(), interfaces::RequestError>,
        expected_response: Option<ExpectedResponse>,
    ) {
        let header = header_with_flags(flags);
        let mut link_message = LinkMessage::default();
        link_message.header.index = link_id;
        link_message.header.flags = link_flags;
        link_message.header.change_mask = change_mask;
        link_message.nlas = link_name
            .map(|n| netlink_packet_route::rtnl::link::nlas::Nla::IfName(n.to_string()))
            .into_iter()
            .collect();

        pretty_assertions::assert_eq!(
            test_request(
                NetlinkMessage::new(
                    header,
                    NetlinkPayload::InnerMessage(RtnlMessage::SetLink(link_message)),
                ),
                expected_request_args.map(|a| RequestAndResponse {
                    request: interfaces::RequestArgs::Link(interfaces::LinkRequestArgs::Set(a)),
                    response: interfaces_worker_result,
                }),
            )
            .await,
            expected_response
                .into_iter()
                .map(|expected_response| {
                    SentMessage::unicast(match expected_response {
                        ExpectedResponse::Ack => netlink_packet::new_error(Ok(()), header),
                        ExpectedResponse::Error(e) => netlink_packet::new_error(Err(e), header),
                        ExpectedResponse::Done => netlink_packet::new_done(header),
                    })
                })
                .collect::<Vec<_>>(),
        )
    }

    /// Test RTM_GETADDR.
    #[test_case(
        0,
        AF_UNSPEC,
        None,
        None; "af_unspec_no_flags")]
    #[test_case(
        NLM_F_ACK,
        AF_UNSPEC,
        None,
        Some(ExpectedResponse::Ack); "af_unspec_ack_flag")]
    #[test_case(
        NLM_F_DUMP,
        AF_UNSPEC,
        Some(interfaces::GetAddressArgs::Dump {
            ip_version_filter: None,
        }),
        Some(ExpectedResponse::Done); "af_unspec_dump_flag")]
    #[test_case(
        NLM_F_DUMP | NLM_F_ACK,
        AF_UNSPEC,
        Some(interfaces::GetAddressArgs::Dump {
            ip_version_filter: None,
        }),
        Some(ExpectedResponse::Done); "af_unspec_dump_and_ack_flags")]
    #[test_case(
        0,
        AF_INET,
        None,
        None; "af_inet_no_flags")]
    #[test_case(
        NLM_F_ACK,
        AF_INET,
        None,
        Some(ExpectedResponse::Ack); "af_inet_ack_flag")]
    #[test_case(
        NLM_F_DUMP,
        AF_INET,
        Some(interfaces::GetAddressArgs::Dump {
            ip_version_filter: Some(IpVersion::V4),
        }),
        Some(ExpectedResponse::Done); "af_inet_dump_flag")]
    #[test_case(
        NLM_F_DUMP | NLM_F_ACK,
        AF_INET,
        Some(interfaces::GetAddressArgs::Dump {
            ip_version_filter: Some(IpVersion::V4),
        }),
        Some(ExpectedResponse::Done); "af_inet_dump_and_ack_flags")]
    #[test_case(
        0,
        AF_INET6,
        None,
        None; "af_inet6_no_flags")]
    #[test_case(
        NLM_F_ACK,
        AF_INET6,
        None,
        Some(ExpectedResponse::Ack); "af_inet6_ack_flag")]
    #[test_case(
        NLM_F_DUMP,
        AF_INET6,
        Some(interfaces::GetAddressArgs::Dump {
            ip_version_filter: Some(IpVersion::V6),
        }),
        Some(ExpectedResponse::Done); "af_inet6_dump_flag")]
    #[test_case(
        NLM_F_DUMP | NLM_F_ACK,
        AF_INET6,
        Some(interfaces::GetAddressArgs::Dump {
            ip_version_filter: Some(IpVersion::V6),
        }),
        Some(ExpectedResponse::Done); "af_inet6_dump_and_ack_flags")]
    #[test_case(
        0,
        AF_PACKET,
        None,
        None; "af_other_no_flags")]
    #[test_case(
        NLM_F_ACK,
        AF_PACKET,
        None,
        Some(ExpectedResponse::Ack); "af_other_ack_flag")]
    #[test_case(
        NLM_F_DUMP,
        AF_PACKET,
        None,
        Some(ExpectedResponse::Error(Errno::EINVAL)); "af_other_dump_flag")]
    #[test_case(
        NLM_F_DUMP | NLM_F_ACK,
        AF_PACKET,
        None,
        Some(ExpectedResponse::Error(Errno::EINVAL)); "af_other_dump_and_ack_flags")]
    #[fuchsia::test]
    async fn test_get_addr(
        flags: u16,
        family: u16,
        expected_request_args: Option<interfaces::GetAddressArgs>,
        expected_response: Option<ExpectedResponse>,
    ) {
        let header = header_with_flags(flags);
        let address_message = {
            let mut message = AddressMessage::default();
            message.header.family = family.try_into().unwrap();
            message
        };

        pretty_assertions::assert_eq!(
            test_request(
                NetlinkMessage::new(
                    header,
                    NetlinkPayload::InnerMessage(RtnlMessage::GetAddress(address_message)),
                ),
                expected_request_args.map(|a| RequestAndResponse {
                    request: interfaces::RequestArgs::Address(interfaces::AddressRequestArgs::Get(
                        a
                    )),
                    response: Ok(()),
                }),
            )
            .await,
            expected_response
                .into_iter()
                .map(|expected_response| SentMessage::unicast(match expected_response {
                    ExpectedResponse::Ack => netlink_packet::new_error(Ok(()), header),
                    ExpectedResponse::Error(e) => netlink_packet::new_error(Err(e), header),
                    ExpectedResponse::Done => netlink_packet::new_done(header),
                }))
                .collect::<Vec<_>>(),
        )
    }

    enum AddressRequestKind {
        New { add_subnet_route: bool },
        Del,
    }

    struct TestAddrCase {
        kind: AddressRequestKind,
        flags: u16,
        family: u16,
        nlas: Vec<AddressNla>,
        prefix_len: u8,
        interface_id: u32,
        expected_request_args: Option<RequestAndResponse<interfaces::AddressAndInterfaceArgs>>,
        expected_response: Option<ExpectedResponse>,
    }

    fn bytes_from_addr(a: AddrSubnetEither) -> Vec<u8> {
        match a {
            AddrSubnetEither::V4(a) => a.addr().get().ipv4_bytes().to_vec(),
            AddrSubnetEither::V6(a) => a.addr().get().ipv6_bytes().to_vec(),
        }
    }

    fn prefix_from_addr(a: AddrSubnetEither) -> u8 {
        let (_addr, prefix) = a.addr_prefix();
        prefix
    }

    fn interface_id_as_u32(id: u64) -> u32 {
        id.try_into().unwrap()
    }

    fn valid_new_del_addr_request(
        kind: AddressRequestKind,
        ack: bool,
        addr: AddrSubnetEither,
        extra_nlas: impl IntoIterator<Item = AddressNla>,
        interface_id: u64,
        response: Result<(), interfaces::RequestError>,
    ) -> TestAddrCase {
        TestAddrCase {
            kind,
            flags: if ack { NLM_F_ACK } else { 0 },
            family: match addr {
                AddrSubnetEither::V4(_) => AF_INET,
                AddrSubnetEither::V6(_) => AF_INET6,
            },
            nlas: [AddressNla::Local(bytes_from_addr(addr))]
                .into_iter()
                .chain(extra_nlas)
                .collect(),
            prefix_len: prefix_from_addr(addr),
            interface_id: interface_id_as_u32(interface_id),
            expected_request_args: Some(RequestAndResponse {
                request: interfaces::AddressAndInterfaceArgs {
                    address: addr,
                    interface_id: NonZeroU32::new(interface_id_as_u32(interface_id)).unwrap(),
                },
                response,
            }),
            expected_response: ack.then_some(ExpectedResponse::Ack),
        }
    }

    fn valid_new_addr_request_with_extra_nlas(
        ack: bool,
        addr: AddrSubnetEither,
        extra_nlas: impl IntoIterator<Item = AddressNla>,
        interface_id: u64,
        response: Result<(), interfaces::RequestError>,
    ) -> TestAddrCase {
        valid_new_del_addr_request(
            AddressRequestKind::New { add_subnet_route: true },
            ack,
            addr,
            extra_nlas,
            interface_id,
            response,
        )
    }

    fn valid_new_addr_request(
        ack: bool,
        addr: AddrSubnetEither,
        interface_id: u64,
        response: Result<(), interfaces::RequestError>,
    ) -> TestAddrCase {
        valid_new_addr_request_with_extra_nlas(ack, addr, None, interface_id, response)
    }

    fn invalid_new_addr_request(
        ack: bool,
        addr: AddrSubnetEither,
        interface_id: u64,
        errno: Errno,
    ) -> TestAddrCase {
        TestAddrCase {
            expected_request_args: None,
            expected_response: Some(ExpectedResponse::Error(errno)),
            ..valid_new_addr_request(ack, addr, interface_id, Ok(()))
        }
    }

    fn valid_del_addr_request(
        ack: bool,
        addr: AddrSubnetEither,
        interface_id: u64,
        response: Result<(), interfaces::RequestError>,
    ) -> TestAddrCase {
        valid_new_del_addr_request(AddressRequestKind::Del, ack, addr, None, interface_id, response)
    }

    fn invalid_del_addr_request(
        ack: bool,
        addr: AddrSubnetEither,
        interface_id: u64,
        errno: Errno,
    ) -> TestAddrCase {
        TestAddrCase {
            expected_request_args: None,
            expected_response: Some(ExpectedResponse::Error(errno)),
            ..valid_del_addr_request(ack, addr, interface_id, Ok(()))
        }
    }

    /// Test RTM_NEWADDR and RTM_DELADDR
    // Add address tests cases.
    #[test_case(
        TestAddrCase {
            expected_request_args: None,
            ..valid_new_addr_request(
                true,
                net_addr_subnet!("0.0.0.0/0"),
                interfaces::testutil::PPP_INTERFACE_ID,
                Ok(()))
        }; "new_v4_unspecified_address_zero_prefix_ok_ack")]
    #[test_case(
        TestAddrCase {
            expected_request_args: None,
            ..valid_new_addr_request(
                false,
                net_addr_subnet!("0.0.0.0/24"),
                interfaces::testutil::PPP_INTERFACE_ID,
                Ok(()))
        }; "new_v4_unspecified_address_non_zero_prefix_ok_no_ack")]
    #[test_case(
        invalid_new_addr_request(
            true,
            net_addr_subnet!("::/0"),
            interfaces::testutil::ETH_INTERFACE_ID,
            Errno::EADDRNOTAVAIL); "new_v6_unspecified_address_zero_prefix_ack")]
    #[test_case(
        invalid_new_addr_request(
            false,
            net_addr_subnet!("::/64"),
            interfaces::testutil::ETH_INTERFACE_ID,
            Errno::EADDRNOTAVAIL); "new_v6_unspecified_address_non_zero_prefix_no_ack")]
    #[test_case(
        valid_new_addr_request(
            true,
            interfaces::testutil::test_addr_subnet_v4(),
            interfaces::testutil::LO_INTERFACE_ID,
            Ok(())); "new_v4_ok_ack")]
    #[test_case(
        valid_new_addr_request(
            true,
            interfaces::testutil::test_addr_subnet_v6(),
            interfaces::testutil::LO_INTERFACE_ID,
            Ok(())); "new_v6_ok_ack")]
    #[test_case(
        valid_new_addr_request(
            false,
            interfaces::testutil::test_addr_subnet_v4(),
            interfaces::testutil::ETH_INTERFACE_ID,
            Ok(())); "new_v4_ok_no_ack")]
    #[test_case(
        valid_new_addr_request(
            false,
            interfaces::testutil::test_addr_subnet_v6(),
            interfaces::testutil::ETH_INTERFACE_ID,
            Ok(())); "new_v6_ok_no_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![
                AddressNla::Local(bytes_from_addr(interfaces::testutil::test_addr_subnet_v4())),
            ],
            ..valid_new_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::ETH_INTERFACE_ID,
                Ok(()),
            )
        }; "new_v4_local_nla_ok_no_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![
                AddressNla::Address(bytes_from_addr(interfaces::testutil::test_addr_subnet_v6())),
            ],
            ..valid_new_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::PPP_INTERFACE_ID,
                Ok(()),
            )
        }; "new_v6_address_nla_ok_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![
                AddressNla::Address(bytes_from_addr(interfaces::testutil::test_addr_subnet_v6())),
                AddressNla::Local(bytes_from_addr(interfaces::testutil::test_addr_subnet_v6())),
            ],
            ..valid_new_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::PPP_INTERFACE_ID,
                Ok(()),
            )
        }; "new_v6_same_local_and_address_nla_ok_no_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![
                AddressNla::Local(bytes_from_addr(interfaces::testutil::test_addr_subnet_v6())),
                AddressNla::Address(Vec::new()),
            ],
            ..invalid_new_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::ENOTSUP,
            )
        }; "new_v6_valid_local_and_empty_address_nlas_no_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![
                AddressNla::Local(Vec::new()),
                AddressNla::Address(bytes_from_addr(interfaces::testutil::test_addr_subnet_v4())),
            ],
            ..invalid_new_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::ETH_INTERFACE_ID,
                Errno::ENOTSUP,
            )
        }; "new_v4_empty_local_and_valid_address_nlas_no_ack")]
    #[test_case(
        TestAddrCase {
            kind: AddressRequestKind::New { add_subnet_route: true },
            ..valid_new_addr_request_with_extra_nlas(
                false,
                interfaces::testutil::test_addr_subnet_v4(),
                [AddressNla::Flags(0)],
                interfaces::testutil::ETH_INTERFACE_ID,
                Ok(()),
            )
        }; "new_v4_with_route_ok_no_ack")]
    #[test_case(
        TestAddrCase {
            kind: AddressRequestKind::New { add_subnet_route: false },
            ..valid_new_addr_request_with_extra_nlas(
                true,
                interfaces::testutil::test_addr_subnet_v6(),
                [AddressNla::Flags(IFA_F_NOPREFIXROUTE)],
                interfaces::testutil::LO_INTERFACE_ID,
                Ok(()),
            )
        }; "new_v6_without_route_ok_ack")]
    #[test_case(
        TestAddrCase {
            expected_response: Some(ExpectedResponse::Error(Errno::EINVAL)),
            ..valid_new_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::LO_INTERFACE_ID,
                Err(interfaces::RequestError::InvalidRequest),
            )
        }; "new_v4_invalid_response_ack")]
    #[test_case(
        TestAddrCase {
            expected_response: Some(ExpectedResponse::Error(Errno::EEXIST)),
            ..valid_new_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::LO_INTERFACE_ID,
                Err(interfaces::RequestError::AlreadyExists),
            )
        }; "new_v6_exist_response_no_ack")]
    #[test_case(
        TestAddrCase {
            expected_response: Some(ExpectedResponse::Error(Errno::ENODEV)),
            ..valid_new_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Err(interfaces::RequestError::UnrecognizedInterface),
            )
        }; "new_v6_unrecognized_interface_response_no_ack")]
    #[test_case(
        TestAddrCase {
            expected_response: Some(ExpectedResponse::Error(Errno::EADDRNOTAVAIL)),
            ..valid_new_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::ETH_INTERFACE_ID,
                Err(interfaces::RequestError::AddressNotFound),
            )
        }; "new_v4_not_found_response_ck")]
    #[test_case(
        TestAddrCase {
            interface_id: 0,
            ..invalid_new_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::LO_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "new_zero_interface_id_ack")]
    #[test_case(
        TestAddrCase {
            interface_id: 0,
            ..invalid_new_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "new_zero_interface_id_no_ack")]
    #[test_case(
        TestAddrCase {
            nlas: Vec::new(),
            ..invalid_new_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "new_no_nlas_ack")]
    #[test_case(
        TestAddrCase {
            nlas: Vec::new(),
            ..invalid_new_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "new_no_nlas_no_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![AddressNla::Flags(0)],
            ..invalid_new_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "new_missing_address_and_local_nla_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![AddressNla::Flags(0)],
            ..invalid_new_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "new_missing_address_and_local_nla_no_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![AddressNla::Local(Vec::new())],
            ..invalid_new_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "new_invalid_local_nla_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![AddressNla::Local(Vec::new())],
            ..invalid_new_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "new_invalid_local_nla_no_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![AddressNla::Address(Vec::new())],
            ..invalid_new_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "new_invalid_address_nla_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![AddressNla::Address(Vec::new())],
            ..invalid_new_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "new_invalid_address_nla_no_ack")]
    #[test_case(
        TestAddrCase {
            prefix_len: 0,
            ..valid_new_addr_request(
                true,
                net_addr_subnet!("192.0.2.123/0"),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Ok(()),
            )
        }; "new_zero_prefix_len_ack")]
    #[test_case(
        TestAddrCase {
            prefix_len: 0,
            ..valid_new_addr_request(
                false,
                net_addr_subnet!("2001:db8::1324/0"),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Ok(()),
            )
        }; "new_zero_prefix_len_no_ack")]
    #[test_case(
        TestAddrCase {
            prefix_len: u8::MAX,
            ..invalid_new_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "new_invalid_prefix_len_ack")]
    #[test_case(
        TestAddrCase {
            prefix_len: u8::MAX,
            ..invalid_new_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "new_invalid_prefix_len_no_ack")]
    #[test_case(
        TestAddrCase {
            family: AF_UNSPEC,
            ..invalid_new_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::LO_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "new_invalid_family_ack")]
    #[test_case(
        TestAddrCase {
            family: AF_UNSPEC,
            ..invalid_new_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::PPP_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "new_invalid_family_no_ack")]
    // Delete address tests cases.
    #[test_case(
        valid_del_addr_request(
            true,
            interfaces::testutil::test_addr_subnet_v4(),
            interfaces::testutil::LO_INTERFACE_ID,
            Ok(())); "del_v4_ok_ack")]
    #[test_case(
        valid_del_addr_request(
            true,
            interfaces::testutil::test_addr_subnet_v6(),
            interfaces::testutil::LO_INTERFACE_ID,
            Ok(())); "del_v6_ok_ack")]
    #[test_case(
        valid_del_addr_request(
            false,
            interfaces::testutil::test_addr_subnet_v4(),
            interfaces::testutil::ETH_INTERFACE_ID,
            Ok(())); "del_v4_ok_no_ack")]
    #[test_case(
        valid_del_addr_request(
            false,
            interfaces::testutil::test_addr_subnet_v6(),
            interfaces::testutil::ETH_INTERFACE_ID,
            Ok(())); "del_v6_ok_no_ack")]
    #[test_case(
        TestAddrCase {
            expected_response: Some(ExpectedResponse::Error(Errno::EINVAL)),
            ..valid_del_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::LO_INTERFACE_ID,
                Err(interfaces::RequestError::InvalidRequest),
            )
        }; "del_v4_invalid_response_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![
                AddressNla::Local(bytes_from_addr(interfaces::testutil::test_addr_subnet_v4())),
            ],
            ..valid_del_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::ETH_INTERFACE_ID,
                Ok(()),
            )
        }; "del_v4_local_nla_ok_no_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![
                AddressNla::Address(bytes_from_addr(interfaces::testutil::test_addr_subnet_v6())),
            ],
            ..valid_del_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::PPP_INTERFACE_ID,
                Ok(()),
            )
        }; "del_v6_address_nla_ok_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![
                AddressNla::Address(bytes_from_addr(interfaces::testutil::test_addr_subnet_v4())),
                AddressNla::Local(bytes_from_addr(interfaces::testutil::test_addr_subnet_v4())),
            ],
            ..valid_del_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::ETH_INTERFACE_ID,
                Ok(()),
            )
        }; "del_v4_same_local_and_address_nla_ok_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![
                AddressNla::Local(bytes_from_addr(interfaces::testutil::test_addr_subnet_v6())),
                AddressNla::Address(Vec::new()),
            ],
            ..invalid_del_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::ENOTSUP,
            )
        }; "del_v6_valid_local_and_empty_address_nlas_no_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![
                AddressNla::Local(Vec::new()),
                AddressNla::Address(bytes_from_addr(interfaces::testutil::test_addr_subnet_v4())),
            ],
            ..invalid_del_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::ETH_INTERFACE_ID,
                Errno::ENOTSUP,
            )
        }; "del_v4_empty_local_and_valid_address_nlas_no_ack")]
    #[test_case(
        TestAddrCase {
            expected_response: Some(ExpectedResponse::Error(Errno::EEXIST)),
            ..valid_del_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::LO_INTERFACE_ID,
                Err(interfaces::RequestError::AlreadyExists),
            )
        }; "del_v6_exist_response_no_ack")]
    #[test_case(
        TestAddrCase {
            expected_response: Some(ExpectedResponse::Error(Errno::ENODEV)),
            ..valid_del_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Err(interfaces::RequestError::UnrecognizedInterface),
            )
        }; "del_v6_unrecognized_interface_response_no_ack")]
    #[test_case(
        TestAddrCase {
            expected_response: Some(ExpectedResponse::Error(Errno::EADDRNOTAVAIL)),
            ..valid_del_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::ETH_INTERFACE_ID,
                Err(interfaces::RequestError::AddressNotFound),
            )
        }; "del_v4_not_found_response_ck")]
    #[test_case(
        TestAddrCase {
            interface_id: 0,
            ..invalid_del_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::LO_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "del_zero_interface_id_ack")]
    #[test_case(
        TestAddrCase {
            interface_id: 0,
            ..invalid_del_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "del_zero_interface_id_no_ack")]
    #[test_case(
        TestAddrCase {
            nlas: Vec::new(),
            ..invalid_del_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "del_no_nlas_ack")]
    #[test_case(
        TestAddrCase {
            nlas: Vec::new(),
            ..invalid_del_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "del_no_nlas_no_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![AddressNla::Flags(0)],
            ..invalid_del_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "del_missing_address_and_local_nla_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![AddressNla::Flags(0)],
            ..invalid_del_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "del_missing_address_and_local_nla_no_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![AddressNla::Local(Vec::new())],
            ..invalid_del_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "del_invalid_local_nla_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![AddressNla::Local(Vec::new())],
            ..invalid_del_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "del_invalid_local_nla_no_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![AddressNla::Address(Vec::new())],
            ..invalid_del_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "del_invalid_address_nla_ack")]
    #[test_case(
        TestAddrCase {
            nlas: vec![AddressNla::Address(Vec::new())],
            ..invalid_del_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "del_invalid_address_nla_no_ack")]
    #[test_case(
        TestAddrCase {
            prefix_len: 0,
            ..valid_del_addr_request(
                true,
                net_addr_subnet!("192.0.2.123/0"),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Ok(()),
            )
        }; "del_zero_prefix_len_ack")]
    #[test_case(
        TestAddrCase {
            prefix_len: 0,
            ..valid_del_addr_request(
                false,
                net_addr_subnet!("2001:db8::1324/0"),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Ok(()),
            )
        }; "del_zero_prefix_len_no_ack")]
    #[test_case(
        TestAddrCase {
            prefix_len: u8::MAX,
            ..invalid_del_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "del_invalid_prefix_len_ack")]
    #[test_case(
        TestAddrCase {
            prefix_len: u8::MAX,
            ..invalid_del_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "del_invalid_prefix_len_no_ack")]
    #[test_case(
        TestAddrCase {
            family: AF_UNSPEC,
            ..invalid_del_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::LO_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "del_invalid_family_ack")]
    #[test_case(
        TestAddrCase {
            family: AF_UNSPEC,
            ..invalid_del_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::PPP_INTERFACE_ID,
                Errno::EINVAL,
            )
        }; "del_invalid_family_no_ack")]
    #[fuchsia::test]
    async fn test_new_del_addr(test_case: TestAddrCase) {
        let TestAddrCase {
            kind,
            flags,
            family,
            nlas,
            prefix_len,
            interface_id,
            expected_request_args,
            expected_response,
        } = test_case;

        let header = header_with_flags(flags);
        let address_message = {
            let mut message = AddressMessage::default();
            message.header.family = family.try_into().unwrap();
            message.header.index = interface_id;
            message.header.prefix_len = prefix_len;
            message.nlas = nlas;
            message
        };

        let (message, request) = match kind {
            AddressRequestKind::New { add_subnet_route } => (
                RtnlMessage::NewAddress(address_message),
                expected_request_args.map(|RequestAndResponse { request, response }| {
                    RequestAndResponse {
                        request: interfaces::RequestArgs::Address(
                            interfaces::AddressRequestArgs::New(interfaces::NewAddressArgs {
                                address_and_interface_id: request,
                                add_subnet_route,
                            }),
                        ),
                        response,
                    }
                }),
            ),
            AddressRequestKind::Del => (
                RtnlMessage::DelAddress(address_message),
                expected_request_args.map(|RequestAndResponse { request, response }| {
                    RequestAndResponse {
                        request: interfaces::RequestArgs::Address(
                            interfaces::AddressRequestArgs::Del(interfaces::DelAddressArgs {
                                address_and_interface_id: request,
                            }),
                        ),
                        response,
                    }
                }),
            ),
        };

        pretty_assertions::assert_eq!(
            test_request(
                NetlinkMessage::new(header, NetlinkPayload::InnerMessage(message),),
                request,
            )
            .await,
            expected_response
                .into_iter()
                .map(|response| SentMessage::unicast(match response {
                    ExpectedResponse::Ack => netlink_packet::new_error(Ok(()), header),
                    ExpectedResponse::Done => netlink_packet::new_done(header),
                    ExpectedResponse::Error(e) => netlink_packet::new_error(Err(e), header),
                }))
                .collect::<Vec<_>>(),
        )
    }

    // Separate from `test_route_request`, because RTM_GETROUTE requests do not have typed
    // arguments and there needs to be an option to send a DUMP request to the
    // v4 and v6 routes worker.
    async fn test_get_route_request(
        family: u16,
        request: NetlinkMessage<RtnlMessage>,
        expected_request: Option<routes::GetRouteArgs>,
    ) -> Vec<SentMessage<RtnlMessage>> {
        let (mut client_sink, mut client) = crate::client::testutil::new_fake_client::<NetlinkRoute>(
            crate::client::testutil::CLIENT_ID_1,
            &[],
        );

        let (interfaces_request_sink, _interfaces_request_stream) = mpsc::channel(0);
        let (v4_routes_request_sink, mut v4_routes_request_stream) = mpsc::channel(0);
        let (v6_routes_request_sink, mut v6_routes_request_stream) = mpsc::channel(0);

        let mut handler = NetlinkRouteRequestHandler::<FakeSender<_>, _> {
            interfaces_request_sink,
            v4_routes_request_sink,
            v6_routes_request_sink,
            rules_request_handler: RuleTable::new(),
        };
        let ((), ()) = futures::future::join(handler.handle_request(request, &mut client), async {
            if family == AF_UNSPEC || family == AF_INET {
                let next = v4_routes_request_stream.next();
                match expected_request
                    .map(|a| routes::RequestArgs::<Ipv4>::Route(routes::RouteRequestArgs::Get(a)))
                {
                    Some(expected_request) => {
                        let routes::Request { args, sequence_number: _, client: _, completer } =
                            next.await.expect("handler should send request");
                        assert_eq!(args, expected_request);
                        completer.send(Ok(())).expect("handler should be alive");
                    }
                    None => assert_matches!(next.now_or_never(), None),
                };
            }
            if family == AF_UNSPEC || family == AF_INET6 {
                let next = v6_routes_request_stream.next();
                match expected_request
                    .map(|a| routes::RequestArgs::<Ipv6>::Route(routes::RouteRequestArgs::Get(a)))
                {
                    Some(expected_request) => {
                        let routes::Request { args, sequence_number: _, client: _, completer } =
                            next.await.expect("handler should send request");
                        assert_eq!(args, expected_request);
                        completer.send(Ok(())).expect("handler should be alive");
                    }
                    None => assert_matches!(next.now_or_never(), None),
                };
            }
        })
        .await;

        client_sink.take_messages()
    }

    /// Test RTM_GETROUTE.
    #[test_case(
        0,
        AF_UNSPEC,
        None,
        None; "af_unspec_no_flags")]
    #[test_case(
        NLM_F_ACK,
        AF_UNSPEC,
        None,
        Some(ExpectedResponse::Ack); "af_unspec_ack_flag")]
    #[test_case(
        NLM_F_DUMP,
        AF_UNSPEC,
        Some(routes::GetRouteArgs::Dump),
        Some(ExpectedResponse::Done); "af_unspec_dump_flag")]
    #[test_case(
        NLM_F_DUMP | NLM_F_ACK,
        AF_UNSPEC,
        Some(routes::GetRouteArgs::Dump),
        Some(ExpectedResponse::Done); "af_unspec_dump_and_ack_flags")]
    #[test_case(
        0,
        AF_INET,
        None,
        None; "af_inet_no_flags")]
    #[test_case(
        NLM_F_ACK,
        AF_INET,
        None,
        Some(ExpectedResponse::Ack); "af_inet_ack_flag")]
    #[test_case(
        NLM_F_DUMP,
        AF_INET,
        Some(routes::GetRouteArgs::Dump),
        Some(ExpectedResponse::Done); "af_inet_dump_flag")]
    #[test_case(
        NLM_F_DUMP | NLM_F_ACK,
        AF_INET,
        Some(routes::GetRouteArgs::Dump),
        Some(ExpectedResponse::Done); "af_inet_dump_and_ack_flags")]
    #[test_case(
        0,
        AF_INET6,
        None,
        None; "af_inet6_no_flags")]
    #[test_case(
        NLM_F_ACK,
        AF_INET6,
        None,
        Some(ExpectedResponse::Ack); "af_inet6_ack_flag")]
    #[test_case(
        NLM_F_DUMP,
        AF_INET6,
        Some(routes::GetRouteArgs::Dump),
        Some(ExpectedResponse::Done); "af_inet6_dump_flag")]
    #[test_case(
        NLM_F_DUMP | NLM_F_ACK,
        AF_INET6,
        Some(routes::GetRouteArgs::Dump),
        Some(ExpectedResponse::Done); "af_inet6_dump_and_ack_flags")]
    #[test_case(
        0,
        AF_PACKET,
        None,
        None; "af_other_no_flags")]
    #[test_case(
        NLM_F_ACK,
        AF_PACKET,
        None,
        Some(ExpectedResponse::Ack); "af_other_ack_flag")]
    #[test_case(
        NLM_F_DUMP,
        AF_PACKET,
        None,
        Some(ExpectedResponse::Error(Errno::EINVAL)); "af_other_dump_flag")]
    #[test_case(
        NLM_F_DUMP | NLM_F_ACK,
        AF_PACKET,
        None,
        Some(ExpectedResponse::Error(Errno::EINVAL)); "af_other_dump_and_ack_flags")]
    #[fuchsia::test]
    async fn test_get_route(
        flags: u16,
        family: u16,
        expected_request_args: Option<routes::GetRouteArgs>,
        expected_response: Option<ExpectedResponse>,
    ) {
        let header = header_with_flags(flags);
        let route_message = {
            let mut message = RouteMessage::default();
            message.header.address_family = family.try_into().unwrap();
            message
        };

        pretty_assertions::assert_eq!(
            test_get_route_request(
                family,
                NetlinkMessage::new(
                    header,
                    NetlinkPayload::InnerMessage(RtnlMessage::GetRoute(route_message)),
                ),
                expected_request_args,
            )
            .await,
            expected_response
                .into_iter()
                .map(|expected_response| SentMessage::unicast(match expected_response {
                    ExpectedResponse::Ack => netlink_packet::new_error(Ok(()), header),
                    ExpectedResponse::Error(e) => netlink_packet::new_error(Err(e), header),
                    ExpectedResponse::Done => netlink_packet::new_done(header),
                }))
                .collect::<Vec<_>>(),
        )
    }

    /// Represents a single expected request, and the fake response.
    #[derive(Debug)]
    pub(crate) struct FakeRuleRequestResponse {
        pub(crate) expected_request_args: RuleRequestArgs,
        pub(crate) expected_ip_version: IpVersion,
        pub(crate) response: Result<(), Errno>,
    }

    /// A fake implementation of [`RuleRequestHandler`].
    ///
    /// Handles a sequence of rule requests by pulling the expected request
    /// and fake response from the front of `requests_and_responses`.
    #[derive(Clone, Debug)]
    pub(crate) struct FakeRuleRequestHandler {
        pub(crate) requests_and_responses: Arc<Mutex<VecDeque<FakeRuleRequestResponse>>>,
    }

    impl FakeRuleRequestHandler {
        fn new(requests_and_responses: impl IntoIterator<Item = FakeRuleRequestResponse>) -> Self {
            FakeRuleRequestHandler {
                requests_and_responses: Arc::new(Mutex::new(VecDeque::from_iter(
                    requests_and_responses,
                ))),
            }
        }
    }

    impl<S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>> RuleRequestHandler<S>
        for FakeRuleRequestHandler
    {
        fn handle_request(&mut self, actual_request: RuleRequest<S>) -> Result<(), Errno> {
            let Self { requests_and_responses } = self;
            let FakeRuleRequestResponse { expected_request_args, expected_ip_version, response } =
                requests_and_responses.lock().unwrap().pop_front().expect(
                    "FakeRuleRequest handler should have a fake request/response pre-configured",
                );
            let RuleRequest { args, ip_version, sequence_number: _, client: _ } = actual_request;
            assert_eq!(args, expected_request_args);
            assert_eq!(ip_version, expected_ip_version);
            response
        }
    }

    fn default_rule_for_family(family: u16) -> RuleMessage {
        let mut rule = RuleMessage::default();
        rule.header.family = family.try_into().expect("address family should fit in a u8");
        rule
    }

    const AF_INVALID: u16 = 255;

    #[test_case(
        RtnlMessage::GetRule,
        0,
        AF_UNSPEC,
        vec![],
        Some(ExpectedResponse::Error(Errno::ENOTSUP)); "get_rule_no_dump")]
    #[test_case(
        RtnlMessage::GetRule,
        NLM_F_DUMP,
        AF_INVALID,
        vec![],
        Some(ExpectedResponse::Error(Errno::EAFNOSUPPORT)); "get_rule_dump_invalid_address_family")]
    #[test_case(
        RtnlMessage::GetRule,
        NLM_F_DUMP,
        AF_INET,
        vec![FakeRuleRequestResponse{
            expected_request_args: RuleRequestArgs::DumpRules,
            expected_ip_version: IpVersion::V4,
            response: Ok(()),
        }],
        Some(ExpectedResponse::Done); "get_rule_dump_v4")]
    #[test_case(
        RtnlMessage::GetRule,
        NLM_F_DUMP,
        AF_INET6,
        vec![FakeRuleRequestResponse{
            expected_request_args: RuleRequestArgs::DumpRules,
            expected_ip_version: IpVersion::V6,
            response: Ok(()),
        }],
        Some(ExpectedResponse::Done); "get_rule_dump_v6")]
    #[test_case(
        RtnlMessage::GetRule,
        NLM_F_DUMP,
        AF_UNSPEC,
        vec![FakeRuleRequestResponse{
            expected_request_args: RuleRequestArgs::DumpRules,
            expected_ip_version: IpVersion::V4,
            response: Ok(()),
        },
        FakeRuleRequestResponse{
            expected_request_args: RuleRequestArgs::DumpRules,
            expected_ip_version: IpVersion::V6,
            response: Ok(()),
        }],
        Some(ExpectedResponse::Done); "get_rule_dump_af_unspec")]
    #[test_case(
        RtnlMessage::GetRule,
        NLM_F_DUMP,
        AF_INET,
        vec![FakeRuleRequestResponse{
            expected_request_args: RuleRequestArgs::DumpRules,
            expected_ip_version: IpVersion::V4,
            response: Err(Errno::ENOTSUP),
        }],
        Some(ExpectedResponse::Error(Errno::ENOTSUP)); "get_rule_dump_v4_fails")]
    #[test_case(
        RtnlMessage::GetRule,
        NLM_F_DUMP,
        AF_INET6,
        vec![FakeRuleRequestResponse{
            expected_request_args: RuleRequestArgs::DumpRules,
            expected_ip_version: IpVersion::V6,
            response: Err(Errno::ENOTSUP),
        }],
        Some(ExpectedResponse::Error(Errno::ENOTSUP)); "get_rule_dump_v6_fails")]
    #[test_case(
        RtnlMessage::GetRule,
        NLM_F_DUMP,
        AF_UNSPEC,
        vec![FakeRuleRequestResponse{
            expected_request_args: RuleRequestArgs::DumpRules,
            expected_ip_version: IpVersion::V4,
            response: Err(Errno::ENOTSUP),
        }],
        Some(ExpectedResponse::Error(Errno::ENOTSUP)); "get_rule_dump_af_unspec_v4_fails")]
    #[test_case(
        RtnlMessage::GetRule,
        NLM_F_DUMP,
        AF_UNSPEC,
        vec![FakeRuleRequestResponse{
            expected_request_args: RuleRequestArgs::DumpRules,
            expected_ip_version: IpVersion::V4,
            response: Ok(()),
        },
        FakeRuleRequestResponse{
            expected_request_args: RuleRequestArgs::DumpRules,
            expected_ip_version: IpVersion::V6,
            response: Err(Errno::ENOTSUP),
        }],
        Some(ExpectedResponse::Error(Errno::ENOTSUP)); "get_rule_dump_af_unspec_v6_fails")]
    #[test_case(
        RtnlMessage::NewRule,
        0,
        AF_INET,
        vec![FakeRuleRequestResponse{
            expected_request_args: RuleRequestArgs::New(default_rule_for_family(AF_INET)),
            expected_ip_version: IpVersion::V4,
            response: Ok(()),
        }],
        None; "new_rule_succeeds_v4")]
    #[test_case(
        RtnlMessage::NewRule,
        0,
        AF_INET6,
        vec![FakeRuleRequestResponse{
            expected_request_args: RuleRequestArgs::New(default_rule_for_family(AF_INET6)),
            expected_ip_version: IpVersion::V6,
            response: Ok(()),
        }],
        None; "new_rule_succeeds_v6")]
    #[test_case(
        RtnlMessage::NewRule,
        0,
        AF_UNSPEC,
        vec![],
        Some(ExpectedResponse::Error(Errno::EAFNOSUPPORT)); "new_rule_af_unspec_fails")]
    #[test_case(
        RtnlMessage::NewRule,
        NLM_F_ACK,
        AF_INET,
        vec![FakeRuleRequestResponse{
            expected_request_args: RuleRequestArgs::New(default_rule_for_family(AF_INET)),
            expected_ip_version: IpVersion::V4,
            response: Ok(()),
        }],
        Some(ExpectedResponse::Ack); "new_rule_v4_succeeds_with_ack")]
    #[test_case(
        RtnlMessage::NewRule,
        NLM_F_ACK,
        AF_INET6,
        vec![FakeRuleRequestResponse{
            expected_request_args: RuleRequestArgs::New(default_rule_for_family(AF_INET6)),
            expected_ip_version: IpVersion::V6,
            response: Ok(()),
        }],
        Some(ExpectedResponse::Ack); "new_rule_v6_succeeds_with_ack")]
    #[test_case(
        RtnlMessage::NewRule,
        0,
        AF_INET,
        vec![FakeRuleRequestResponse{
            expected_request_args: RuleRequestArgs::New(default_rule_for_family(AF_INET)),
            expected_ip_version: IpVersion::V4,
            response: Err(Errno::ENOTSUP),
        }],
        Some(ExpectedResponse::Error(Errno::ENOTSUP)); "new_rule_v4_fails")]
    #[test_case(
        RtnlMessage::NewRule,
        0,
        AF_INET6,
        vec![FakeRuleRequestResponse{
            expected_request_args: RuleRequestArgs::New(default_rule_for_family(AF_INET6)),
            expected_ip_version: IpVersion::V6,
            response: Err(Errno::ENOTSUP),
        }],
        Some(ExpectedResponse::Error(Errno::ENOTSUP)); "new_rule_v6_fails")]
    #[test_case(
        RtnlMessage::NewRule,
        NLM_F_REPLACE,
        AF_INET,
        vec![],
        Some(ExpectedResponse::Error(Errno::ENOTSUP)); "new_rule_v4_replace_unimplemented")]
    #[test_case(
        RtnlMessage::NewRule,
        NLM_F_REPLACE,
        AF_INET6,
        vec![],
        Some(ExpectedResponse::Error(Errno::ENOTSUP)); "new_rule_v6_replace_unimplemented")]
    #[test_case(
        RtnlMessage::DelRule,
        0,
        AF_INET,
        vec![FakeRuleRequestResponse{
            expected_request_args: RuleRequestArgs::Del(default_rule_for_family(AF_INET)),
            expected_ip_version: IpVersion::V4,
            response: Ok(()),
        }],
        None; "del_rule_succeeds_v4")]
    #[test_case(
        RtnlMessage::DelRule,
        0,
        AF_INET6,
        vec![FakeRuleRequestResponse{
            expected_request_args: RuleRequestArgs::Del(default_rule_for_family(AF_INET6)),
            expected_ip_version: IpVersion::V6,
            response: Ok(()),
        }],
        None; "del_rule_succeeds_v6")]
    #[test_case(
        RtnlMessage::DelRule,
        0,
        AF_UNSPEC,
        vec![],
        Some(ExpectedResponse::Error(Errno::EAFNOSUPPORT)); "del_rule_af_unspec_fails")]
    #[test_case(
        RtnlMessage::DelRule,
        NLM_F_ACK,
        AF_INET,
        vec![FakeRuleRequestResponse{
            expected_request_args: RuleRequestArgs::Del(default_rule_for_family(AF_INET)),
            expected_ip_version: IpVersion::V4,
            response: Ok(()),
        }],
        Some(ExpectedResponse::Ack); "del_rule_v4_succeeds_with_ack")]
    #[test_case(
        RtnlMessage::DelRule,
        NLM_F_ACK,
        AF_INET6,
        vec![FakeRuleRequestResponse{
            expected_request_args: RuleRequestArgs::Del(default_rule_for_family(AF_INET6)),
            expected_ip_version: IpVersion::V6,
            response: Ok(()),
        }],
        Some(ExpectedResponse::Ack); "del_rule_v6_succeeds_with_ack")]
    #[test_case(
        RtnlMessage::DelRule,
        0,
        AF_INET,
        vec![FakeRuleRequestResponse{
            expected_request_args: RuleRequestArgs::Del(default_rule_for_family(AF_INET)),
            expected_ip_version: IpVersion::V4,
            response: Err(Errno::ENOTSUP),
        }],
        Some(ExpectedResponse::Error(Errno::ENOTSUP)); "del_rule_v4_fails")]
    #[test_case(
        RtnlMessage::DelRule,
        0,
        AF_INET6,
        vec![FakeRuleRequestResponse{
            expected_request_args: RuleRequestArgs::Del(default_rule_for_family(AF_INET6)),
            expected_ip_version: IpVersion::V6,
            response: Err(Errno::ENOTSUP),
        }],
        Some(ExpectedResponse::Error(Errno::ENOTSUP)); "del_rule_v6_fails")]
    #[fuchsia::test]
    async fn test_rule_request(
        rule_fn: fn(RuleMessage) -> RtnlMessage,
        flags: u16,
        address_family: u16,
        requests_and_responses: Vec<FakeRuleRequestResponse>,
        expected_response: Option<ExpectedResponse>,
    ) {
        let (interfaces_request_sink, _interfaces_request_stream) = mpsc::channel(0);
        let (v4_routes_request_sink, _v4_routes_request_stream) = mpsc::channel(0);
        let (v6_routes_request_sink, _v6_routes_request_stream) = mpsc::channel(0);

        let mut handler = NetlinkRouteRequestHandler::<FakeSender<_>, _> {
            interfaces_request_sink,
            v4_routes_request_sink,
            v6_routes_request_sink,
            rules_request_handler: FakeRuleRequestHandler::new(requests_and_responses),
        };

        let (mut client_sink, mut client) = {
            crate::client::testutil::new_fake_client::<NetlinkRoute>(
                crate::client::testutil::CLIENT_ID_1,
                &[],
            )
        };

        let header = header_with_flags(flags);

        handler
            .handle_request(
                NetlinkMessage::new(
                    header,
                    NetlinkPayload::InnerMessage(rule_fn(default_rule_for_family(address_family))),
                ),
                &mut client,
            )
            .await;

        match expected_response {
            Some(ExpectedResponse::Ack) => {
                assert_eq!(
                    client_sink.take_messages(),
                    [SentMessage::unicast(netlink_packet::new_error(Ok(()), header))]
                )
            }
            Some(ExpectedResponse::Error(e)) => {
                assert_eq!(
                    client_sink.take_messages(),
                    [SentMessage::unicast(netlink_packet::new_error(Err(e), header))]
                )
            }
            Some(ExpectedResponse::Done) => {
                assert_eq!(
                    client_sink.take_messages(),
                    [SentMessage::unicast(netlink_packet::new_done(header))]
                )
            }
            None => {
                assert_eq!(client_sink.take_messages(), [])
            }
        }
    }

    const TEST_V4_SUBNET: Subnet<Ipv4Addr> = net_subnet_v4!("192.0.2.0/24");
    const TEST_V4_NEXTHOP: Ipv4Addr = net_ip_v4!("192.0.2.1");
    const TEST_V6_SUBNET: Subnet<Ipv6Addr> = net_subnet_v6!("2001:db8::0/32");
    const TEST_V6_NEXTHOP: Ipv6Addr = net_ip_v6!("2001:db8::1");

    fn test_nexthop_spec_addr<I: Ip>() -> SpecifiedAddr<I::Addr> {
        I::map_ip(
            (),
            |()| SpecifiedAddr::new(TEST_V4_NEXTHOP).unwrap(),
            |()| SpecifiedAddr::new(TEST_V6_NEXTHOP).unwrap(),
        )
    }

    #[derive(Clone)]
    struct RouteRequestAndResponse<R> {
        request: R,
        response: Result<(), routes::RequestError>,
    }

    #[derive(Clone, Copy)]
    enum RouteRequestKind {
        New,
        Del,
    }

    struct TestRouteCase<I: Ip> {
        kind: RouteRequestKind,
        flags: u16,
        family: u16,
        nlas: Vec<RouteNla>,
        destination_prefix_len: u8,
        table: u8,
        rtm_type: u8,
        expected_request_args: Option<RouteRequestAndResponse<routes::RouteRequestArgs<I>>>,
        expected_response: Option<ExpectedResponse>,
    }

    fn bytes_from_spec_addr<I: Ip>(a: &SpecifiedAddr<I::Addr>) -> Vec<u8> {
        let IpInvariant(bytes) = I::map_ip(
            a,
            |a| IpInvariant(a.ipv4_bytes().to_vec()),
            |a| IpInvariant(a.ipv6_bytes().to_vec()),
        );
        bytes
    }

    fn bytes_from_subnet<I: Ip>(a: &Subnet<I::Addr>) -> Vec<u8> {
        let IpInvariant(bytes) = I::map_ip(
            a,
            |a| IpInvariant(a.network().ipv4_bytes().to_vec()),
            |a| IpInvariant(a.network().ipv6_bytes().to_vec()),
        );
        bytes
    }

    fn build_route_test_case<I: Ip>(
        kind: RouteRequestKind,
        flags: u16,
        dst: Subnet<I::Addr>,
        next_hop: Option<SpecifiedAddr<I::Addr>>,
        extra_nlas: impl IntoIterator<Item = RouteNla>,
        interface_id: Option<NonZeroU64>,
        table: u8,
        rtm_type: u8,
        response: Result<(), routes::RequestError>,
    ) -> TestRouteCase<I> {
        let extra_nlas = extra_nlas.into_iter().collect::<Vec<_>>();
        let table_from_nla = extra_nlas.clone().into_iter().find_map(|nla| match nla {
            RouteNla::Table(table) => Some(table),
            _ => None,
        });
        let priority_from_nla = extra_nlas.clone().into_iter().find_map(|nla| match nla {
            RouteNla::Priority(priority) => Some(priority),
            _ => None,
        });
        // `0` is the default metric value for IPv4 routes and `1` is
        // the default for Ipv6 routes when the NLA is not provided.
        let priority_default = {
            let IpInvariant(priority) = I::map_ip((), |()| IpInvariant(0), |()| IpInvariant(1));
            priority
        };

        TestRouteCase::<I> {
            kind,
            flags,
            family: {
                let IpInvariant(family) =
                    I::map_ip((), |()| IpInvariant(AF_INET), |()| IpInvariant(AF_INET6));
                family
            },
            nlas: [RouteNla::Destination(bytes_from_subnet::<I>(&dst))]
                .into_iter()
                .chain(interface_id.map(|id| RouteNla::Oif(interface_id_as_u32(id.get()))))
                .chain(extra_nlas.into_iter())
                .collect(),
            destination_prefix_len: {
                let IpInvariant(prefix_len) = I::map_ip(
                    dst,
                    |dst| IpInvariant(dst.prefix()),
                    |dst| IpInvariant(dst.prefix()),
                );
                prefix_len
            },
            table,
            rtm_type,
            expected_request_args: Some(RouteRequestAndResponse {
                request: {
                    match kind {
                        RouteRequestKind::New => {
                            let interface_id =
                                interface_id.expect("new requests must have interface id");

                            routes::RouteRequestArgs::New::<I>(routes::NewRouteArgs::Unicast(
                                routes::UnicastNewRouteArgs {
                                    subnet: dst,
                                    target: fnet_routes_ext::RouteTarget {
                                        outbound_interface: interface_id.get(),
                                        next_hop,
                                    },
                                    priority: priority_from_nla.unwrap_or(priority_default),
                                    // Use the table value from the NLA if provided.
                                    table: table_from_nla.unwrap_or(table as u32),
                                },
                            ))
                        }
                        RouteRequestKind::Del => {
                            routes::RouteRequestArgs::Del::<I>(routes::DelRouteArgs::Unicast(
                                routes::UnicastDelRouteArgs {
                                    subnet: dst,
                                    outbound_interface: interface_id,
                                    next_hop,
                                    priority: priority_from_nla
                                        .map(|priority| NonZeroU32::new(priority))
                                        .flatten(),
                                    // Use the table value from the NLA if provided. When the NLA
                                    // value is 0, use the value from the header. Default to
                                    // RT_TABLE_MAIN when the header value is unspecified.
                                    table: table_from_nla
                                        .map(|table_nla| NonZeroU32::new(table_nla))
                                        .flatten()
                                        .unwrap_or(NonZeroU32::new(table as u32).unwrap_or(
                                            NonZeroU32::new(RT_TABLE_MAIN as u32).unwrap(),
                                        )),
                                },
                            ))
                        }
                    }
                },
                response,
            }),
            expected_response: (flags & NLM_F_ACK == NLM_F_ACK).then_some(ExpectedResponse::Ack),
        }
    }

    fn build_valid_route_test_case_with_extra_nlas<I: Ip>(
        kind: RouteRequestKind,
        flags: u16,
        addr: Subnet<I::Addr>,
        next_hop: Option<SpecifiedAddr<I::Addr>>,
        extra_nlas: impl IntoIterator<Item = RouteNla>,
        interface_id: Option<u64>,
        table: u8,
        rtm_type: u8,
        response: Result<(), routes::RequestError>,
    ) -> TestRouteCase<I> {
        build_route_test_case::<I>(
            kind,
            flags,
            addr,
            next_hop,
            extra_nlas,
            interface_id.map(|id| NonZeroU64::new(id)).flatten(),
            table,
            rtm_type,
            response,
        )
    }

    fn build_valid_route_test_case<I: Ip>(
        kind: RouteRequestKind,
        flags: u16,
        addr: Subnet<I::Addr>,
        interface_id: Option<u64>,
        table: u8,
        rtm_type: u8,
        response: Result<(), routes::RequestError>,
    ) -> TestRouteCase<I> {
        build_valid_route_test_case_with_extra_nlas::<I>(
            kind,
            flags,
            addr,
            None,
            None,
            interface_id,
            table,
            rtm_type,
            response,
        )
    }

    fn build_invalid_route_test_case<I: Ip>(
        kind: RouteRequestKind,
        flags: u16,
        addr: Subnet<I::Addr>,
        interface_id: Option<u64>,
        table: u8,
        rtm_type: u8,
        errno: Errno,
    ) -> TestRouteCase<I> {
        build_invalid_route_test_case_with_extra_nlas::<I>(
            kind,
            flags,
            addr,
            None,
            None,
            interface_id,
            table,
            rtm_type,
            errno,
        )
    }

    fn build_invalid_route_test_case_with_extra_nlas<I: Ip>(
        kind: RouteRequestKind,
        flags: u16,
        addr: Subnet<I::Addr>,
        next_hop: Option<SpecifiedAddr<I::Addr>>,
        extra_nlas: impl IntoIterator<Item = RouteNla>,
        interface_id: Option<u64>,
        table: u8,
        rtm_type: u8,
        errno: Errno,
    ) -> TestRouteCase<I> {
        TestRouteCase {
            expected_request_args: None,
            expected_response: Some(ExpectedResponse::Error(errno)),
            ..build_valid_route_test_case_with_extra_nlas::<I>(
                kind,
                flags,
                addr,
                next_hop,
                extra_nlas,
                interface_id,
                table,
                rtm_type,
                Ok(()),
            )
        }
    }

    #[derive(Clone, Debug, PartialEq)]
    enum RouteRequestArgsEither {
        V4(routes::RequestArgs<Ipv4>),
        V6(routes::RequestArgs<Ipv6>),
    }

    async fn test_route_request<I: Ip>(
        request: NetlinkMessage<RtnlMessage>,
        req_and_resp: Option<RouteRequestAndResponse<routes::RouteRequestArgs<I>>>,
    ) -> Vec<SentMessage<RtnlMessage>> {
        let (mut client_sink, mut client) = {
            crate::client::testutil::new_fake_client::<NetlinkRoute>(
                crate::client::testutil::CLIENT_ID_1,
                &[],
            )
        };

        let (interfaces_request_sink, _interfaces_request_stream) = mpsc::channel(0);
        let (v4_routes_request_sink, mut v4_routes_request_stream) = mpsc::channel(0);
        let (v6_routes_request_sink, mut v6_routes_request_stream) = mpsc::channel(0);

        let mut handler = NetlinkRouteRequestHandler::<FakeSender<_>, _> {
            interfaces_request_sink,
            v4_routes_request_sink,
            v6_routes_request_sink,
            rules_request_handler: RuleTable::new(),
        };

        match req_and_resp {
            None => {
                handler.handle_request(request, &mut client).await;
                match I::VERSION {
                    IpVersion::V4 => {
                        assert_matches!(v4_routes_request_stream.next().now_or_never(), None)
                    }
                    IpVersion::V6 => {
                        assert_matches!(v6_routes_request_stream.next().now_or_never(), None)
                    }
                }
            }
            Some(RouteRequestAndResponse { request: expected_request, response }) => {
                let ((), ()) =
                    futures::future::join(handler.handle_request(request, &mut client), async {
                        let args = match I::VERSION {
                            IpVersion::V4 => {
                                let next = v4_routes_request_stream.next();
                                let routes::Request {
                                    args,
                                    sequence_number: _,
                                    client: _,
                                    completer,
                                } = next.await.expect("handler should send request");
                                completer.send(response).expect("handler should be alive");
                                RouteRequestArgsEither::V4(args)
                            }
                            IpVersion::V6 => {
                                let next = v6_routes_request_stream.next();
                                let routes::Request {
                                    args,
                                    sequence_number: _,
                                    client: _,
                                    completer,
                                } = next.await.expect("handler should send request");
                                completer.send(response).expect("handler should be alive");
                                RouteRequestArgsEither::V6(args)
                            }
                        };

                        #[derive(GenericOverIp)]
                        struct EqualityInputs<I: Ip> {
                            args: RouteRequestArgsEither,
                            expected_request: routes::RequestArgs<I>,
                        }

                        let () = I::map_ip(
                            EqualityInputs {
                                args: args,
                                expected_request: routes::RequestArgs::Route(expected_request),
                            },
                            |EqualityInputs { args, expected_request }| {
                                let args = assert_matches!(
                                    args,
                                    RouteRequestArgsEither::V4(request) => request
                                );
                                assert_eq!(args, expected_request);
                            },
                            |EqualityInputs { args, expected_request }| {
                                let args = assert_matches!(
                                    args,
                                    RouteRequestArgsEither::V6(request) => request
                                );
                                assert_eq!(args, expected_request);
                            },
                        );
                    })
                    .await;
            }
        }

        client_sink.take_messages()
    }

    /// Test RTM_NEWROUTE and RTM_DELROUTE
    // Add route test cases.
    #[test_case(
        TestRouteCase::<Ipv4> {
            family: AF_UNSPEC,
            ..build_invalid_route_test_case(
                RouteRequestKind::New,
                NLM_F_ACK,
                TEST_V4_SUBNET,
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Errno::EINVAL,
            )
        }; "new_v4_invalid_family_ack")]
    #[test_case(
        TestRouteCase::<Ipv6> {
            family: AF_UNSPEC,
            ..build_invalid_route_test_case(
                RouteRequestKind::New,
                0,
                TEST_V6_SUBNET,
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Errno::EINVAL,
            )
        })]
    #[test_case(
        TestRouteCase::<Ipv4> {
            flags: NLM_F_ACK | NLM_F_REPLACE,
            ..build_invalid_route_test_case::<Ipv4>(
                RouteRequestKind::New,
                NLM_F_ACK,
                TEST_V4_SUBNET,
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Errno::ENOTSUP,
            )
        }; "new_v4_replace_flag_ack")]
    #[test_case(
        build_invalid_route_test_case::<Ipv6>(
            RouteRequestKind::New,
            NLM_F_REPLACE,
            TEST_V6_SUBNET,
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Errno::ENOTSUP,
        ); "new_v6_replace_flag_no_ack")]
    #[test_case(
        build_invalid_route_test_case::<Ipv4>(
            RouteRequestKind::New,
            NLM_F_ACK,
            TEST_V4_SUBNET,
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_MULTICAST,
            Errno::ENOTSUP); "new_v4_non_unicast_type_ack")]
    #[test_case(
        build_invalid_route_test_case::<Ipv6>(
            RouteRequestKind::New,
            0,
            TEST_V6_SUBNET,
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_MULTICAST,
            Errno::ENOTSUP); "new_v6_non_unicast_type_no_ack")]
    #[test_case(
        build_valid_route_test_case::<Ipv4>(
            RouteRequestKind::New,
            NLM_F_ACK,
            net_subnet_v4!("0.0.0.0/0"),
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(())); "new_v4_default_route_ok_ack")]
    #[test_case(
        build_valid_route_test_case::<Ipv4>(
            RouteRequestKind::New,
            0,
            net_subnet_v4!("0.0.0.0/24"),
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(())); "new_v4_unspecified_route_non_zero_prefix_ok_no_ack")]
    #[test_case(
        build_valid_route_test_case::<Ipv6>(
            RouteRequestKind::New,
            NLM_F_ACK,
            net_subnet_v6!("::/0"),
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(())); "new_v6_default_route_prefix_ack")]
    #[test_case(
        build_valid_route_test_case::<Ipv6>(
            RouteRequestKind::New,
            0,
            net_subnet_v6!("::/64"),
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(())); "new_v6_unspecified_route_non_zero_prefix_no_ack")]
    #[test_case(
        build_valid_route_test_case::<Ipv4>(
            RouteRequestKind::New,
            NLM_F_ACK,
            TEST_V4_SUBNET,
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(())); "new_v4_ok_ack")]
    #[test_case(
        build_valid_route_test_case::<Ipv6>(
            RouteRequestKind::New,
            NLM_F_ACK,
            TEST_V6_SUBNET,
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(())); "new_v6_ok_ack")]
    #[test_case(
        build_valid_route_test_case::<Ipv4>(
            RouteRequestKind::New,
            0,
            TEST_V4_SUBNET,
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(())); "new_v4_ok_no_ack")]
    #[test_case(
        build_valid_route_test_case::<Ipv6>(
            RouteRequestKind::New,
            0,
            TEST_V6_SUBNET,
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(())); "new_v6_ok_no_ack")]
    #[test_case(
        build_valid_route_test_case_with_extra_nlas::<Ipv4>(
            RouteRequestKind::New,
            NLM_F_ACK,
            TEST_V4_SUBNET,
            Some(test_nexthop_spec_addr::<Ipv4>()),
            [RouteNla::Gateway(bytes_from_spec_addr::<Ipv4>(
                &test_nexthop_spec_addr::<Ipv4>()
            ))],
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(())); "new_v4_with_nexthop_ok_ack")]
    #[test_case(
        build_valid_route_test_case_with_extra_nlas::<Ipv6>(
            RouteRequestKind::New,
            0,
            TEST_V6_SUBNET,
            Some(test_nexthop_spec_addr::<Ipv6>()),
            [RouteNla::Gateway(bytes_from_spec_addr::<Ipv6>(
                &test_nexthop_spec_addr::<Ipv6>()
            ))],
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(())); "new_v6_with_nexthop_ok_no_ack")]
    #[test_case(
        build_invalid_route_test_case_with_extra_nlas::<Ipv4>(
            RouteRequestKind::New,
            NLM_F_ACK,
            TEST_V4_SUBNET,
            Some(test_nexthop_spec_addr::<Ipv4>()),
            [RouteNla::Gateway(Vec::new())],
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Errno::EINVAL); "new_v4_with_invalid_nexthop_ok_ack")]
    #[test_case(
        build_invalid_route_test_case_with_extra_nlas::<Ipv6>(
            RouteRequestKind::New,
            0,
            TEST_V6_SUBNET,
            Some(test_nexthop_spec_addr::<Ipv6>()),
            [RouteNla::Gateway(Vec::new())],
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Errno::EINVAL); "new_v6_with_invalid_nexthop_ok_no_ack")]
    #[test_case(
        build_valid_route_test_case_with_extra_nlas::<Ipv4>(
            RouteRequestKind::New,
            NLM_F_ACK,
            TEST_V4_SUBNET,
            None,
            [RouteNla::Gateway(net_ip_v4!("0.0.0.0").ipv4_bytes().to_vec())],
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(())); "new_v4_unspecified_nexthop_ok_ack")]
    #[test_case(
        build_valid_route_test_case_with_extra_nlas::<Ipv6>(
            RouteRequestKind::New,
            0,
            TEST_V6_SUBNET,
            None,
            [RouteNla::Gateway(net_ip_v6!("::").ipv6_bytes().to_vec())],
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(())); "new_v6_unspecified_nexthop_no_ack")]
    #[test_case(
        build_valid_route_test_case_with_extra_nlas::<Ipv4>(
            RouteRequestKind::New,
            NLM_F_ACK,
            TEST_V4_SUBNET,
            None,
            [RouteNla::Priority(100)],
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(())); "new_v4_priority_nla_ok_ack")]
    #[test_case(
        build_valid_route_test_case_with_extra_nlas::<Ipv6>(
            RouteRequestKind::New,
            0,
            TEST_V6_SUBNET,
            None,
            [RouteNla::Priority(100)],
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(())); "new_v6_priority_nla_ok_no_ack")]
    #[test_case(
        TestRouteCase::<Ipv4> {
            expected_response: Some(ExpectedResponse::Error(Errno::EINVAL)),
            ..build_valid_route_test_case(
                RouteRequestKind::New,
                NLM_F_ACK,
                TEST_V4_SUBNET,
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Err(routes::RequestError::InvalidRequest),
            )
        }; "new_v4_invalid_request_response_ack")]
    #[test_case(
        TestRouteCase::<Ipv6> {
            expected_response: Some(ExpectedResponse::Error(Errno::EINVAL)),
            ..build_valid_route_test_case(
                RouteRequestKind::New,
                NLM_F_ACK,
                TEST_V6_SUBNET,
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Err(routes::RequestError::InvalidRequest),
            )
        }; "new_v6_invalid_request_response_no_ack")]
    #[test_case(
        TestRouteCase::<Ipv6> {
            expected_response: Some(ExpectedResponse::Error(Errno::ENODEV)),
            ..build_valid_route_test_case(
                RouteRequestKind::New,
                0,
                TEST_V6_SUBNET,
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Err(routes::RequestError::UnrecognizedInterface),
            )
        }; "new_v6_unrecognized_interface_response_no_ack")]
    #[test_case(
        TestRouteCase::<Ipv6> {
            expected_response: Some(ExpectedResponse::Error(Errno::ENOTSUP)),
            ..build_valid_route_test_case(
                RouteRequestKind::New,
                0,
                TEST_V6_SUBNET,
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Err(routes::RequestError::Unknown),
            )
        }; "new_v6_unknown_response_no_ack")]
    #[test_case(
        TestRouteCase::<Ipv4> {
            nlas: vec![
                RouteNla::Destination(bytes_from_subnet::<Ipv4>(
                    &TEST_V4_SUBNET
                )),
            ],
            ..build_invalid_route_test_case(
                RouteRequestKind::New,
                0,
                TEST_V4_SUBNET,
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Errno::ENOTSUP,
            )
        }; "new_v4_missing_oif_nla_ack")]
    #[test_case(
        TestRouteCase::<Ipv6> {
            nlas: vec![
                RouteNla::Destination(bytes_from_subnet::<Ipv6>(
                    &TEST_V6_SUBNET
                )),
            ],
            ..build_invalid_route_test_case(
                RouteRequestKind::New,
                0,
                TEST_V6_SUBNET,
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Errno::ENOTSUP,
            )
        }; "new_v6_missing_oif_nla_ack")]
    #[test_case(
        TestRouteCase::<Ipv4> {
            nlas: vec![
                RouteNla::Destination(bytes_from_subnet::<Ipv4>(
                    &TEST_V4_SUBNET
                )),
            ],
            ..build_invalid_route_test_case(
                RouteRequestKind::New,
                0,
                TEST_V4_SUBNET,
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Errno::ENOTSUP,
            )
        }; "new_v4_missing_oif_nla_no_ack")]
    #[test_case(
        TestRouteCase::<Ipv6> {
            nlas: vec![
                RouteNla::Destination(bytes_from_subnet::<Ipv6>(
                    &TEST_V6_SUBNET
                )),
            ],
            ..build_invalid_route_test_case(
                RouteRequestKind::New,
                0,
                TEST_V6_SUBNET,
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Errno::ENOTSUP,
            )
        }; "new_v6_missing_oif_nla_no_ack")]
    #[test_case(
        TestRouteCase::<Ipv4> {
            nlas: vec![
                RouteNla::Oif(interfaces::testutil::ETH_INTERFACE_ID.try_into().unwrap()),
            ],
            destination_prefix_len: 1,
            ..build_invalid_route_test_case(
                RouteRequestKind::New,
                NLM_F_ACK,
                TEST_V4_SUBNET,
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Errno::EINVAL,
            )
        }; "new_v4_missing_destination_nla_nonzero_prefix_ack")]
    #[test_case(
        TestRouteCase::<Ipv6> {
            nlas: vec![
                RouteNla::Oif(interfaces::testutil::ETH_INTERFACE_ID.try_into().unwrap()),
            ],
            destination_prefix_len: 1,
            ..build_invalid_route_test_case(
                RouteRequestKind::New,
                0,
                TEST_V6_SUBNET,
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Errno::EINVAL,
            )
        }; "new_v6_missing_destination_nla_nonzero_prefix_no_ack")]
    #[test_case(
        TestRouteCase::<Ipv4> {
            nlas: vec![
                RouteNla::Oif(interfaces::testutil::LO_INTERFACE_ID.try_into().unwrap()),
            ],
            ..build_valid_route_test_case(
                RouteRequestKind::New,
                NLM_F_ACK,
                net_subnet_v4!("0.0.0.0/0"),
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Ok(()))}; "new_v4_missing_destination_nla_zero_prefix_ack")]
    #[test_case(
        TestRouteCase::<Ipv6> {
            nlas: vec![
                RouteNla::Oif(interfaces::testutil::LO_INTERFACE_ID.try_into().unwrap()),
            ],
            ..build_valid_route_test_case(
                RouteRequestKind::New,
                0,
                net_subnet_v6!("::/0"),
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Ok(()))}; "new_v6_missing_destination_nla_zero_prefix_no_ack")]
    #[test_case(
        TestRouteCase::<Ipv4> {
            nlas: Vec::new(),
            ..build_invalid_route_test_case(
                RouteRequestKind::New,
                NLM_F_ACK,
                TEST_V4_SUBNET,
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Errno::EINVAL,
            )
        }; "new_v4_no_nlas_ack")]
    #[test_case(
        TestRouteCase::<Ipv6> {
            nlas: Vec::new(),
            ..build_invalid_route_test_case(
                RouteRequestKind::New,
                0,
                TEST_V6_SUBNET,
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Errno::EINVAL,
            )
        }; "new_v6_no_nlas_no_ack")]
    #[test_case(
        TestRouteCase::<Ipv4> {
            nlas: vec![RouteNla::Destination(Vec::new())],
            ..build_invalid_route_test_case(
                RouteRequestKind::New,
                NLM_F_ACK,
                TEST_V4_SUBNET,
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Errno::EINVAL,
            )
        }; "new_v4_invalid_destination_nla_ack")]
    #[test_case(
        TestRouteCase::<Ipv6> {
            nlas: vec![RouteNla::Destination(Vec::new())],
            ..build_invalid_route_test_case(
                RouteRequestKind::New,
                0,
                TEST_V6_SUBNET,
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Errno::EINVAL,
            )
        }; "new_v6_invalid_destination_nla_no_ack")]
    #[test_case(
        build_valid_route_test_case_with_extra_nlas::<Ipv4>(
            RouteRequestKind::New,
            NLM_F_ACK,
            TEST_V4_SUBNET,
            None,
            // `RT_TABLE_COMPAT` is generally used when `table` is outside the bounds of u8 values.
            [RouteNla::Table(u8::MAX as u32 + 1)],
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_COMPAT,
            RTN_UNICAST,
            Ok(())); "new_v4_with_table_nla_rt_table_compat_ack")]
    #[test_case(
        build_valid_route_test_case_with_extra_nlas::<Ipv6>(
            RouteRequestKind::New,
            0,
            TEST_V6_SUBNET,
            None,
            // `RT_TABLE_COMPAT` is generally used when `table` is outside the bounds of u8 values.
            [RouteNla::Table(u8::MAX as u32 + 1)],
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_COMPAT,
            RTN_UNICAST,
            Ok(())); "new_v6_with_table_nla_rt_table_compat_no_ack")]
    #[test_case(
        build_valid_route_test_case_with_extra_nlas::<Ipv4>(
            RouteRequestKind::New,
            NLM_F_ACK,
            TEST_V4_SUBNET,
            None,
            [RouteNla::Table(u8::MAX as u32 + 1)],
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(())); "new_v4_with_table_nla_rt_table_main_ack")]
    #[test_case(
        build_valid_route_test_case_with_extra_nlas::<Ipv6>(
            RouteRequestKind::New,
            0,
            TEST_V6_SUBNET,
            None,
            [RouteNla::Table(u8::MAX as u32 + 1)],
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(())); "new_v6_with_table_nla_rt_table_main_no_ack")]
    #[test_case(
        TestRouteCase::<Ipv4> {
            destination_prefix_len: u8::MAX,
            ..build_invalid_route_test_case(
                RouteRequestKind::New,
                NLM_F_ACK,
                TEST_V4_SUBNET,
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Errno::EINVAL,
            )
        }; "new_v4_invalid_prefix_len_ack")]
    #[test_case(
        TestRouteCase::<Ipv6> {
            destination_prefix_len: u8::MAX,
            ..build_invalid_route_test_case(
                RouteRequestKind::New,
                0,
                TEST_V6_SUBNET,
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Errno::EINVAL,
            )
        }; "new_v6_invalid_prefix_len_no_ack")]
    #[test_case(
        TestRouteCase::<Ipv4> {
            destination_prefix_len: 0,
            ..build_invalid_route_test_case(
                RouteRequestKind::New,
                NLM_F_ACK,
                TEST_V4_SUBNET,
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Errno::EINVAL,
            )
        }; "new_v4_zero_prefix_len_ack")]
    #[test_case(
        TestRouteCase::<Ipv6> {
            destination_prefix_len: 0,
            ..build_invalid_route_test_case(
                RouteRequestKind::New,
                0,
                TEST_V6_SUBNET,
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Errno::EINVAL,
            )
        }; "new_v6_zero_prefix_len_no_ack")]
    // Delete route test cases.
    #[test_case(
        TestRouteCase::<Ipv4> {
            family: AF_UNSPEC,
            ..build_invalid_route_test_case(
                RouteRequestKind::Del,
                NLM_F_ACK,
                TEST_V4_SUBNET,
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Errno::EINVAL,
            )
        }; "del_v4_invalid_family_ack")]
    #[test_case(
        TestRouteCase::<Ipv6> {
            family: AF_UNSPEC,
            ..build_invalid_route_test_case(
                RouteRequestKind::Del,
                0,
                TEST_V6_SUBNET,
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Errno::EINVAL,
            )
        }; "del_v6_invalid_family_no_ack")]
    #[test_case(
        build_invalid_route_test_case::<Ipv4>(
            RouteRequestKind::Del,
            NLM_F_ACK,
            TEST_V4_SUBNET,
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_MULTICAST,
            Errno::ENOTSUP); "del_v4_non_unicast_type_ack")]
    #[test_case(
        build_invalid_route_test_case::<Ipv6>(
            RouteRequestKind::Del,
            0,
            TEST_V6_SUBNET,
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_MULTICAST,
            Errno::ENOTSUP); "del_v6_non_unicast_type_no_ack")]
    #[test_case(
        build_valid_route_test_case::<Ipv4>(
            RouteRequestKind::Del,
            NLM_F_ACK,
            net_subnet_v4!("0.0.0.0/0"),
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(())); "del_v4_default_route_ok_ack")]
    #[test_case(
        build_valid_route_test_case::<Ipv4>(
            RouteRequestKind::Del,
            0,
            net_subnet_v4!("0.0.0.0/24"),
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(())); "del_v4_unspecified_route_non_zero_prefix_ok_no_ack")]
    #[test_case(
        build_valid_route_test_case::<Ipv6>(
            RouteRequestKind::Del,
            NLM_F_ACK,
            net_subnet_v6!("::/0"),
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(())); "del_v6_default_route_prefix_ack")]
    #[test_case(
        build_valid_route_test_case::<Ipv6>(
            RouteRequestKind::Del,
            0,
            net_subnet_v6!("::/64"),
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(())); "del_v6_unspecified_route_non_zero_prefix_no_ack")]
    #[test_case(
        build_valid_route_test_case::<Ipv4>(
            RouteRequestKind::Del,
            NLM_F_ACK,
            TEST_V4_SUBNET,
            None,
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(())); "del_v4_only_dest_nla_ack")]
    #[test_case(
        build_valid_route_test_case::<Ipv6>(
            RouteRequestKind::Del,
            NLM_F_ACK,
            TEST_V6_SUBNET,
            None,
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(())); "del_v6_only_dest_nla_ack")]
    #[test_case(
        build_valid_route_test_case::<Ipv4>(
            RouteRequestKind::Del,
            0,
            TEST_V4_SUBNET,
            None,
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(())); "del_v4_only_dest_nla_no_ack")]
    #[test_case(
        build_valid_route_test_case::<Ipv6>(
            RouteRequestKind::Del,
            0,
            TEST_V6_SUBNET,
            None,
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(())); "del_v6_only_dest_nla_no_ack")]
    #[test_case(
        build_valid_route_test_case_with_extra_nlas::<Ipv4>(
            RouteRequestKind::Del,
            NLM_F_ACK,
            TEST_V4_SUBNET,
            Some(test_nexthop_spec_addr::<Ipv4>()),
            [RouteNla::Gateway(bytes_from_spec_addr::<Ipv4>(
                &test_nexthop_spec_addr::<Ipv4>()
            ))],
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(())); "del_v4_with_nexthop_ok_ack")]
    #[test_case(
        build_valid_route_test_case_with_extra_nlas::<Ipv6>(
            RouteRequestKind::Del,
            0,
            TEST_V6_SUBNET,
            Some(test_nexthop_spec_addr::<Ipv6>()),
            [RouteNla::Gateway(bytes_from_spec_addr::<Ipv6>(
                &test_nexthop_spec_addr::<Ipv6>()
            ))],
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(())); "del_v6_with_nexthop_ok_no_ack")]
    #[test_case(
        build_invalid_route_test_case_with_extra_nlas::<Ipv4>(
            RouteRequestKind::Del,
            NLM_F_ACK,
            TEST_V4_SUBNET,
            Some(test_nexthop_spec_addr::<Ipv4>()),
            [RouteNla::Gateway(Vec::new())],
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Errno::EINVAL); "del_v4_with_invalid_nexthop_ack")]
    #[test_case(
        build_invalid_route_test_case_with_extra_nlas::<Ipv6>(
            RouteRequestKind::Del,
            0,
            TEST_V6_SUBNET,
            Some(test_nexthop_spec_addr::<Ipv6>()),
            [RouteNla::Gateway(Vec::new())],
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Errno::EINVAL); "del_v6_with_invalid_nexthop_no_ack")]
    #[test_case(
        build_valid_route_test_case_with_extra_nlas::<Ipv4>(
            RouteRequestKind::Del,
            NLM_F_ACK,
            TEST_V4_SUBNET,
            None,
            [RouteNla::Gateway(net_ip_v4!("0.0.0.0").ipv4_bytes().to_vec())],
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(())); "del_v4_unspecified_nexthop_ok_ack")]
    #[test_case(
        build_valid_route_test_case_with_extra_nlas::<Ipv6>(
            RouteRequestKind::Del,
            0,
            TEST_V6_SUBNET,
            None,
            [RouteNla::Gateway(net_ip_v6!("::").ipv6_bytes().to_vec())],
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(())); "del_v6_unspecified_nexthop_no_ack")]
    #[test_case(
        build_valid_route_test_case_with_extra_nlas::<Ipv4>(
            RouteRequestKind::Del,
            NLM_F_ACK,
            TEST_V4_SUBNET,
            None,
            [RouteNla::Priority(100)],
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(())); "del_v4_priority_nla_ok_ack")]
    #[test_case(
        build_valid_route_test_case_with_extra_nlas::<Ipv6>(
            RouteRequestKind::Del,
            0,
            TEST_V6_SUBNET,
            None,
            [RouteNla::Priority(100)],
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(())); "del_v6_priority_nla_ok_no_ack")]
    #[test_case(
        TestRouteCase::<Ipv4> {
            expected_response: Some(ExpectedResponse::Error(Errno::EINVAL)),
            ..build_valid_route_test_case(
                RouteRequestKind::Del,
                NLM_F_ACK,
                TEST_V4_SUBNET,
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Err(routes::RequestError::InvalidRequest),
            )
        }; "del_v4_invalid_request_response_ack")]
    #[test_case(
        TestRouteCase::<Ipv6> {
            expected_response: Some(ExpectedResponse::Error(Errno::EINVAL)),
            ..build_valid_route_test_case(
                RouteRequestKind::Del,
                NLM_F_ACK,
                TEST_V6_SUBNET,
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Err(routes::RequestError::InvalidRequest),
            )
        }; "del_v6_invalid_request_response_no_ack")]
    #[test_case(
        TestRouteCase::<Ipv6> {
            expected_response: Some(ExpectedResponse::Error(Errno::ENODEV)),
            ..build_valid_route_test_case(
                RouteRequestKind::Del,
                0,
                TEST_V6_SUBNET,
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Err(routes::RequestError::UnrecognizedInterface),
            )
        }; "del_v6_unrecognized_interface_response_no_ack")]
    #[test_case(
        TestRouteCase::<Ipv6> {
            expected_response: Some(ExpectedResponse::Error(Errno::ENOTSUP)),
            ..build_valid_route_test_case(
                RouteRequestKind::Del,
                0,
                TEST_V6_SUBNET,
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Err(routes::RequestError::Unknown),
            )
        }; "del_v6_unknown_response_no_ack")]
    #[test_case(
        build_valid_route_test_case::<Ipv4>(
            RouteRequestKind::Del,
            NLM_F_ACK,
            TEST_V4_SUBNET,
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(()),
        ); "del_v4_dest_oif_nlas_ack")]
    #[test_case(
        build_valid_route_test_case::<Ipv6>(
            RouteRequestKind::Del,
            NLM_F_ACK,
            TEST_V6_SUBNET,
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(()),
        ); "del_v6_dest_oif_nlas_ack")]
    #[test_case(
        build_valid_route_test_case::<Ipv4>(
            RouteRequestKind::Del,
            0,
            TEST_V4_SUBNET,
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(()),
        ); "del_v4_dest_oif_nlas_no_ack")]
    #[test_case(
        build_valid_route_test_case::<Ipv6>(
            RouteRequestKind::Del,
            0,
            TEST_V6_SUBNET,
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(()),
        ); "del_v6_dest_oif_nlas_no_ack")]
    #[test_case(
        TestRouteCase::<Ipv4> {
            nlas: Vec::new(),
            destination_prefix_len: 1,
            ..build_invalid_route_test_case(
                RouteRequestKind::Del,
                NLM_F_ACK,
                TEST_V4_SUBNET,
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Errno::EINVAL,
            )
        }; "del_v4_missing_destination_nla_nonzero_prefix_ack")]
    #[test_case(
        TestRouteCase::<Ipv6> {
            nlas: Vec::new(),
            destination_prefix_len: 1,
            ..build_invalid_route_test_case(
                RouteRequestKind::Del,
                0,
                TEST_V6_SUBNET,
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Errno::EINVAL,
            )
        }; "del_v6_missing_destination_nla_nonzero_prefix_no_ack")]
    #[test_case(
        TestRouteCase::<Ipv4> {
            nlas: Vec::new(),
            destination_prefix_len: 0,
            ..build_valid_route_test_case(
                RouteRequestKind::Del,
                NLM_F_ACK,
                net_subnet_v4!("0.0.0.0/0"),
                None,
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Ok(()),
            )
        }; "del_v4_no_nlas_zero_prefix_len_ack")]
    #[test_case(
        TestRouteCase::<Ipv6> {
            nlas: Vec::new(),
            destination_prefix_len: 0,
            ..build_valid_route_test_case(
                RouteRequestKind::Del,
                0,
                net_subnet_v6!("::/0"),
                None,
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Ok(()),
            )
        }; "del_v6_no_nlas_zero_prefix_len_no_ack")]
    #[test_case(
        build_valid_route_test_case::<Ipv4>(
            RouteRequestKind::Del,
            NLM_F_ACK,
            net_subnet_v4!("0.0.0.0/0"),
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(()),
        ); "del_v4_missing_destination_nla_zero_prefix_ack")]
    #[test_case(
        build_valid_route_test_case::<Ipv6>(
            RouteRequestKind::Del,
            0,
            net_subnet_v6!("::/0"),
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(()),
        ); "del_v6_missing_destination_nla_zero_prefix_no_ack")]
    #[test_case(
        TestRouteCase::<Ipv4> {
            nlas: vec![RouteNla::Destination(Vec::new())],
            ..build_invalid_route_test_case(
                RouteRequestKind::Del,
                NLM_F_ACK,
                TEST_V4_SUBNET,
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Errno::EINVAL,
            )
        }; "del_v4_invalid_destination_nla_ack")]
    #[test_case(
        TestRouteCase::<Ipv6> {
            nlas: vec![RouteNla::Destination(Vec::new())],
            ..build_invalid_route_test_case(
                RouteRequestKind::Del,
                0,
                TEST_V6_SUBNET,
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Errno::EINVAL,
            )
        }; "del_v6_invalid_destination_nla_no_ack")]
    #[test_case(
        build_valid_route_test_case_with_extra_nlas::<Ipv4>(
            RouteRequestKind::Del,
            NLM_F_ACK,
            TEST_V4_SUBNET,
            None,
            // `RT_TABLE_COMPAT` is generally used when `table` is outside the bounds of u8 values.
            [RouteNla::Table(u8::MAX as u32 + 1)],
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_COMPAT,
            RTN_UNICAST,
            Ok(())); "del_v4_with_table_nla_rt_table_compat_ack")]
    #[test_case(
        build_valid_route_test_case_with_extra_nlas::<Ipv6>(
            RouteRequestKind::Del,
            0,
            TEST_V6_SUBNET,
            None,
            // `RT_TABLE_COMPAT` is generally used when `table` is outside the bounds of u8 values.
            [RouteNla::Table(u8::MAX as u32 + 1)],
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_COMPAT,
            RTN_UNICAST,
            Ok(())); "del_v6_with_table_nla_rt_table_compat_no_ack")]
    #[test_case(
        build_valid_route_test_case_with_extra_nlas::<Ipv4>(
            RouteRequestKind::Del,
            NLM_F_ACK,
            TEST_V4_SUBNET,
            None,
            [RouteNla::Table(u8::MAX as u32 + 1)],
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(())); "del_v4_with_table_nla_rt_table_main_ack")]
    #[test_case(
        build_valid_route_test_case_with_extra_nlas::<Ipv6>(
            RouteRequestKind::Del,
            0,
            TEST_V6_SUBNET,
            None,
            [RouteNla::Table(u8::MAX as u32 + 1)],
            Some(interfaces::testutil::LO_INTERFACE_ID),
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(())); "del_v6_with_table_nla_rt_table_main_no_ack")]
    #[test_case(
        TestRouteCase::<Ipv4> {
            destination_prefix_len: u8::MAX,
            ..build_invalid_route_test_case(
                RouteRequestKind::Del,
                NLM_F_ACK,
                TEST_V4_SUBNET,
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Errno::EINVAL,
            )
        }; "del_v4_invalid_prefix_len_ack")]
    #[test_case(
        TestRouteCase::<Ipv6> {
            destination_prefix_len: u8::MAX,
            ..build_invalid_route_test_case(
                RouteRequestKind::Del,
                0,
                TEST_V6_SUBNET,
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Errno::EINVAL,
            )
        }; "del_v6_invalid_prefix_len_no_ack")]
    #[test_case(
        TestRouteCase::<Ipv4> {
            destination_prefix_len: 0,
            ..build_invalid_route_test_case(
                RouteRequestKind::Del,
                NLM_F_ACK,
                TEST_V4_SUBNET,
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Errno::EINVAL,
            )
        }; "del_v4_zero_prefix_len_ack")]
    #[test_case(
        TestRouteCase::<Ipv6> {
            destination_prefix_len: 0,
            ..build_invalid_route_test_case(
                RouteRequestKind::Del,
                0,
                TEST_V6_SUBNET,
                Some(interfaces::testutil::LO_INTERFACE_ID),
                RT_TABLE_MAIN,
                RTN_UNICAST,
                Errno::EINVAL,
            )
        }; "del_v6_zero_prefix_len_no_ack")]
    #[test_case(
        build_valid_route_test_case_with_extra_nlas::<Ipv4>(
            RouteRequestKind::Del,
            NLM_F_ACK,
            TEST_V4_SUBNET,
            None,
            [RouteNla::Oif(0)],
            None,
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(()),
        ); "del_v4_zero_interface_id_ack")]
    #[test_case(
        build_valid_route_test_case_with_extra_nlas::<Ipv6>(
            RouteRequestKind::Del,
            NLM_F_ACK,
            TEST_V6_SUBNET,
            None,
            [RouteNla::Oif(0)],
            None,
            RT_TABLE_MAIN,
            RTN_UNICAST,
            Ok(()),
        ); "del_v6_zero_interface_id_no_ack")]
    #[fuchsia::test]
    async fn test_new_del_route<I: Ip>(test_case: TestRouteCase<I>) {
        let TestRouteCase {
            kind,
            flags,
            family,
            nlas,
            destination_prefix_len,
            table,
            rtm_type,
            expected_request_args,
            expected_response,
        }: TestRouteCase<I> = test_case;

        let header = header_with_flags(flags);
        let route_message = {
            let mut message = RouteMessage::default();
            message.header.address_family = family.try_into().unwrap();
            message.header.destination_prefix_length = destination_prefix_len;
            message.header.table = table;
            message.header.kind = rtm_type;
            message.nlas = nlas;
            message
        };

        let (message, request) = match kind {
            RouteRequestKind::New => (RtnlMessage::NewRoute(route_message), expected_request_args),
            RouteRequestKind::Del => (RtnlMessage::DelRoute(route_message), expected_request_args),
        };

        pretty_assertions::assert_eq!(
            test_route_request(
                NetlinkMessage::new(header, NetlinkPayload::InnerMessage(message)),
                request,
            )
            .await,
            expected_response
                .into_iter()
                .map(|response| SentMessage::unicast(match response {
                    ExpectedResponse::Ack => netlink_packet::new_error(Ok(()), header),
                    ExpectedResponse::Done => netlink_packet::new_done(header),
                    ExpectedResponse::Error(e) => netlink_packet::new_error(Err(e), header),
                }))
                .collect::<Vec<_>>(),
        )
    }
}
