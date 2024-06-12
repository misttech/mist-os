// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Extension crate for the `fuchsia.net.multicast.admin` FIDL API.
//!
//! This crate provides types and traits to abstract the separate IPv4 and IPv6
//! FIDL APIs onto a single API surface that is generic over IP version.

use fidl_fuchsia_net_ext::IntoExt as _;
use fidl_fuchsia_net_multicast_admin as fnet_multicast_admin;
use net_types::ip::{Ip, Ipv4, Ipv6};

/// A type capable of responding to FIDL requests.
pub trait FidlResponder<R> {
    /// Try to send the given response over this responder.
    fn try_send(self, response: R) -> Result<(), fidl::Error>;
}

/// An IP extension providing functionality for `fuchsia_net_multicast_admin`.
pub trait FidlMulticastAdminIpExt: Ip {
    /// Protocol Marker for the multicast routing table controller.
    type TableControllerMarker: fidl::endpoints::DiscoverableProtocolMarker;
    /// Request Stream for the multicast routing table controller.
    type TableControllerRequestStream: fidl::endpoints::RequestStream<
        Ok: Send + Into<TableControllerRequest<Self>>,
        ControlHandle: Send,
    >;
    /// The Unicast Source and Multicast Destination address tuple.
    type Addresses: Into<UnicastSourceAndMulticastDestination<Self>>;
    /// A [`FidlResponder`] for multicast routing table AddRoute requests.
    type AddRouteResponder: FidlResponder<Result<(), AddRouteError>>;
    /// A [`FidlResponder`] for multicast routing table DelRoute requests.
    type DelRouteResponder: FidlResponder<Result<(), DelRouteError>>;
    /// A [`FidlResponder`] for multicast routing table GetRouteStats requests.
    type GetRouteStatsResponder: for<'a> FidlResponder<
        Result<&'a fnet_multicast_admin::RouteStats, GetRouteStatsError>,
    >;
    /// A [`FidlResponder`] for multicast routing table WatchRoutingEvents
    /// requests.
    type WatchRoutingEventsResponder: for<'a> FidlResponder<WatchRoutingEventsResponse<'a, Self>>;
}

impl FidlMulticastAdminIpExt for Ipv4 {
    type TableControllerMarker = fnet_multicast_admin::Ipv4RoutingTableControllerMarker;
    type TableControllerRequestStream =
        fnet_multicast_admin::Ipv4RoutingTableControllerRequestStream;
    type Addresses = fnet_multicast_admin::Ipv4UnicastSourceAndMulticastDestination;
    type AddRouteResponder = fnet_multicast_admin::Ipv4RoutingTableControllerAddRouteResponder;
    type DelRouteResponder = fnet_multicast_admin::Ipv4RoutingTableControllerDelRouteResponder;
    type GetRouteStatsResponder =
        fnet_multicast_admin::Ipv4RoutingTableControllerGetRouteStatsResponder;
    type WatchRoutingEventsResponder =
        fnet_multicast_admin::Ipv4RoutingTableControllerWatchRoutingEventsResponder;
}

impl FidlMulticastAdminIpExt for Ipv6 {
    type TableControllerMarker = fnet_multicast_admin::Ipv6RoutingTableControllerMarker;
    type TableControllerRequestStream =
        fnet_multicast_admin::Ipv6RoutingTableControllerRequestStream;
    type Addresses = fnet_multicast_admin::Ipv6UnicastSourceAndMulticastDestination;
    type AddRouteResponder = fnet_multicast_admin::Ipv6RoutingTableControllerAddRouteResponder;
    type DelRouteResponder = fnet_multicast_admin::Ipv6RoutingTableControllerDelRouteResponder;
    type GetRouteStatsResponder =
        fnet_multicast_admin::Ipv6RoutingTableControllerGetRouteStatsResponder;
    type WatchRoutingEventsResponder =
        fnet_multicast_admin::Ipv6RoutingTableControllerWatchRoutingEventsResponder;
}

