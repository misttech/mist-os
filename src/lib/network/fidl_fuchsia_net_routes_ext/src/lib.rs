// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Extensions for the fuchsia.net.routes FIDL library.
//!
//! The fuchsia.net.routes API has separate V4 and V6 watcher variants to
//! enforce maximum type safety and access control at the API layer. For the
//! most part, these APIs are a mirror image of one another. This library
//! provides an a single implementation that is generic over
//! [`net_types::ip::Ip`] version, as well as conversion utilities.

#![deny(missing_docs)]

#[cfg(test)]
mod testutil;

use async_utils::fold;
use fidl_fuchsia_net_ext::{IntoExt as _, TryIntoExt as _};
use fidl_fuchsia_net_routes as fnet_routes;
use futures::{Future, Stream, StreamExt as _, TryStreamExt as _};
use net_types::{
    ip::{GenericOverIp, Ip, IpInvariant, Ipv4, Ipv6, Ipv6Addr, Subnet},
    SpecifiedAddr, UnicastAddress,
};
use thiserror::Error;

/// Conversion errors from `fnet_routes` FIDL types to the generic equivalents
/// defined in this module.
#[derive(Clone, Copy, Debug, Error, PartialEq)]
pub enum FidlConversionError {
    /// A required field was unset. The provided string is the human-readable
    /// name of the unset field.
    #[error("required field is unset: {0}")]
    RequiredFieldUnset(&'static str),
    /// Destination Subnet conversion failed.
    #[error("failed to convert `destination` to net_types subnet: {0:?}")]
    DestinationSubnet(net_types::ip::SubnetError),
    /// Next-Hop specified address conversion failed.
    #[error("failed to convert `next_hop` to a specified addr")]
    UnspecifiedNextHop,
    /// Next-Hop unicast address conversion failed.
    #[error("failed to convert `next_hop` to a unicast addr")]
    NextHopNotUnicast,
}

/// The specified properties of a route. This type enforces that all required
/// fields from [`fnet_routes::SpecifiedRouteProperties`] are set.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SpecifiedRouteProperties {
    /// The specified metric of the route.
    pub metric: fnet_routes::SpecifiedMetric,
}

impl TryFrom<fnet_routes::SpecifiedRouteProperties> for SpecifiedRouteProperties {
    type Error = FidlConversionError;
    fn try_from(
        specified_properties: fnet_routes::SpecifiedRouteProperties,
    ) -> Result<Self, Self::Error> {
        Ok(SpecifiedRouteProperties {
            metric: specified_properties.metric.ok_or(FidlConversionError::RequiredFieldUnset(
                "fuchsia.net.routes/SpecifiedRouteProperties.metric",
            ))?,
        })
    }
}

/// The effective properties of a route. This type enforces that all required
/// fields from [`fnet_routes::EffectiveRouteProperties`] are set.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct EffectiveRouteProperties {
    /// The effective metric of the route.
    pub metric: u32,
}

impl TryFrom<fnet_routes::EffectiveRouteProperties> for EffectiveRouteProperties {
    type Error = FidlConversionError;
    fn try_from(
        effective_properties: fnet_routes::EffectiveRouteProperties,
    ) -> Result<Self, Self::Error> {
        Ok(EffectiveRouteProperties {
            metric: effective_properties.metric.ok_or(FidlConversionError::RequiredFieldUnset(
                "fuchsia.net.routes/EffectiveRouteProperties.metric",
            ))?,
        })
    }
}

/// The properties of a route, abstracting over
/// [`fnet_routes::RoutePropertiesV4`] and [`fnet_routes::RoutePropertiesV6`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RouteProperties {
    /// the specified properties of the route.
    pub specified_properties: SpecifiedRouteProperties,
}

impl TryFrom<fnet_routes::RoutePropertiesV4> for RouteProperties {
    type Error = FidlConversionError;
    fn try_from(properties: fnet_routes::RoutePropertiesV4) -> Result<Self, Self::Error> {
        Ok(RouteProperties {
            specified_properties: properties
                .specified_properties
                .ok_or(FidlConversionError::RequiredFieldUnset(
                    "fuchsia.net.routes/RoutePropertiesV4.specified_properties",
                ))?
                .try_into()?,
        })
    }
}

impl TryFrom<fnet_routes::RoutePropertiesV6> for RouteProperties {
    type Error = FidlConversionError;
    fn try_from(properties: fnet_routes::RoutePropertiesV6) -> Result<Self, Self::Error> {
        Ok(RouteProperties {
            specified_properties: properties
                .specified_properties
                .ok_or(FidlConversionError::RequiredFieldUnset(
                    "fuchsia.net.routes/RoutePropertiesV6.specified_properties",
                ))?
                .try_into()?,
        })
    }
}

/// A target of a route, abstracting over [`fnet_routes::RouteTargetV4`] and
/// [`fnet_routes::RouteTargetV6`].
///
/// The `next_hop` address is required to be unicast. IPv4 addresses can only be
/// determined to be unicast within the broader context of a subnet, hence they
/// are only guaranteed to be specified in this context. IPv6 addresses,
/// however, will be confirmed to be unicast.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RouteTarget<I: Ip> {
    /// The outbound_interface to use when forwarding packets.
    pub outbound_interface: u64,
    /// The next-hop IP address of the route.
    pub next_hop: Option<SpecifiedAddr<I::Addr>>,
}

impl TryFrom<fnet_routes::RouteTargetV4> for RouteTarget<Ipv4> {
    type Error = FidlConversionError;
    fn try_from(target: fnet_routes::RouteTargetV4) -> Result<Self, Self::Error> {
        let fnet_routes::RouteTargetV4 { outbound_interface, next_hop } = target;
        let next_hop = next_hop
            .map(|addr| {
                SpecifiedAddr::new((*addr).into_ext())
                    .ok_or(FidlConversionError::UnspecifiedNextHop)
            })
            .transpose()?;
        Ok(RouteTarget { outbound_interface, next_hop })
    }
}

impl TryFrom<fnet_routes::RouteTargetV6> for RouteTarget<Ipv6> {
    type Error = FidlConversionError;
    fn try_from(target: fnet_routes::RouteTargetV6) -> Result<Self, Self::Error> {
        let fnet_routes::RouteTargetV6 { outbound_interface, next_hop } = target;
        let addr: Option<SpecifiedAddr<Ipv6Addr>> = next_hop
            .map(|addr| {
                SpecifiedAddr::new((*addr).into_ext())
                    .ok_or(FidlConversionError::UnspecifiedNextHop)
            })
            .transpose()?;
        if let Some(specified_addr) = addr {
            if !specified_addr.is_unicast() {
                return Err(FidlConversionError::NextHopNotUnicast);
            }
        }
        Ok(RouteTarget { outbound_interface, next_hop: addr })
    }
}

/// The action of a route, abstracting over [`fnet_routes::RouteActionV4`] and
/// [`fnet_routes::RouteActionV6`].
///
/// These fidl types are both defined as flexible unions, which allows the
/// definition to grow over time. The `Unknown` enum variant accounts for any
/// new types that are not yet known to the local version of the FIDL bindings.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RouteAction<I: Ip> {
    /// The RouteAction is unknown.
    Unknown,
    /// Forward packets to the specified target.
    Forward(RouteTarget<I>),
}

