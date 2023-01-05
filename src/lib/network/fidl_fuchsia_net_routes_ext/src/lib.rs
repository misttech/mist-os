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

use fidl_fuchsia_net_ext::{IntoExt as _, TryIntoExt as _};
use fidl_fuchsia_net_routes as fnet_routes;
use net_types::{
    ip::{Ip, Ipv4, Ipv6, Ipv6Addr, Subnet},
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
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpecifiedRouteProperties {
    metric: fnet_routes::SpecifiedMetric,
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
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EffectiveRouteProperties {
    metric: u32,
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
#[derive(Clone, Copy, Debug, PartialEq)]
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
#[derive(Clone, Copy, Debug, PartialEq)]
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
#[derive(Clone, Copy, Debug, PartialEq)]
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
#[derive(Clone, Copy, Debug, PartialEq)]
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
#[derive(Clone, Copy, Debug, PartialEq)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use fidl_fuchsia_net as _;
    use net_declare::{
        fidl_ip_v4, fidl_ip_v4_with_prefix, fidl_ip_v6, fidl_ip_v6_with_prefix, net_ip_v4,
        net_ip_v6, net_subnet_v4, net_subnet_v6,
    };

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
}