/// An IP generic version of
/// [`fnet_multicast_admin::Ipv4RoutingTableControllerRequest`] and
/// [`fnet_multicast_admin::Ipv6RoutingTableControllerRequest`].
#[derive(Debug)]
pub enum TableControllerRequest<I: FidlMulticastAdminIpExt> {
    AddRoute {
        addresses: UnicastSourceAndMulticastDestination<I>,
        route: fnet_multicast_admin::Route,
        responder: I::AddRouteResponder,
    },
    DelRoute {
        addresses: UnicastSourceAndMulticastDestination<I>,
        responder: I::DelRouteResponder,
    },
    GetRouteStats {
        addresses: UnicastSourceAndMulticastDestination<I>,
        responder: I::GetRouteStatsResponder,
    },
    WatchRoutingEvents {
        responder: I::WatchRoutingEventsResponder,
    },
}

impl From<fnet_multicast_admin::Ipv4RoutingTableControllerRequest>
    for TableControllerRequest<Ipv4>
{
    fn from(request: fnet_multicast_admin::Ipv4RoutingTableControllerRequest) -> Self {
        use fnet_multicast_admin::Ipv4RoutingTableControllerRequest::*;
        match request {
            AddRoute { addresses, route, responder } => {
                TableControllerRequest::AddRoute { addresses: addresses.into(), route, responder }
            }
            DelRoute { addresses, responder } => {
                TableControllerRequest::DelRoute { addresses: addresses.into(), responder }
            }
            GetRouteStats { addresses, responder } => {
                TableControllerRequest::GetRouteStats { addresses: addresses.into(), responder }
            }
            WatchRoutingEvents { responder } => {
                TableControllerRequest::WatchRoutingEvents { responder }
            }
        }
    }
}

impl From<fnet_multicast_admin::Ipv6RoutingTableControllerRequest>
    for TableControllerRequest<Ipv6>
{
    fn from(request: fnet_multicast_admin::Ipv6RoutingTableControllerRequest) -> Self {
        use fnet_multicast_admin::Ipv6RoutingTableControllerRequest::*;
        match request {
            AddRoute { addresses, route, responder } => {
                TableControllerRequest::AddRoute { addresses: addresses.into(), route, responder }
            }
            DelRoute { addresses, responder } => {
                TableControllerRequest::DelRoute { addresses: addresses.into(), responder }
            }
            GetRouteStats { addresses, responder } => {
                TableControllerRequest::GetRouteStats { addresses: addresses.into(), responder }
            }
            WatchRoutingEvents { responder } => {
                TableControllerRequest::WatchRoutingEvents { responder }
            }
        }
    }
}

/// An IP generic version of
/// [`fnet_multicast_admin::Ipv4UnicastSourceAndMulticastDestination`] and
/// [`fnet_multicast_admin::Ipv6UnicastSourceAndMulticastDestination`].
#[derive(Debug, Clone, PartialEq)]
pub struct UnicastSourceAndMulticastDestination<I: Ip> {
    pub unicast_source: I::Addr,
    pub multicast_destination: I::Addr,
}

impl From<fnet_multicast_admin::Ipv4UnicastSourceAndMulticastDestination>
    for UnicastSourceAndMulticastDestination<Ipv4>
{
    fn from(addresses: fnet_multicast_admin::Ipv4UnicastSourceAndMulticastDestination) -> Self {
        let fnet_multicast_admin::Ipv4UnicastSourceAndMulticastDestination {
            unicast_source,
            multicast_destination,
        } = addresses;
        UnicastSourceAndMulticastDestination {
            unicast_source: unicast_source.into_ext(),
            multicast_destination: multicast_destination.into_ext(),
        }
    }
}

impl From<fnet_multicast_admin::Ipv6UnicastSourceAndMulticastDestination>
    for UnicastSourceAndMulticastDestination<Ipv6>
{
    fn from(addresses: fnet_multicast_admin::Ipv6UnicastSourceAndMulticastDestination) -> Self {
        let fnet_multicast_admin::Ipv6UnicastSourceAndMulticastDestination {
            unicast_source,
            multicast_destination,
        } = addresses;
        UnicastSourceAndMulticastDestination {
            unicast_source: unicast_source.into_ext(),
            multicast_destination: multicast_destination.into_ext(),
        }
    }
}