impl TryFrom<fnet_routes::RouteActionV4> for RouteAction<Ipv4> {
    type Error = FidlConversionError;
    fn try_from(action: fnet_routes::RouteActionV4) -> Result<Self, Self::Error> {
        match action {
            fnet_routes::RouteActionV4::Forward(target) => {
                Ok(RouteAction::Forward(target.try_into()?))
            }
            fnet_routes::RouteActionV4Unknown!() => Ok(RouteAction::Unknown),
        }
    }
}

impl TryFrom<fnet_routes::RouteActionV6> for RouteAction<Ipv6> {
    type Error = FidlConversionError;
    fn try_from(action: fnet_routes::RouteActionV6) -> Result<Self, Self::Error> {
        match action {
            fnet_routes::RouteActionV6::Forward(target) => {
                Ok(RouteAction::Forward(target.try_into()?))
            }
            fnet_routes::RouteActionV4Unknown!() => Ok(RouteAction::Unknown),
        }
    }
}

/// A route, abstracting over [`fnet_routes::RouteV4`] and
/// [`fnet_routes::RouteV6`].
///
/// The `destination` subnet is verified to be a valid subnet; e.g. its
/// prefix-len is a valid value, and its host bits are cleared.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Route<I: Ip> {
    /// The destination subnet of the route.
    pub destination: Subnet<I::Addr>,
    /// The action specifying how to handle packets matching this route.
    pub action: RouteAction<I>,
    /// The additional properties of the route.
    pub properties: RouteProperties,
}

impl TryFrom<fnet_routes::RouteV4> for Route<Ipv4> {
    type Error = FidlConversionError;
    fn try_from(route: fnet_routes::RouteV4) -> Result<Self, Self::Error> {
        let fnet_routes::RouteV4 { destination, action, properties } = route;
        Ok(Route {
            destination: destination
                .try_into_ext()
                .map_err(FidlConversionError::DestinationSubnet)?,
            action: action.try_into()?,
            properties: properties.try_into()?,
        })
    }
}

impl TryFrom<fnet_routes::RouteV6> for Route<Ipv6> {
    type Error = FidlConversionError;
    fn try_from(route: fnet_routes::RouteV6) -> Result<Self, Self::Error> {
        let fnet_routes::RouteV6 { destination, action, properties } = route;
        let destination =
            destination.try_into_ext().map_err(FidlConversionError::DestinationSubnet)?;
        Ok(Route { destination, action: action.try_into()?, properties: properties.try_into()? })
    }
}

/// An installed route, abstracting over [`fnet_routes::InstalledRouteV4`] and
/// [`fnet_routes::InstalledRouteV6`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct InstalledRoute<I: Ip> {
    /// The route.
    pub route: Route<I>,
    /// The route's effective properties.
    pub effective_properties: EffectiveRouteProperties,
}

impl TryFrom<fnet_routes::InstalledRouteV4> for InstalledRoute<Ipv4> {
    type Error = FidlConversionError;
    fn try_from(installed_route: fnet_routes::InstalledRouteV4) -> Result<Self, Self::Error> {
        Ok(InstalledRoute {
            route: installed_route
                .route
                .ok_or(FidlConversionError::RequiredFieldUnset(
                    "fuchsia.net.routes/InstalledRouteV4.route",
                ))?
                .try_into()?,
            effective_properties: installed_route
                .effective_properties
                .ok_or(FidlConversionError::RequiredFieldUnset(
                    "fuchsia.net.routes/InstalledRouteV4.effective_properties",
                ))?
                .try_into()?,
        })
    }
}

impl TryFrom<fnet_routes::InstalledRouteV6> for InstalledRoute<Ipv6> {
    type Error = FidlConversionError;
    fn try_from(installed_route: fnet_routes::InstalledRouteV6) -> Result<Self, Self::Error> {
        Ok(InstalledRoute {
            route: installed_route
                .route
                .ok_or(FidlConversionError::RequiredFieldUnset(
                    "fuchsia.net.routes/InstalledRouteV6.route",
                ))?
                .try_into()?,
            effective_properties: installed_route
                .effective_properties
                .ok_or(FidlConversionError::RequiredFieldUnset(
                    "fuchsia.net.routes/InstalledRouteV6.effective_properties",
                ))?
                .try_into()?,
        })
    }
}

/// An event reported to the watcher, abstracting over
/// [`fnet_routes::EventV4`] and [fnet_routes::EventV6`].
///
/// These fidl types are both defined as flexible unions, which allows the
/// definition to grow over time. The `Unknown` enum variant accounts for any
/// new types that are not yet known to the local version of the FIDL bindings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Event<I: Ip> {
    /// An unknown event.
    Unknown,
    /// A route that existed prior to watching.
    Existing(InstalledRoute<I>),
    /// Sentinel value indicating no more `existing` events will be received.
    Idle,
    /// A route that was added while watching.
    Added(InstalledRoute<I>),
    /// A route that was removed while watching.
    Removed(InstalledRoute<I>),
}

impl TryFrom<fnet_routes::EventV4> for Event<Ipv4> {
    type Error = FidlConversionError;
    fn try_from(event: fnet_routes::EventV4) -> Result<Self, Self::Error> {
        match event {
            fnet_routes::EventV4::Existing(route) => Ok(Event::Existing(route.try_into()?)),
            fnet_routes::EventV4::Idle(fnet_routes::Empty) => Ok(Event::Idle),
            fnet_routes::EventV4::Added(route) => Ok(Event::Added(route.try_into()?)),
            fnet_routes::EventV4::Removed(route) => Ok(Event::Removed(route.try_into()?)),
            fnet_routes::EventV4Unknown!() => Ok(Event::Unknown),
        }
    }
}

impl TryFrom<fnet_routes::EventV6> for Event<Ipv6> {
    type Error = FidlConversionError;
    fn try_from(event: fnet_routes::EventV6) -> Result<Self, Self::Error> {
        match event {
            fnet_routes::EventV6::Existing(route) => Ok(Event::Existing(route.try_into()?)),
            fnet_routes::EventV6::Idle(fnet_routes::Empty) => Ok(Event::Idle),
            fnet_routes::EventV6::Added(route) => Ok(Event::Added(route.try_into()?)),
            fnet_routes::EventV6::Removed(route) => Ok(Event::Removed(route.try_into()?)),
            fnet_routes::EventV6Unknown!() => Ok(Event::Unknown),
        }
    }
}

/// Route watcher creation errors.
#[derive(Clone, Debug, Error)]
pub enum WatcherCreationError {
    /// Proxy creation failed.
    #[error("failed to create route watcher proxy: {0}")]
    CreateProxy(fidl::Error),
    /// Watcher acquisition failed.
    #[error("failed to get route watcher: {0}")]
    GetWatcher(fidl::Error),
}