impl From<UnicastSourceAndMulticastDestination<Ipv4>>
    for fnet_multicast_admin::Ipv4UnicastSourceAndMulticastDestination
{
    fn from(addresses: UnicastSourceAndMulticastDestination<Ipv4>) -> Self {
        let UnicastSourceAndMulticastDestination { unicast_source, multicast_destination } =
            addresses;
        fnet_multicast_admin::Ipv4UnicastSourceAndMulticastDestination {
            unicast_source: unicast_source.into_ext(),
            multicast_destination: multicast_destination.into_ext(),
        }
    }
}

impl From<UnicastSourceAndMulticastDestination<Ipv6>>
    for fnet_multicast_admin::Ipv6UnicastSourceAndMulticastDestination
{
    fn from(addresses: UnicastSourceAndMulticastDestination<Ipv6>) -> Self {
        let UnicastSourceAndMulticastDestination { unicast_source, multicast_destination } =
            addresses;
        fnet_multicast_admin::Ipv6UnicastSourceAndMulticastDestination {
            unicast_source: unicast_source.into_ext(),
            multicast_destination: multicast_destination.into_ext(),
        }
    }
}

/// An IP generic version of
/// [`fnet_multicast_admin::Ipv4RoutingTableControllerAddRouteError`] and
/// [`fnet_multicast_admin::Ipv6RoutingTableControllerAddRouteError`].
#[derive(Debug, Clone, PartialEq)]
pub enum AddRouteError {
    InvalidAddress,
    RequiredRouteFieldsMissing,
    InterfaceNotFound,
    InputCannotBeOutput,
}

impl From<AddRouteError> for fnet_multicast_admin::Ipv4RoutingTableControllerAddRouteError {
    fn from(error: AddRouteError) -> Self {
        use fnet_multicast_admin::Ipv4RoutingTableControllerAddRouteError::*;
        match error {
            AddRouteError::InvalidAddress => InvalidAddress,
            AddRouteError::RequiredRouteFieldsMissing => RequiredRouteFieldsMissing,
            AddRouteError::InterfaceNotFound => InterfaceNotFound,
            AddRouteError::InputCannotBeOutput => InputCannotBeOutput,
        }
    }
}

impl From<AddRouteError> for fnet_multicast_admin::Ipv6RoutingTableControllerAddRouteError {
    fn from(error: AddRouteError) -> Self {
        use fnet_multicast_admin::Ipv6RoutingTableControllerAddRouteError::*;
        match error {
            AddRouteError::InvalidAddress => InvalidAddress,
            AddRouteError::RequiredRouteFieldsMissing => RequiredRouteFieldsMissing,
            AddRouteError::InterfaceNotFound => InterfaceNotFound,
            AddRouteError::InputCannotBeOutput => InputCannotBeOutput,
        }
    }
}

impl FidlResponder<Result<(), AddRouteError>>
    for fnet_multicast_admin::Ipv4RoutingTableControllerAddRouteResponder
{
    fn try_send(self, response: Result<(), AddRouteError>) -> Result<(), fidl::Error> {
        self.send(response.map_err(Into::into))
    }
}

impl FidlResponder<Result<(), AddRouteError>>
    for fnet_multicast_admin::Ipv6RoutingTableControllerAddRouteResponder
{
    fn try_send(self, response: Result<(), AddRouteError>) -> Result<(), fidl::Error> {
        self.send(response.map_err(Into::into))
    }
}

/// An IP generic version of
/// [`fnet_multicast_admin::Ipv4RoutingTableControllerDelRouteError`] and
/// [`fnet_multicast_admin::Ipv6RoutingTableControllerDelRouteError`].
#[derive(Debug, Clone, PartialEq)]
pub enum DelRouteError {
    InvalidAddress,
    NotFound,
}

impl From<DelRouteError> for fnet_multicast_admin::Ipv4RoutingTableControllerDelRouteError {
    fn from(error: DelRouteError) -> Self {
        use fnet_multicast_admin::Ipv4RoutingTableControllerDelRouteError::*;
        match error {
            DelRouteError::InvalidAddress => InvalidAddress,
            DelRouteError::NotFound => NotFound,
        }
    }
}

impl From<DelRouteError> for fnet_multicast_admin::Ipv6RoutingTableControllerDelRouteError {
    fn from(error: DelRouteError) -> Self {
        use fnet_multicast_admin::Ipv6RoutingTableControllerDelRouteError::*;
        match error {
            DelRouteError::InvalidAddress => InvalidAddress,
            DelRouteError::NotFound => NotFound,
        }
    }
}

impl FidlResponder<Result<(), DelRouteError>>
    for fnet_multicast_admin::Ipv4RoutingTableControllerDelRouteResponder
{
    fn try_send(self, response: Result<(), DelRouteError>) -> Result<(), fidl::Error> {
        self.send(response.map_err(Into::into))
    }
}

impl FidlResponder<Result<(), DelRouteError>>
    for fnet_multicast_admin::Ipv6RoutingTableControllerDelRouteResponder
{
    fn try_send(self, response: Result<(), DelRouteError>) -> Result<(), fidl::Error> {
        self.send(response.map_err(Into::into))
    }
}

/// An IP generic version of
/// [`fnet_multicast_admin::Ipv4RoutingTableControllerGetRouteStatsError`] and
/// [`fnet_multicast_admin::Ipv6RoutingTableControllerGetRouteStatsError`].
#[derive(Debug, Clone, PartialEq)]
pub enum GetRouteStatsError {
    InvalidAddress,
    NotFound,
}

impl From<GetRouteStatsError>
    for fnet_multicast_admin::Ipv4RoutingTableControllerGetRouteStatsError
{
    fn from(error: GetRouteStatsError) -> Self {
        use fnet_multicast_admin::Ipv4RoutingTableControllerGetRouteStatsError::*;
        match error {
            GetRouteStatsError::InvalidAddress => InvalidAddress,
            GetRouteStatsError::NotFound => NotFound,
        }
    }
}

impl From<GetRouteStatsError>
    for fnet_multicast_admin::Ipv6RoutingTableControllerGetRouteStatsError
{
    fn from(error: GetRouteStatsError) -> Self {
        use fnet_multicast_admin::Ipv6RoutingTableControllerGetRouteStatsError::*;
        match error {
            GetRouteStatsError::InvalidAddress => InvalidAddress,
            GetRouteStatsError::NotFound => NotFound,
        }
    }
}

impl FidlResponder<Result<&fnet_multicast_admin::RouteStats, GetRouteStatsError>>
    for fnet_multicast_admin::Ipv4RoutingTableControllerGetRouteStatsResponder
{
    fn try_send(
        self,
        response: Result<&fnet_multicast_admin::RouteStats, GetRouteStatsError>,
    ) -> Result<(), fidl::Error> {
        self.send(response.map_err(Into::into))
    }
}

impl FidlResponder<Result<&fnet_multicast_admin::RouteStats, GetRouteStatsError>>
    for fnet_multicast_admin::Ipv6RoutingTableControllerGetRouteStatsResponder
{
    fn try_send(
        self,
        response: Result<&fnet_multicast_admin::RouteStats, GetRouteStatsError>,
    ) -> Result<(), fidl::Error> {
        self.send(response.map_err(Into::into))
    }
}

/// An IP generic version of the fields accepted by an
/// [`fnet_multicast_admin::Ipv4RoutingTableControllerWatchRoutingEventsResponder`] and
/// [`fnet_multicast_admin::Ipv6RoutingTableControllerWatchRoutingEventsResponder`].
#[derive(Debug)]
pub struct WatchRoutingEventsResponse<'a, I: FidlMulticastAdminIpExt> {
    dropped_events: u64,
    addresses: &'a UnicastSourceAndMulticastDestination<I>,
    input_interface: u64,
    event: &'a fnet_multicast_admin::RoutingEvent,
}