/// Route watcher `Watch` errors.
#[derive(Clone, Debug, Error)]
pub enum WatchError {
    /// The call to `Watch` returned a FIDL error.
    #[error("the call to `Watch()` failed: {0}")]
    Fidl(fidl::Error),
    /// The event returned by `Watch` encountered a conversion error.
    #[error("failed to convert event returned by `Watch()`: {0}")]
    Conversion(FidlConversionError),
    /// The server returned an empty batch of events.
    #[error("the call to `Watch()` returned an empty batch of events")]
    EmptyEventBatch,
}

/// IP Extension for the `fuchsia.net.routes` FIDL API.
pub trait FidlRouteIpExt: Ip {
    /// The "state" protocol to use for this IP version.
    type StateMarker: fidl::endpoints::DiscoverableProtocolMarker;
    /// The "watcher" protocol to use for this IP version.
    type WatcherMarker: fidl::endpoints::ProtocolMarker;
    /// The type of "event" returned by the this IP version's watcher protocol.
    type WatchEvent: TryInto<Event<Self>, Error = FidlConversionError>
        + Clone
        + std::fmt::Debug
        + PartialEq
        + Unpin;
}

impl FidlRouteIpExt for Ipv4 {
    type StateMarker = fnet_routes::StateV4Marker;
    type WatcherMarker = fnet_routes::WatcherV4Marker;
    type WatchEvent = fnet_routes::EventV4;
}

impl FidlRouteIpExt for Ipv6 {
    type StateMarker = fnet_routes::StateV6Marker;
    type WatcherMarker = fnet_routes::WatcherV6Marker;
    type WatchEvent = fnet_routes::EventV6;
}

/// Dispatches either `GetWatcherV4` or `GetWatcherV6` on the state proxy.
pub fn get_watcher<I: FidlRouteIpExt>(
    state_proxy: &<I::StateMarker as fidl::endpoints::ProtocolMarker>::Proxy,
) -> Result<<I::WatcherMarker as fidl::endpoints::ProtocolMarker>::Proxy, WatcherCreationError> {
    let (watcher_proxy, watcher_server_end) = fidl::endpoints::create_proxy::<I::WatcherMarker>()
        .map_err(WatcherCreationError::CreateProxy)?;

    #[derive(GenericOverIp)]
    struct GetWatcherInputs<'a, I: Ip + FidlRouteIpExt> {
        watcher_server_end: fidl::endpoints::ServerEnd<I::WatcherMarker>,
        state_proxy: &'a <I::StateMarker as fidl::endpoints::ProtocolMarker>::Proxy,
    }
    let IpInvariant(result) = I::map_ip::<GetWatcherInputs<'_, I>, _>(
        GetWatcherInputs::<'_, I> { watcher_server_end, state_proxy },
        |GetWatcherInputs { watcher_server_end, state_proxy }| {
            IpInvariant(
                state_proxy
                    .get_watcher_v4(watcher_server_end, fnet_routes::WatcherOptionsV4::EMPTY),
            )
        },
        |GetWatcherInputs { watcher_server_end, state_proxy }| {
            IpInvariant(
                state_proxy
                    .get_watcher_v6(watcher_server_end, fnet_routes::WatcherOptionsV6::EMPTY),
            )
        },
    );

    result.map_err(WatcherCreationError::GetWatcher)?;
    Ok(watcher_proxy)
}

/// Calls `Watch()` on the provided `WatcherV4` or `WatcherV6` proxy.
pub fn watch<'a, I: FidlRouteIpExt>(
    watcher_proxy: &'a <I::WatcherMarker as fidl::endpoints::ProtocolMarker>::Proxy,
) -> impl Future<Output = Result<Vec<I::WatchEvent>, fidl::Error>> {
    #[derive(GenericOverIp)]
    struct WatchInputs<'a, I: Ip + FidlRouteIpExt> {
        watcher_proxy: &'a <I::WatcherMarker as fidl::endpoints::ProtocolMarker>::Proxy,
    }
    #[derive(GenericOverIp)]
    struct WatchOutputs<I: Ip + FidlRouteIpExt> {
        watch_fut: fidl::client::QueryResponseFut<Vec<I::WatchEvent>>,
    }
    let WatchOutputs { watch_fut } = I::map_ip::<WatchInputs<'_, I>, WatchOutputs<I>>(
        WatchInputs { watcher_proxy },
        |WatchInputs { watcher_proxy }| WatchOutputs { watch_fut: watcher_proxy.watch() },
        |WatchInputs { watcher_proxy }| WatchOutputs { watch_fut: watcher_proxy.watch() },
    );
    watch_fut
}

/// Connects to the watcher protocol and converts the Hanging-Get style API into
/// an Event stream.
///
/// Each call to `Watch` returns a batch of events, which are flattened into a
/// single stream. If an error is encountered while calling `Watch` or while
/// converting the event, the stream is immediately terminated.
pub fn event_stream_from_state<I: FidlRouteIpExt>(
    routes_state: &<I::StateMarker as fidl::endpoints::ProtocolMarker>::Proxy,
) -> Result<impl Stream<Item = Result<Event<I>, WatchError>>, WatcherCreationError> {
    let watcher = get_watcher::<I>(routes_state)?;
    Ok(futures::stream::try_unfold(watcher, |watcher| async {
        let events_batch = watch::<I>(&watcher).await.map_err(WatchError::Fidl)?;
        if events_batch.is_empty() {
            return Err(WatchError::EmptyEventBatch);
        }
        // Convert the `I::WatchEvent` into an `Event<I>` and return any error.
        let events_batch = events_batch
            .into_iter()
            .map(|event| event.try_into())
            .collect::<Result<Vec<_>, _>>()
            .map_err(WatchError::Conversion)?;
        // Below, `try_flatten` requires that the inner stream yields `Result`s.
        let event_stream = futures::stream::iter(events_batch).map(Ok);
        Ok(Some((event_stream, watcher)))
    })
    // Flatten the stream of event streams into a single event stream.
    .try_flatten())
}

/// Errors returned by [`collect_routes_until_idle`].
#[derive(Clone, Debug, Error)]
pub enum CollectRoutesUntilIdleError<I: FidlRouteIpExt> {
    /// There was an error in the event stream.
    #[error("there was an error in the event stream: {0}")]
    ErrorInStream(WatchError),
    /// There was an unexpected event in the event stream. Only `existing` or
    /// `idle` events are expected.
    #[error("there was an unexpected event in the event stream: {0:?}")]
    UnexpectedEvent(Event<I>),
    /// The event stream unexpectedly ended.
    #[error("the event stream unexpectedly ended")]
    StreamEnded,
}

/// Collects all `existing` events from the stream, stopping once the `idle`
/// event is observed.
pub async fn collect_routes_until_idle<
    I: FidlRouteIpExt,
    C: Extend<InstalledRoute<I>> + Default,