impl<'a> FidlResponder<WatchRoutingEventsResponse<'a, Ipv4>>
    for fnet_multicast_admin::Ipv4RoutingTableControllerWatchRoutingEventsResponder
{
    fn try_send(self, response: WatchRoutingEventsResponse<'a, Ipv4>) -> Result<(), fidl::Error> {
        let WatchRoutingEventsResponse { dropped_events, addresses, input_interface, event } =
            response;
        let addresses = addresses.clone().into();
        self.send(dropped_events, &addresses, input_interface, event)
    }
}

impl<'a> FidlResponder<WatchRoutingEventsResponse<'a, Ipv6>>
    for fnet_multicast_admin::Ipv6RoutingTableControllerWatchRoutingEventsResponder
{
    fn try_send(self, response: WatchRoutingEventsResponse<'a, Ipv6>) -> Result<(), fidl::Error> {
        let WatchRoutingEventsResponse { dropped_events, addresses, input_interface, event } =
            response;
        let addresses = addresses.clone().into();
        self.send(dropped_events, &addresses, input_interface, event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use net_declare::{fidl_ip_v4, fidl_ip_v6, net_ip_v4, net_ip_v6};
    use test_case::test_case;

    #[test]
    fn ipv4_unicast_source_and_multicast_destination() {
        let fidl_addrs = fnet_multicast_admin::Ipv4UnicastSourceAndMulticastDestination {
            unicast_source: fidl_ip_v4!("192.0.2.1"),
            multicast_destination: fidl_ip_v4!("224.0.0.1"),
        };
        let addrs = UnicastSourceAndMulticastDestination::<Ipv4> {
            unicast_source: net_ip_v4!("192.0.2.1"),
            multicast_destination: net_ip_v4!("224.0.0.1"),
        };
        assert_eq!(
            fnet_multicast_admin::Ipv4UnicastSourceAndMulticastDestination::from(addrs.clone()),
            fidl_addrs
        );
        assert_eq!(UnicastSourceAndMulticastDestination::<Ipv4>::from(fidl_addrs), addrs);
    }

    #[test]
    fn ipv6_unicast_source_and_multicast_destination() {
        let fidl_addrs = fnet_multicast_admin::Ipv6UnicastSourceAndMulticastDestination {
            unicast_source: fidl_ip_v6!("2001:db8::1"),
            multicast_destination: fidl_ip_v6!("ff00::1"),
        };
        let addrs = UnicastSourceAndMulticastDestination::<Ipv6> {
            unicast_source: net_ip_v6!("2001:db8::1"),
            multicast_destination: net_ip_v6!("ff00::1"),
        };
        assert_eq!(
            fnet_multicast_admin::Ipv6UnicastSourceAndMulticastDestination::from(addrs.clone()),
            fidl_addrs
        );
        assert_eq!(UnicastSourceAndMulticastDestination::<Ipv6>::from(fidl_addrs), addrs);
    }

    #[test_case(
        AddRouteError::InvalidAddress =>
        fnet_multicast_admin::Ipv4RoutingTableControllerAddRouteError::InvalidAddress;
        "invalid_address"
    )]
    #[test_case(
        AddRouteError::RequiredRouteFieldsMissing =>
        fnet_multicast_admin::Ipv4RoutingTableControllerAddRouteError::RequiredRouteFieldsMissing;
        "required_route_fields_missing"
    )]
    #[test_case(
        AddRouteError::InterfaceNotFound =>
        fnet_multicast_admin::Ipv4RoutingTableControllerAddRouteError::InterfaceNotFound;
        "interface_not_found"
    )]
    #[test_case(
        AddRouteError::InputCannotBeOutput =>
        fnet_multicast_admin::Ipv4RoutingTableControllerAddRouteError::InputCannotBeOutput;
        "input_cannot_be_output"
    )]
    fn ipv4_add_route_error(
        err: AddRouteError,
    ) -> fnet_multicast_admin::Ipv4RoutingTableControllerAddRouteError {
        fnet_multicast_admin::Ipv4RoutingTableControllerAddRouteError::from(err)
    }

    #[test_case(
        AddRouteError::InvalidAddress =>
        fnet_multicast_admin::Ipv6RoutingTableControllerAddRouteError::InvalidAddress;
        "invalid_address"
    )]
    #[test_case(
        AddRouteError::RequiredRouteFieldsMissing =>
        fnet_multicast_admin::Ipv6RoutingTableControllerAddRouteError::RequiredRouteFieldsMissing;
        "required_route_fields_missing"
    )]
    #[test_case(
        AddRouteError::InterfaceNotFound =>
        fnet_multicast_admin::Ipv6RoutingTableControllerAddRouteError::InterfaceNotFound;
        "interface_not_found"
    )]
    #[test_case(
        AddRouteError::InputCannotBeOutput =>
        fnet_multicast_admin::Ipv6RoutingTableControllerAddRouteError::InputCannotBeOutput;
        "input_cannot_be_output"
    )]
    fn ipv6_add_route_error(
        err: AddRouteError,
    ) -> fnet_multicast_admin::Ipv6RoutingTableControllerAddRouteError {
        fnet_multicast_admin::Ipv6RoutingTableControllerAddRouteError::from(err)
    }

    #[test_case(
        DelRouteError::InvalidAddress =>
        fnet_multicast_admin::Ipv4RoutingTableControllerDelRouteError::InvalidAddress;
        "invalid_address"
    )]
    #[test_case(
        DelRouteError::NotFound =>
        fnet_multicast_admin::Ipv4RoutingTableControllerDelRouteError::NotFound;
        "not_found"
    )]
    fn ipv4_del_route_error(
        err: DelRouteError,
    ) -> fnet_multicast_admin::Ipv4RoutingTableControllerDelRouteError {
        fnet_multicast_admin::Ipv4RoutingTableControllerDelRouteError::from(err)
    }

    #[test_case(
        DelRouteError::InvalidAddress =>
        fnet_multicast_admin::Ipv6RoutingTableControllerDelRouteError::InvalidAddress;
        "invalid_address"
    )]
    #[test_case(
        DelRouteError::NotFound =>
        fnet_multicast_admin::Ipv6RoutingTableControllerDelRouteError::NotFound;
        "not_found"
    )]
    fn ipv6_del_route_error(
        err: DelRouteError,
    ) -> fnet_multicast_admin::Ipv6RoutingTableControllerDelRouteError {
        fnet_multicast_admin::Ipv6RoutingTableControllerDelRouteError::from(err)
    }

    #[test_case(
        GetRouteStatsError::InvalidAddress =>
        fnet_multicast_admin::Ipv4RoutingTableControllerGetRouteStatsError::InvalidAddress;
        "invalid_address"
    )]
    #[test_case(
        GetRouteStatsError::NotFound =>
        fnet_multicast_admin::Ipv4RoutingTableControllerGetRouteStatsError::NotFound;
        "not_found"
    )]
    fn ipv4_get_route_stats_error(
        err: GetRouteStatsError,
    ) -> fnet_multicast_admin::Ipv4RoutingTableControllerGetRouteStatsError {
        fnet_multicast_admin::Ipv4RoutingTableControllerGetRouteStatsError::from(err)
    }

    #[test_case(
        GetRouteStatsError::InvalidAddress =>
        fnet_multicast_admin::Ipv6RoutingTableControllerGetRouteStatsError::InvalidAddress;
        "invalid_address"
    )]
    #[test_case(
        GetRouteStatsError::NotFound =>
        fnet_multicast_admin::Ipv6RoutingTableControllerGetRouteStatsError::NotFound;
        "not_found"
    )]
    fn ipv6_get_route_stats_error(
        err: GetRouteStatsError,
    ) -> fnet_multicast_admin::Ipv6RoutingTableControllerGetRouteStatsError {
        fnet_multicast_admin::Ipv6RoutingTableControllerGetRouteStatsError::from(err)
    }
}