>(
    event_stream: impl futures::Stream<Item = Result<Event<I>, WatchError>> + Unpin,
) -> Result<C, CollectRoutesUntilIdleError<I>> {
    fold::fold_while(
        event_stream,
        Ok(C::default()),
        |existing_routes: Result<C, CollectRoutesUntilIdleError<I>>, event| {
            futures::future::ready(match existing_routes {
                Err(_) => {
                    unreachable!("`existing_routes` must be `Ok`, because we stop folding on err")
                }
                Ok(mut existing_routes) => match event {
                    Err(e) => {
                        fold::FoldWhile::Done(Err(CollectRoutesUntilIdleError::ErrorInStream(e)))
                    }
                    Ok(e) => match e {
                        Event::Existing(e) => {
                            existing_routes.extend([e]);
                            fold::FoldWhile::Continue(Ok(existing_routes))
                        }
                        Event::Idle => fold::FoldWhile::Done(Ok(existing_routes)),
                        e @ Event::Unknown | e @ Event::Added(_) | e @ Event::Removed(_) => {
                            fold::FoldWhile::Done(Err(
                                CollectRoutesUntilIdleError::UnexpectedEvent(e),
                            ))
                        }
                    },
                },
            })
        },
    )
    .await
    .short_circuited()
    .map_err(|_accumulated_thus_far: Result<C, CollectRoutesUntilIdleError<I>>| {
        CollectRoutesUntilIdleError::StreamEnded
    })?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;
    use assert_matches::assert_matches;
    use fidl_fuchsia_net as _;
    use fuchsia_zircon_status as zx_status;
    use futures::FutureExt;
    use net_declare::{
        fidl_ip_v4, fidl_ip_v4_with_prefix, fidl_ip_v6, fidl_ip_v6_with_prefix, net_ip_v4,
        net_ip_v6, net_subnet_v4, net_subnet_v6,
    };
    use netstack_testing_macros::netstack_test;
    use test_case::test_case;

    /// Allows types to provided an arbitrary but valid value for tests.
    trait ArbitraryTestValue {
        const ARBITRARY_TEST_VALUE: Self;
    }

    impl ArbitraryTestValue for fnet_routes::SpecifiedRouteProperties {
        const ARBITRARY_TEST_VALUE: Self = fnet_routes::SpecifiedRouteProperties {
            metric: Some(fnet_routes::SpecifiedMetric::ExplicitMetric(0)),
            ..fnet_routes::SpecifiedRouteProperties::EMPTY
        };
    }

    impl ArbitraryTestValue for fnet_routes::EffectiveRouteProperties {
        const ARBITRARY_TEST_VALUE: Self = fnet_routes::EffectiveRouteProperties {
            metric: Some(0),
            ..fnet_routes::EffectiveRouteProperties::EMPTY
        };
    }

    impl ArbitraryTestValue for fnet_routes::RoutePropertiesV4 {
        const ARBITRARY_TEST_VALUE: Self = fnet_routes::RoutePropertiesV4 {
            specified_properties: Some(fnet_routes::SpecifiedRouteProperties::ARBITRARY_TEST_VALUE),
            ..fnet_routes::RoutePropertiesV4::EMPTY
        };
    }

    impl ArbitraryTestValue for fnet_routes::RoutePropertiesV6 {
        const ARBITRARY_TEST_VALUE: Self = fnet_routes::RoutePropertiesV6 {
            specified_properties: Some(fnet_routes::SpecifiedRouteProperties::ARBITRARY_TEST_VALUE),
            ..fnet_routes::RoutePropertiesV6::EMPTY
        };
    }

    impl ArbitraryTestValue for fnet_routes::RouteTargetV4 {
        const ARBITRARY_TEST_VALUE: Self =
            fnet_routes::RouteTargetV4 { outbound_interface: 1, next_hop: None };
    }

    impl ArbitraryTestValue for fnet_routes::RouteTargetV6 {
        const ARBITRARY_TEST_VALUE: Self =
            fnet_routes::RouteTargetV6 { outbound_interface: 1, next_hop: None };
    }

    impl ArbitraryTestValue for fnet_routes::RouteActionV4 {
        const ARBITRARY_TEST_VALUE: Self =
            fnet_routes::RouteActionV4::Forward(fnet_routes::RouteTargetV4::ARBITRARY_TEST_VALUE);
    }

    impl ArbitraryTestValue for fnet_routes::RouteActionV6 {
        const ARBITRARY_TEST_VALUE: Self =
            fnet_routes::RouteActionV6::Forward(fnet_routes::RouteTargetV6::ARBITRARY_TEST_VALUE);
    }

    impl ArbitraryTestValue for fnet_routes::RouteV4 {
        const ARBITRARY_TEST_VALUE: Self = fnet_routes::RouteV4 {
            destination: fidl_ip_v4_with_prefix!("192.168.0.0/24"),
            action: fnet_routes::RouteActionV4::ARBITRARY_TEST_VALUE,
            properties: fnet_routes::RoutePropertiesV4::ARBITRARY_TEST_VALUE,
        };
    }

    impl ArbitraryTestValue for fnet_routes::RouteV6 {
        const ARBITRARY_TEST_VALUE: Self = fnet_routes::RouteV6 {
            destination: fidl_ip_v6_with_prefix!("fe80::0/64"),
            action: fnet_routes::RouteActionV6::ARBITRARY_TEST_VALUE,
            properties: fnet_routes::RoutePropertiesV6::ARBITRARY_TEST_VALUE,
        };
    }

    impl ArbitraryTestValue for fnet_routes::InstalledRouteV4 {
        const ARBITRARY_TEST_VALUE: Self = fnet_routes::InstalledRouteV4 {
            route: Some(fnet_routes::RouteV4::ARBITRARY_TEST_VALUE),
            effective_properties: Some(fnet_routes::EffectiveRouteProperties::ARBITRARY_TEST_VALUE),
            ..fnet_routes::InstalledRouteV4::EMPTY
        };
    }

    impl ArbitraryTestValue for fnet_routes::InstalledRouteV6 {
        const ARBITRARY_TEST_VALUE: Self = fnet_routes::InstalledRouteV6 {
            route: Some(fnet_routes::RouteV6::ARBITRARY_TEST_VALUE),
            effective_properties: Some(fnet_routes::EffectiveRouteProperties::ARBITRARY_TEST_VALUE),
            ..fnet_routes::InstalledRouteV6::EMPTY
        };
    }

    #[test]
    fn specified_route_properties_try_from_unset_metric() {
        assert_eq!(
            SpecifiedRouteProperties::try_from(fnet_routes::SpecifiedRouteProperties::EMPTY),
            Err(FidlConversionError::RequiredFieldUnset(
                "fuchsia.net.routes/SpecifiedRouteProperties.metric",
            ))
        )
    }

    #[test]
    fn specified_route_properties_try_from() {
        assert_eq!(
            fnet_routes::SpecifiedRouteProperties {
                metric: Some(fnet_routes::SpecifiedMetric::ExplicitMetric(1)),
                ..fnet_routes::SpecifiedRouteProperties::EMPTY
            }
            .try_into(),
            Ok(SpecifiedRouteProperties {
                metric: fnet_routes::SpecifiedMetric::ExplicitMetric(1),
            }),
        )
    }

    #[test]
    fn effective_route_properties_try_from_unset_metric() {
        assert_eq!(
            EffectiveRouteProperties::try_from(fnet_routes::EffectiveRouteProperties::EMPTY),
            Err(FidlConversionError::RequiredFieldUnset(
                "fuchsia.net.routes/EffectiveRouteProperties.metric",
            ))
        )
    }

    #[test]
    fn effective_route_properties_try_from() {
        assert_eq!(
            fnet_routes::EffectiveRouteProperties {
                metric: Some(1),
                ..fnet_routes::EffectiveRouteProperties::EMPTY
            }
            .try_into(),
            Ok(EffectiveRouteProperties { metric: 1 }),
        )
    }

    #[test]
    fn route_properties_try_from_unset_specified_properties_v4() {
        assert_eq!(
            RouteProperties::try_from(fnet_routes::RoutePropertiesV4::EMPTY),
            Err(FidlConversionError::RequiredFieldUnset(
                "fuchsia.net.routes/RoutePropertiesV4.specified_properties"
            ))
        )
    }

    #[test]
    fn route_properties_try_from_unset_specified_properties_v6() {
        assert_eq!(
            RouteProperties::try_from(fnet_routes::RoutePropertiesV6::EMPTY),
            Err(FidlConversionError::RequiredFieldUnset(
                "fuchsia.net.routes/RoutePropertiesV6.specified_properties"
            ))
        )
    }

    #[test]
    fn route_properties_try_from_v4() {
        assert_eq!(
            fnet_routes::RoutePropertiesV4 {
                specified_properties: Some(
                    fnet_routes::SpecifiedRouteProperties::ARBITRARY_TEST_VALUE
                ),
                ..fnet_routes::RoutePropertiesV4::EMPTY
            }
            .try_into(),
            Ok(RouteProperties {
                specified_properties: fnet_routes::SpecifiedRouteProperties::ARBITRARY_TEST_VALUE
                    .try_into()
                    .unwrap()
            })
        )
    }

    #[test]
    fn route_properties_try_from_v6() {
        assert_eq!(
            fnet_routes::RoutePropertiesV6 {
                specified_properties: Some(
                    fnet_routes::SpecifiedRouteProperties::ARBITRARY_TEST_VALUE
                ),
                ..fnet_routes::RoutePropertiesV6::EMPTY
            }
            .try_into(),
            Ok(RouteProperties {
                specified_properties: fnet_routes::SpecifiedRouteProperties::ARBITRARY_TEST_VALUE
                    .try_into()
                    .unwrap()
            })
        )
    }

    #[test]
    fn route_target_try_from_unspecified_next_hop_v4() {
        assert_eq!(
            RouteTarget::try_from(fnet_routes::RouteTargetV4 {
                outbound_interface: 1,
                next_hop: Some(Box::new(fidl_ip_v4!("0.0.0.0"))),
            }),
            Err(FidlConversionError::UnspecifiedNextHop)
        )
    }

    #[test]
    fn route_target_try_from_unspecified_next_hop_v6() {
        assert_eq!(
            RouteTarget::try_from(fnet_routes::RouteTargetV6 {
                outbound_interface: 1,
                next_hop: Some(Box::new(fidl_ip_v6!("::"))),
            }),
            Err(FidlConversionError::UnspecifiedNextHop)
        );
    }

    #[test]
    fn route_target_try_from_multicast_next_hop_v6() {
        assert_eq!(
            RouteTarget::try_from(fnet_routes::RouteTargetV6 {
                outbound_interface: 1,
                next_hop: Some(Box::new(fidl_ip_v6!("ff00::1"))),
            }),
            Err(FidlConversionError::NextHopNotUnicast)
        )
    }

    #[test]
    fn route_target_try_from_v4() {
        assert_eq!(
            fnet_routes::RouteTargetV4 {
                outbound_interface: 1,
                next_hop: Some(Box::new(fidl_ip_v4!("192.168.0.1"))),
            }
            .try_into(),
            Ok(RouteTarget {
                outbound_interface: 1,
                next_hop: Some(SpecifiedAddr::new(net_ip_v4!("192.168.0.1")).unwrap())
            })
        )
    }

    #[test]
    fn route_target_try_from_v6() {
        assert_eq!(
            fnet_routes::RouteTargetV6 {
                outbound_interface: 1,
                next_hop: Some(Box::new(fidl_ip_v6!("fe80::1")))
            }
            .try_into(),
            Ok(RouteTarget {
                outbound_interface: 1,
                next_hop: Some(SpecifiedAddr::new(net_ip_v6!("fe80::1")).unwrap())
            })
        )
    }

    #[test]
    fn route_action_try_from_forward_v4() {
        assert_eq!(
            fnet_routes::RouteActionV4::Forward(fnet_routes::RouteTargetV4::ARBITRARY_TEST_VALUE)
                .try_into(),
            Ok(RouteAction::Forward(
                fnet_routes::RouteTargetV4::ARBITRARY_TEST_VALUE.try_into().unwrap()
            ))
        )
    }

    #[test]
    fn route_action_try_from_forward_v6() {
        assert_eq!(
            fnet_routes::RouteActionV6::Forward(fnet_routes::RouteTargetV6::ARBITRARY_TEST_VALUE)
                .try_into(),
            Ok(RouteAction::Forward(
                fnet_routes::RouteTargetV6::ARBITRARY_TEST_VALUE.try_into().unwrap()
            ))
        )
    }

    #[test]
    fn route_action_try_from_unknown_v4() {
        assert_eq!(
            fnet_routes::RouteActionV4::unknown_variant_for_testing().try_into(),
            Ok(RouteAction::Unknown)
        )
    }

    #[test]
    fn route_action_try_from_unknown_v6() {
        assert_eq!(
            fnet_routes::RouteActionV6::unknown_variant_for_testing().try_into(),
            Ok(RouteAction::Unknown)
        )
    }

    #[test]
    fn route_try_from_invalid_destination_v4() {
        assert_matches!(
            Route::try_from(fnet_routes::RouteV4 {
                // Invalid, because subnets should not have the "host bits" set.
                destination: fidl_ip_v4_with_prefix!("192.168.0.1/24"),
                action: fnet_routes::RouteActionV4::ARBITRARY_TEST_VALUE,
                properties: fnet_routes::RoutePropertiesV4::ARBITRARY_TEST_VALUE,
            }),
            Err(FidlConversionError::DestinationSubnet(_))
        );
    }

    #[test]
    fn route_try_from_invalid_destination_v6() {
        assert_matches!(
            Route::try_from(fnet_routes::RouteV6 {
                // Invalid, because subnets should not have the "host bits" set.
                destination: fidl_ip_v6_with_prefix!("fe80::1/64"),
                action: fnet_routes::RouteActionV6::ARBITRARY_TEST_VALUE,
                properties: fnet_routes::RoutePropertiesV6::ARBITRARY_TEST_VALUE,
            }),
            Err(FidlConversionError::DestinationSubnet(_))
        );
    }

    #[test]
    fn route_try_from_v4() {
        assert_eq!(
            fnet_routes::RouteV4 {
                destination: fidl_ip_v4_with_prefix!("192.168.0.0/24"),
                action: fnet_routes::RouteActionV4::ARBITRARY_TEST_VALUE,
                properties: fnet_routes::RoutePropertiesV4::ARBITRARY_TEST_VALUE,
            }
            .try_into(),
            Ok(Route {
                destination: net_subnet_v4!("192.168.0.0/24"),
                action: fnet_routes::RouteActionV4::ARBITRARY_TEST_VALUE.try_into().unwrap(),
                properties: fnet_routes::RoutePropertiesV4::ARBITRARY_TEST_VALUE
                    .try_into()
                    .unwrap(),
            })
        )
    }

    #[test]
    fn route_try_from_v6() {
        assert_eq!(
            fnet_routes::RouteV6 {
                destination: fidl_ip_v6_with_prefix!("fe80::0/64"),
                action: fnet_routes::RouteActionV6::ARBITRARY_TEST_VALUE,
                properties: fnet_routes::RoutePropertiesV6::ARBITRARY_TEST_VALUE,
            }
            .try_into(),
            Ok(Route {
                destination: net_subnet_v6!("fe80::0/64"),
                action: fnet_routes::RouteActionV6::ARBITRARY_TEST_VALUE.try_into().unwrap(),
                properties: fnet_routes::RoutePropertiesV6::ARBITRARY_TEST_VALUE
                    .try_into()
                    .unwrap(),
            })
        )
    }

    #[test]
    fn installed_route_try_from_unset_route_v4() {
        assert_eq!(
            InstalledRoute::try_from(fnet_routes::InstalledRouteV4 {
                route: None,
                effective_properties: Some(
                    fnet_routes::EffectiveRouteProperties::ARBITRARY_TEST_VALUE,
                ),
                ..fnet_routes::InstalledRouteV4::EMPTY
            }),
            Err(FidlConversionError::RequiredFieldUnset(
                "fuchsia.net.routes/InstalledRouteV4.route"
            ))
        )
    }

    #[test]
    fn installed_route_try_from_unset_route_v6() {
        assert_eq!(
            InstalledRoute::try_from(fnet_routes::InstalledRouteV6 {
                route: None,
                effective_properties: Some(
                    fnet_routes::EffectiveRouteProperties::ARBITRARY_TEST_VALUE,
                ),
                ..fnet_routes::InstalledRouteV6::EMPTY
            }),
            Err(FidlConversionError::RequiredFieldUnset(
                "fuchsia.net.routes/InstalledRouteV6.route"
            ))
        )
    }

    #[test]
    fn installed_route_try_from_unset_effective_properties_v4() {
        assert_eq!(
            InstalledRoute::try_from(fnet_routes::InstalledRouteV4 {
                route: Some(fnet_routes::RouteV4::ARBITRARY_TEST_VALUE),
                effective_properties: None,
                ..fnet_routes::InstalledRouteV4::EMPTY
            }),
            Err(FidlConversionError::RequiredFieldUnset(
                "fuchsia.net.routes/InstalledRouteV4.effective_properties"
            ))
        )
    }

    #[test]
    fn installed_route_try_from_unset_effective_properties_v6() {
        assert_eq!(
            InstalledRoute::try_from(fnet_routes::InstalledRouteV6 {
                route: Some(fnet_routes::RouteV6::ARBITRARY_TEST_VALUE),
                effective_properties: None,
                ..fnet_routes::InstalledRouteV6::EMPTY
            }),
            Err(FidlConversionError::RequiredFieldUnset(
                "fuchsia.net.routes/InstalledRouteV6.effective_properties"
            ))
        )
    }

    #[test]
    fn installed_route_try_from_v4() {
        assert_eq!(
            fnet_routes::InstalledRouteV4 {
                route: Some(fnet_routes::RouteV4::ARBITRARY_TEST_VALUE),
                effective_properties: Some(
                    fnet_routes::EffectiveRouteProperties::ARBITRARY_TEST_VALUE
                ),
                ..fnet_routes::InstalledRouteV4::EMPTY
            }
            .try_into(),
            Ok(InstalledRoute {
                route: fnet_routes::RouteV4::ARBITRARY_TEST_VALUE.try_into().unwrap(),
                effective_properties: fnet_routes::EffectiveRouteProperties::ARBITRARY_TEST_VALUE
                    .try_into()
                    .unwrap(),
            })
        )
    }

    #[test]
    fn installed_route_try_from_v6() {
        assert_eq!(
            fnet_routes::InstalledRouteV6 {
                route: Some(fnet_routes::RouteV6::ARBITRARY_TEST_VALUE),
                effective_properties: Some(
                    fnet_routes::EffectiveRouteProperties::ARBITRARY_TEST_VALUE
                ),
                ..fnet_routes::InstalledRouteV6::EMPTY
            }
            .try_into(),
            Ok(InstalledRoute {
                route: fnet_routes::RouteV6::ARBITRARY_TEST_VALUE.try_into().unwrap(),
                effective_properties: fnet_routes::EffectiveRouteProperties::ARBITRARY_TEST_VALUE
                    .try_into()
                    .unwrap(),
            })
        )
    }

    #[test]
    fn event_try_from_v4() {
        const DEFAULT_ROUTE: fnet_routes::InstalledRouteV4 =
            fnet_routes::InstalledRouteV4::ARBITRARY_TEST_VALUE;
        let expected_route = DEFAULT_ROUTE.try_into().unwrap();
        assert_eq!(
            fnet_routes::EventV4::unknown_variant_for_testing().try_into(),
            Ok(Event::Unknown)
        );
        assert_eq!(
            fnet_routes::EventV4::Existing(DEFAULT_ROUTE).try_into(),
            Ok(Event::Existing(expected_route))
        );
        assert_eq!(fnet_routes::EventV4::Idle(fnet_routes::Empty).try_into(), Ok(Event::Idle));
        assert_eq!(
            fnet_routes::EventV4::Added(DEFAULT_ROUTE).try_into(),
            Ok(Event::Added(expected_route))
        );
        assert_eq!(
            fnet_routes::EventV4::Removed(DEFAULT_ROUTE).try_into(),
            Ok(Event::Removed(expected_route))
        );
    }

    #[test]
    fn event_try_from_v6() {
        const DEFAULT_ROUTE: fnet_routes::InstalledRouteV6 =
            fnet_routes::InstalledRouteV6::ARBITRARY_TEST_VALUE;
        let expected_route = DEFAULT_ROUTE.try_into().unwrap();
        assert_eq!(
            fnet_routes::EventV6::unknown_variant_for_testing().try_into(),
            Ok(Event::Unknown)
        );
        assert_eq!(
            fnet_routes::EventV6::Existing(DEFAULT_ROUTE).try_into(),
            Ok(Event::Existing(expected_route))
        );
        assert_eq!(fnet_routes::EventV6::Idle(fnet_routes::Empty).try_into(), Ok(Event::Idle));
        assert_eq!(
            fnet_routes::EventV6::Added(DEFAULT_ROUTE).try_into(),
            Ok(Event::Added(expected_route))
        );
        assert_eq!(
            fnet_routes::EventV6::Removed(DEFAULT_ROUTE).try_into(),
            Ok(Event::Removed(expected_route))
        );
    }

    // Tests the `event_stream_from_state` with various "shapes". The test
    // parameter is a vec of ranges, where each range corresponds to the batch
    // of events that will be sent in response to a single call to `Watch().
    #[netstack_test]
    #[test_case(Vec::new(); "no events")]
    #[test_case(vec![0..1]; "single_batch_single_event")]
    #[test_case(vec![0..10]; "single_batch_many_events")]
    #[test_case(vec![0..10, 10..20, 20..30]; "many_batches_many_events")]
    async fn event_stream_from_state_against_shape<I: net_types::ip::Ip + FidlRouteIpExt>(
        // TODO(https://fxbug.dev/119320): remove `_test_name` once optional.
        _test_name: &str,
        test_shape: Vec<std::ops::Range<u32>>,
    ) {
        // Build the event stream based on the `test_shape`. Use a channel
        // so that the stream stays open until `close_channel` is called later.
        let (batches_sender, batches_receiver) =
            futures::channel::mpsc::unbounded::<Vec<I::WatchEvent>>();
        for batch_shape in &test_shape {
            batches_sender
                .unbounded_send(testutil::generate_events_in_range::<I>(batch_shape.clone()))
                .expect("failed to send event batch");
        }

        // Instantiate the fake Watcher implementation.
        let (state, state_server_end) =
            fidl::endpoints::create_proxy::<I::StateMarker>().expect("failed to create proxy");
        let (mut state_request_stream, _control_handle) = state_server_end
            .into_stream_and_control_handle()
            .expect("failed to get `State` request stream");
        let watcher_fut = state_request_stream
            .next()
            .then(|req| {
                testutil::serve_state_request::<I>(
                    req.expect("State request_stream unexpectedly ended"),
                    batches_receiver,
                )
            })
            .fuse();

        let event_stream =
            event_stream_from_state::<I>(&state).expect("failed to connect to watcher").fuse();

        futures::pin_mut!(watcher_fut, event_stream);

        for batch_shape in test_shape {
            for event_idx in batch_shape.into_iter() {
                futures::select! {
                    () = watcher_fut => panic!("fake watcher implementation unexpectedly finished"),
                    event = event_stream.next() => {
                        let actual_event = event
                            .expect("event stream unexpectedly empty")
                            .expect("error processing event");
                        let expected_event = testutil::generate_event::<I>(event_idx)
                                .try_into()
                                .expect("test event is unexpectedly invalid");
                        assert_eq!(actual_event, expected_event);
                    }
                };
            }
        }

        // Close `batches_sender` and observe that the `event_stream` ends.
        batches_sender.close_channel();
        let ((), mut events) = futures::join!(watcher_fut, event_stream.collect::<Vec<_>>());
        assert_matches!(
            events.pop(),
            Some(Err(WatchError::Fidl(fidl::Error::ClientChannelClosed {
                status: zx_status::Status::PEER_CLOSED,
                ..
            })))
        );
        assert_matches!(events[..], []);
    }

    // Verify that calling `event_stream_from_state` multiple times with the
    // same `State` proxy, results in independent `Watcher` clients.
    #[netstack_test]
    async fn event_stream_from_state_multiple_watchers<I: net_types::ip::Ip + FidlRouteIpExt>(
        // TODO(https://fxbug.dev/119320): remove `_test_name` once optional.
        _test_name: &str,
    ) {
        // Events for 3 watchers. Each receives one batch containing 10 events.
        let test_data = vec![
            vec![testutil::generate_events_in_range::<I>(0..10)],
            vec![testutil::generate_events_in_range::<I>(10..20)],
            vec![testutil::generate_events_in_range::<I>(20..30)],
        ];

        // Instantiate the fake Watcher implementations.
        let (state, state_server_end) =
            fidl::endpoints::create_proxy::<I::StateMarker>().expect("failed to create proxy");
        let (state_request_stream, _control_handle) = state_server_end
            .into_stream_and_control_handle()
            .expect("failed to get `State` request stream");
        let watchers_fut = state_request_stream
            .zip(futures::stream::iter(test_data.clone()))
            .for_each_concurrent(std::usize::MAX, |(request, watcher_data)| {
                testutil::serve_state_request::<I>(request, futures::stream::iter(watcher_data))
            });

        let validate_event_streams_fut =
            futures::future::join_all(test_data.into_iter().map(|watcher_data| {
                let events_fut = event_stream_from_state::<I>(&state)
                    .expect("failed to connect to watcher")
                    .collect::<std::collections::VecDeque<_>>();
                events_fut.then(|mut events| {
                    for expected_event in watcher_data.into_iter().flatten() {
                        assert_eq!(
                            events
                                .pop_front()
                                .expect("event_stream unexpectedly empty")
                                .expect("error processing event"),
                            expected_event.try_into().expect("test event is unexpectedly invalid"),
                        );
                    }
                    assert_matches!(
                        events.pop_front(),
                        Some(Err(WatchError::Fidl(fidl::Error::ClientChannelClosed {
                            status: zx_status::Status::PEER_CLOSED,
                            ..
                        })))
                    );
                    assert_matches!(events.make_contiguous(), []);
                    futures::future::ready(())
                })
            }));

        let ((), _): ((), Vec<()>) = futures::join!(watchers_fut, validate_event_streams_fut);
    }

    // Verify that failing to convert an event results in an error and closes
    // the event stream. `trailing_event` and `trailing_batch` control whether
    // a good event is sent after the bad event, either as part of the same
    // batch or in a subsequent batch. The test expects this data to be
    // truncated from the resulting event_stream.
    #[netstack_test]
    #[test_case(false, false; "no_trailing")]
    #[test_case(true, false; "trailing_event")]
    #[test_case(false, true; "trailing_batch")]
    #[test_case(true, true; "trailing_event_and_batch")]
    async fn event_stream_from_state_conversion_error<I: net_types::ip::Ip + FidlRouteIpExt>(
        // TODO(https://fxbug.dev/119320): remove `_test_name` once optional.
        _test_name: &str,
        trailing_event: bool,
        trailing_batch: bool,
    ) {
        // Define an event with an invalid destination subnet; receiving it
        // from a call to `Watch` will result in conversion errors.
        #[derive(GenericOverIp)]
        struct EventHolder<I: Ip + FidlRouteIpExt>(I::WatchEvent);
        let EventHolder(bad_event) = I::map_ip(
            (),
            |()| {
                EventHolder(fnet_routes::EventV4::Added(fnet_routes::InstalledRouteV4 {
                    route: Some(fnet_routes::RouteV4 {
                        destination: fidl_ip_v4_with_prefix!("192.168.0.1/24"),
                        ..fnet_routes::RouteV4::ARBITRARY_TEST_VALUE
                    }),
                    ..fnet_routes::InstalledRouteV4::ARBITRARY_TEST_VALUE
                }))
            },
            |()| {
                EventHolder(fnet_routes::EventV6::Added(fnet_routes::InstalledRouteV6 {
                    route: Some(fnet_routes::RouteV6 {
                        destination: fidl_ip_v6_with_prefix!("fe80::1/64"),
                        ..fnet_routes::RouteV6::ARBITRARY_TEST_VALUE
                    }),
                    ..fnet_routes::InstalledRouteV6::ARBITRARY_TEST_VALUE
                }))
            },
        );

        let batch = std::iter::once(bad_event)
            // Optionally append a known good event to the batch.
            .chain(trailing_event.then(|| testutil::generate_event::<I>(0)).into_iter())
            .collect::<Vec<_>>();
        let batches = std::iter::once(batch)
            // Optionally append a known good batch to the sequence of batches.
            .chain(trailing_batch.then(|| vec![testutil::generate_event::<I>(1)]))
            .collect::<Vec<_>>();

        // Instantiate the fake Watcher implementation.
        let (state, state_server_end) =
            fidl::endpoints::create_proxy::<I::StateMarker>().expect("failed to create proxy");
        let (mut state_request_stream, _control_handle) = state_server_end
            .into_stream_and_control_handle()
            .expect("failed to get `State` request stream");
        let watcher_fut = state_request_stream
            .next()
            .then(|req| {
                testutil::serve_state_request::<I>(
                    req.expect("State request_stream unexpectedly ended"),
                    futures::stream::iter(batches),
                )
            })
            .fuse();

        let event_stream =
            event_stream_from_state::<I>(&state).expect("failed to connect to watcher").fuse();

        futures::pin_mut!(watcher_fut, event_stream);
        let ((), events) = futures::join!(watcher_fut, event_stream.collect::<Vec<_>>());
        assert_matches!(&events[..], &[Err(WatchError::Conversion(_))]);
    }

    // Verify that watching an empty batch results in an error and closes
    // the event stream. When `trailing_batch` is true, an additional "good"
    // batch will be sent after the empty batch; the test expects this data to
    // be truncated from the resulting event_stream.
    #[netstack_test]
    #[test_case(false; "no_trailing_batch")]
    #[test_case(true; "trailing_batch")]
    async fn event_stream_from_state_empty_batch_error<I: net_types::ip::Ip + FidlRouteIpExt>(
        // TODO(https://fxbug.dev/119320): remove `_test_name` once optional.
        _test_name: &str,
        trailing_batch: bool,
    ) {
        let batches = std::iter::once(Vec::new())
            // Optionally append a known good batch to the sequence of batches.
            .chain(trailing_batch.then(|| vec![testutil::generate_event::<I>(0)]))
            .collect::<Vec<_>>();

        // Instantiate the fake Watcher implementation.
        let (state, state_server_end) =
            fidl::endpoints::create_proxy::<I::StateMarker>().expect("failed to create proxy");
        let (mut state_request_stream, _control_handle) = state_server_end
            .into_stream_and_control_handle()
            .expect("failed to get `State` request stream");
        let watcher_fut = state_request_stream
            .next()
            .then(|req| {
                testutil::serve_state_request::<I>(
                    req.expect("State request_stream unexpectedly ended"),
                    futures::stream::iter(batches),
                )
            })
            .fuse();

        let event_stream =
            event_stream_from_state::<I>(&state).expect("failed to connect to watcher").fuse();

        futures::pin_mut!(watcher_fut, event_stream);
        let ((), events) = futures::join!(watcher_fut, event_stream.collect::<Vec<_>>());
        assert_matches!(&events[..], &[Err(WatchError::EmptyEventBatch)]);
    }

    fn arbitrary_test_route<I: Ip + FidlRouteIpExt>() -> InstalledRoute<I> {
        #[derive(GenericOverIp)]
        struct RouteHolder<I: Ip + FidlRouteIpExt>(InstalledRoute<I>);
        let RouteHolder(route) = I::map_ip(
            (),
            |()| {
                RouteHolder(fnet_routes::InstalledRouteV4::ARBITRARY_TEST_VALUE.try_into().unwrap())
            },
            |()| {
                RouteHolder(fnet_routes::InstalledRouteV6::ARBITRARY_TEST_VALUE.try_into().unwrap())
            },
        );
        route
    }

    enum CollectRoutesUntilIdleErrorTestCase {
        ErrorInStream,
        UnexpectedEvent,
        StreamEnded,
    }

    #[netstack_test]
    #[test_case(CollectRoutesUntilIdleErrorTestCase::ErrorInStream; "error_in_stream")]
    #[test_case(CollectRoutesUntilIdleErrorTestCase::UnexpectedEvent; "unexpected_event")]
    #[test_case(CollectRoutesUntilIdleErrorTestCase::StreamEnded; "stream_ended")]
    async fn collect_routes_until_idle_error<I: net_types::ip::Ip + FidlRouteIpExt>(
        // TODO(https://fxbug.dev/119320): remove `_test_name` once optional.
        _test_name: &str,
        test_case: CollectRoutesUntilIdleErrorTestCase,
    ) {
        // Build up the test data and the expected outcome base on `test_case`.
        // Note, that `netstack_test` doesn't support test cases whose args are
        // generic functions (below, `test_assertion` is generic over `I`).
        let route = arbitrary_test_route();
        let (event, test_assertion): (_, Box<dyn FnOnce(_)>) = match test_case {
            CollectRoutesUntilIdleErrorTestCase::ErrorInStream => (
                Err(WatchError::EmptyEventBatch),
                Box::new(|result| {
                    assert_matches!(result, Err(CollectRoutesUntilIdleError::ErrorInStream(_)))
                }),
            ),
            CollectRoutesUntilIdleErrorTestCase::UnexpectedEvent => (
                Ok(Event::Added(route)),
                Box::new(|result| {
                    assert_matches!(result, Err(CollectRoutesUntilIdleError::UnexpectedEvent(_)))
                }),
            ),
            CollectRoutesUntilIdleErrorTestCase::StreamEnded => (
                Ok(Event::Existing(route)),
                Box::new(|result| {
                    assert_matches!(result, Err(CollectRoutesUntilIdleError::StreamEnded))
                }),
            ),
        };

        let event_stream = futures::stream::once(futures::future::ready(event));
        futures::pin_mut!(event_stream);
        let result = collect_routes_until_idle::<I, Vec<_>>(event_stream).await;
        test_assertion(result);
    }

    // Verifies that `collect_routes_until_idle` collects all existing events,
    // drops the idle event, and leaves all trailing events intact.
    #[netstack_test]
    async fn collect_routes_until_idle_success<I: net_types::ip::Ip + FidlRouteIpExt>(
        // TODO(https://fxbug.dev/119320): remove `_test_name` once optional.
        _test_name: &str,
    ) {
        let route = arbitrary_test_route();
        let event_stream = futures::stream::iter([
            Ok(Event::Existing(route)),
            Ok(Event::Idle),
            Ok(Event::Added(route)),
        ]);

        futures::pin_mut!(event_stream);
        let existing = collect_routes_until_idle::<I, Vec<_>>(event_stream.by_ref())
            .await
            .expect("failed to collect existing routes");
        assert_eq!(&existing, &[route]);

        let trailing_events = event_stream.collect::<Vec<_>>().await;
        assert_matches!(
            &trailing_events[..],
            &[Ok(Event::Added(found_route))] if found_route == route
        );
    }
}
