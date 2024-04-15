// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! IPv6 Route Discovery as defined by [RFC 4861 section 6.3.4].
//!
//! [RFC 4861 section 6.3.4]: https://datatracker.ietf.org/doc/html/rfc4861#section-6.3.4

use core::hash::Hash;

use fakealloc::collections::HashSet;
use net_types::{
    ip::{Ipv6Addr, Subnet},
    LinkLocalUnicastAddr,
};
use packet_formats::icmp::ndp::NonZeroNdpLifetime;

use crate::{
    context::{TimerContext, TimerHandler},
    device::{AnyDevice, DeviceIdContext},
};

#[derive(Default)]
#[cfg_attr(test, derive(Debug, Eq, PartialEq))]
pub struct Ipv6RouteDiscoveryState {
    // The valid (non-zero lifetime) discovered routes.
    //
    // Routes with a finite lifetime must have a timer set; routes with an
    // infinite lifetime must not.
    routes: HashSet<Ipv6DiscoveredRoute>,
}

/// A discovered route.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub struct Ipv6DiscoveredRoute {
    /// The destination subnet for the route.
    pub subnet: Subnet<Ipv6Addr>,

    /// The next-hop node for the route, if required.
    ///
    /// `None` indicates that the subnet is on-link/directly-connected.
    pub gateway: Option<LinkLocalUnicastAddr<Ipv6Addr>>,
}

/// A timer ID for IPv6 route discovery.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub struct Ipv6DiscoveredRouteTimerId<DeviceId> {
    device_id: DeviceId,
    route: Ipv6DiscoveredRoute,
}

impl<DeviceId> Ipv6DiscoveredRouteTimerId<DeviceId> {
    pub(super) fn device_id(&self) -> &DeviceId {
        let Self { device_id, route: _ } = self;
        device_id
    }
}

/// An implementation of the execution context available when accessing the IPv6
/// route discovery state.
///
/// See [`Ipv6RouteDiscoveryContext::with_discovered_routes_mut`].
pub(super) trait Ipv6DiscoveredRoutesContext<BC>: DeviceIdContext<AnyDevice> {
    /// Adds a newly discovered IPv6 route to the routing table.
    fn add_discovered_ipv6_route(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        route: Ipv6DiscoveredRoute,
    ) -> Result<(), crate::error::ExistsError>;

    /// Deletes a previously discovered (now invalidated) IPv6 route from the
    /// routing table.
    fn del_discovered_ipv6_route(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        route: Ipv6DiscoveredRoute,
    );
}

/// The execution context for IPv6 route discovery.
pub(super) trait Ipv6RouteDiscoveryContext<BC>: DeviceIdContext<AnyDevice> {
    type WithDiscoveredRoutesMutCtx<'a>: Ipv6DiscoveredRoutesContext<BC, DeviceId = Self::DeviceId>;

    /// Gets the route discovery state, mutably.
    fn with_discovered_routes_mut<
        F: FnOnce(&mut Ipv6RouteDiscoveryState, &mut Self::WithDiscoveredRoutesMutCtx<'_>),
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    );
}

/// The bindings execution context for IPv6 route discovery.
trait Ipv6RouteDiscoveryBindingsContext<DeviceId>:
    TimerContext<Ipv6DiscoveredRouteTimerId<DeviceId>>
{
}
impl<DeviceId, BC: TimerContext<Ipv6DiscoveredRouteTimerId<DeviceId>>>
    Ipv6RouteDiscoveryBindingsContext<DeviceId> for BC
{
}

/// An implementation of IPv6 route discovery.
pub trait RouteDiscoveryHandler<BC>: DeviceIdContext<AnyDevice> {
    /// Handles an update affecting discovered routes.
    ///
    /// A `None` value for `lifetime` indicates that the route is not valid and
    /// must be invalidated if it has been discovered; a `Some(_)` value
    /// indicates the new maximum lifetime that the route may be valid for
    /// before being invalidated.
    fn update_route(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        route: Ipv6DiscoveredRoute,
        lifetime: Option<NonZeroNdpLifetime>,
    );

    /// Invalidates all discovered routes.
    fn invalidate_routes(&mut self, bindings_ctx: &mut BC, device_id: &Self::DeviceId);
}

impl<BC: Ipv6RouteDiscoveryBindingsContext<CC::DeviceId>, CC: Ipv6RouteDiscoveryContext<BC>>
    RouteDiscoveryHandler<BC> for CC
{
    fn update_route(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &CC::DeviceId,
        route: Ipv6DiscoveredRoute,
        lifetime: Option<NonZeroNdpLifetime>,
    ) {
        self.with_discovered_routes_mut(
            device_id,
            |Ipv6RouteDiscoveryState { routes }, core_ctx| {
                match lifetime {
                    Some(lifetime) => {
                        let newly_added = routes.insert(route.clone());
                        if newly_added {
                            match core_ctx.add_discovered_ipv6_route(bindings_ctx, device_id, route)
                            {
                                Ok(()) => (),
                                Err(crate::error::ExistsError) => {
                                    // If we fail to add the route to the route table,
                                    // remove it from our table of discovered routes and
                                    // do nothing further.
                                    let _: bool = routes.remove(&route);
                                    return;
                                }
                            }
                        }

                        let timer_id =
                            Ipv6DiscoveredRouteTimerId { device_id: device_id.clone(), route };
                        let prev_timer_fires_at: Option<BC::Instant> = match lifetime {
                            NonZeroNdpLifetime::Finite(lifetime) => {
                                bindings_ctx.schedule_timer(lifetime.get(), timer_id.clone())
                            }
                            // Routes with an infinite lifetime have no timers
                            //
                            // TODO(https://fxbug.dev/42180014): Hold timers scheduled to
                            // fire at infinity.
                            NonZeroNdpLifetime::Infinite => {
                                bindings_ctx.cancel_timer(timer_id.clone())
                            }
                        };

                        if newly_added {
                            if let Some(prev_timer_fires_at) = prev_timer_fires_at {
                                panic!(
                                    "newly added timer ID {:?} should not have already been \
                             scheduled to fire at {:?}",
                                    timer_id, prev_timer_fires_at,
                                )
                            }
                        }
                    }
                    None => {
                        if routes.remove(&route) {
                            invalidate_route(core_ctx, bindings_ctx, device_id, route);
                        }
                    }
                }
            },
        )
    }

    fn invalidate_routes(&mut self, bindings_ctx: &mut BC, device_id: &CC::DeviceId) {
        self.with_discovered_routes_mut(
            device_id,
            |Ipv6RouteDiscoveryState { routes }, core_ctx| {
                for route in core::mem::take(routes).into_iter() {
                    invalidate_route(core_ctx, bindings_ctx, device_id, route);
                }
            },
        )
    }
}

impl<BC: Ipv6RouteDiscoveryBindingsContext<CC::DeviceId>, CC: Ipv6RouteDiscoveryContext<BC>>
    TimerHandler<BC, Ipv6DiscoveredRouteTimerId<CC::DeviceId>> for CC
{
    fn handle_timer(
        &mut self,
        bindings_ctx: &mut BC,
        Ipv6DiscoveredRouteTimerId { device_id, route }: Ipv6DiscoveredRouteTimerId<CC::DeviceId>,
    ) {
        self.with_discovered_routes_mut(
            &device_id,
            |Ipv6RouteDiscoveryState { routes }, core_ctx| {
                assert!(routes.remove(&route), "invalidated route should be discovered");
                del_discovered_ipv6_route(core_ctx, bindings_ctx, &device_id, route);
            },
        )
    }
}

fn del_discovered_ipv6_route<
    BC: Ipv6RouteDiscoveryBindingsContext<CC::DeviceId>,
    CC: Ipv6DiscoveredRoutesContext<BC>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    route: Ipv6DiscoveredRoute,
) {
    core_ctx.del_discovered_ipv6_route(bindings_ctx, device_id, route);
}

fn invalidate_route<
    BC: Ipv6RouteDiscoveryBindingsContext<CC::DeviceId>,
    CC: Ipv6DiscoveredRoutesContext<BC>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    route: Ipv6DiscoveredRoute,
) {
    // Routes with an infinite lifetime have no timers.
    //
    // TODO(https://fxbug.dev/42180014): Hold timers scheduled to fire at infinity.
    let _: Option<BC::Instant> = bindings_ctx
        .cancel_timer(Ipv6DiscoveredRouteTimerId { device_id: device_id.clone(), route });
    del_discovered_ipv6_route(core_ctx, bindings_ctx, device_id, route)
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;
    use core::{convert::TryInto as _, time::Duration};

    use assert_matches::assert_matches;
    use net_types::{ip::Ipv6, Witness as _};
    use packet::{BufferMut, InnerPacketBuilder as _, Serializer as _};
    use packet_formats::{
        icmp::{
            ndp::{
                options::{NdpOptionBuilder, PrefixInformation, RouteInformation},
                OptionSequenceBuilder, RoutePreference, RouterAdvertisement,
            },
            IcmpPacketBuilder, IcmpUnusedCode,
        },
        ip::Ipv6Proto,
        ipv6::Ipv6PacketBuilder,
        utils::NonZeroDuration,
    };

    use super::*;
    use crate::{
        context::testutil::{
            FakeBindingsCtx, FakeCoreCtx, FakeCtx, FakeInstant, FakeTimerCtxExt as _,
        },
        device::{
            ethernet::{EthernetCreationProperties, EthernetLinkDevice},
            testutil::FakeDeviceId,
            DeviceId, FrameDestination,
        },
        ip::{
            device::{
                IpDeviceConfigurationUpdate, IpDeviceEvent, Ipv6DeviceConfigurationUpdate,
                Ipv6DeviceTimerId,
            },
            testutil::FakeIpDeviceIdCtx,
            types::{AddableEntry, AddableEntryEither, AddableMetric, Entry, Metric},
            IpLayerEvent, IPV6_DEFAULT_SUBNET,
        },
        testutil::{
            DispatchedEvent, FakeEventDispatcherConfig, TestIpExt as _, DEFAULT_INTERFACE_METRIC,
            IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
        },
        time::TimerIdInner,
        TimerId,
    };

    #[derive(Default)]
    struct FakeWithDiscoveredRoutesMutCtx {
        route_table: HashSet<Ipv6DiscoveredRoute>,
        ip_device_id_ctx: FakeIpDeviceIdCtx<FakeDeviceId>,
    }

    impl AsRef<FakeIpDeviceIdCtx<FakeDeviceId>> for FakeWithDiscoveredRoutesMutCtx {
        fn as_ref(&self) -> &FakeIpDeviceIdCtx<FakeDeviceId> {
            &self.ip_device_id_ctx
        }
    }

    impl DeviceIdContext<AnyDevice> for FakeWithDiscoveredRoutesMutCtx {
        type DeviceId = <FakeIpDeviceIdCtx<FakeDeviceId> as DeviceIdContext<AnyDevice>>::DeviceId;
        type WeakDeviceId =
            <FakeIpDeviceIdCtx<FakeDeviceId> as DeviceIdContext<AnyDevice>>::WeakDeviceId;
    }

    impl<C> Ipv6DiscoveredRoutesContext<C> for FakeWithDiscoveredRoutesMutCtx {
        fn add_discovered_ipv6_route(
            &mut self,
            _bindings_ctx: &mut C,
            FakeDeviceId: &Self::DeviceId,
            route: Ipv6DiscoveredRoute,
        ) -> Result<(), crate::error::ExistsError> {
            let Self { route_table, ip_device_id_ctx: _ } = self;
            let newly_inserted = route_table.insert(route);
            if newly_inserted {
                Ok(())
            } else {
                Err(crate::error::ExistsError)
            }
        }

        fn del_discovered_ipv6_route(
            &mut self,
            _bindings_ctx: &mut C,
            FakeDeviceId: &Self::DeviceId,
            route: Ipv6DiscoveredRoute,
        ) {
            let Self { route_table, ip_device_id_ctx: _ } = self;
            let _: bool = route_table.remove(&route);
        }
    }

    #[derive(Default)]
    struct FakeIpv6RouteDiscoveryContext {
        state: Ipv6RouteDiscoveryState,
        route_table: FakeWithDiscoveredRoutesMutCtx,
        ip_device_id_ctx: FakeIpDeviceIdCtx<FakeDeviceId>,
    }

    impl AsRef<FakeIpDeviceIdCtx<FakeDeviceId>> for FakeIpv6RouteDiscoveryContext {
        fn as_ref(&self) -> &FakeIpDeviceIdCtx<FakeDeviceId> {
            &self.ip_device_id_ctx
        }
    }

    type FakeCoreCtxImpl = FakeCoreCtx<FakeIpv6RouteDiscoveryContext, (), FakeDeviceId>;

    type FakeBindingsCtxImpl =
        FakeBindingsCtx<Ipv6DiscoveredRouteTimerId<FakeDeviceId>, DispatchedEvent, (), ()>;

    impl Ipv6RouteDiscoveryContext<FakeBindingsCtxImpl> for FakeCoreCtxImpl {
        type WithDiscoveredRoutesMutCtx<'a> = FakeWithDiscoveredRoutesMutCtx;

        fn with_discovered_routes_mut<
            F: FnOnce(&mut Ipv6RouteDiscoveryState, &mut Self::WithDiscoveredRoutesMutCtx<'_>),
        >(
            &mut self,
            &FakeDeviceId: &Self::DeviceId,
            cb: F,
        ) {
            let FakeIpv6RouteDiscoveryContext { state, route_table, ip_device_id_ctx: _ } =
                self.get_mut();
            cb(state, route_table)
        }
    }

    const ROUTE1: Ipv6DiscoveredRoute =
        Ipv6DiscoveredRoute { subnet: IPV6_DEFAULT_SUBNET, gateway: None };
    const ROUTE2: Ipv6DiscoveredRoute = Ipv6DiscoveredRoute {
        subnet: unsafe {
            Subnet::new_unchecked(Ipv6Addr::new([0x2620, 0x1012, 0x1000, 0x5000, 0, 0, 0, 0]), 64)
        },
        gateway: None,
    };

    const ONE_SECOND: NonZeroDuration =
        const_unwrap::const_unwrap_option(NonZeroDuration::from_secs(1));
    const TWO_SECONDS: NonZeroDuration =
        const_unwrap::const_unwrap_option(NonZeroDuration::from_secs(2));
    const THREE_SECONDS: NonZeroDuration =
        const_unwrap::const_unwrap_option(NonZeroDuration::from_secs(3));

    #[test]
    fn new_route_no_lifetime() {
        let FakeCtx { mut core_ctx, mut bindings_ctx } =
            FakeCtx::with_core_ctx(FakeCoreCtxImpl::default());

        RouteDiscoveryHandler::update_route(
            &mut core_ctx,
            &mut bindings_ctx,
            &FakeDeviceId,
            ROUTE1,
            None,
        );
        bindings_ctx.timer_ctx().assert_no_timers_installed();
    }

    fn discover_new_route(
        core_ctx: &mut FakeCoreCtxImpl,
        bindings_ctx: &mut FakeBindingsCtxImpl,
        route: Ipv6DiscoveredRoute,
        duration: NonZeroNdpLifetime,
    ) {
        RouteDiscoveryHandler::update_route(
            core_ctx,
            bindings_ctx,
            &FakeDeviceId,
            route,
            Some(duration),
        );

        let route_table = &core_ctx.get_ref().route_table.route_table;
        assert!(route_table.contains(&route), "route_table={route_table:?}");

        bindings_ctx.timer_ctx().assert_some_timers_installed(match duration {
            NonZeroNdpLifetime::Finite(duration) => Some((
                Ipv6DiscoveredRouteTimerId { device_id: FakeDeviceId, route },
                FakeInstant::from(duration.get()),
            )),
            NonZeroNdpLifetime::Infinite => None,
        })
    }

    fn trigger_next_timer(
        core_ctx: &mut FakeCoreCtxImpl,
        bindings_ctx: &mut FakeBindingsCtxImpl,
        route: Ipv6DiscoveredRoute,
    ) {
        assert_eq!(
            bindings_ctx.trigger_next_timer(core_ctx),
            Some(Ipv6DiscoveredRouteTimerId { device_id: FakeDeviceId, route })
        );
    }

    fn assert_route_invalidated(
        core_ctx: &mut FakeCoreCtxImpl,
        bindings_ctx: &mut FakeBindingsCtxImpl,
        route: Ipv6DiscoveredRoute,
    ) {
        let route_table = &core_ctx.get_ref().route_table.route_table;
        assert!(!route_table.contains(&route), "route_table={route_table:?}");
        bindings_ctx.timer_ctx().assert_no_timers_installed();
    }

    fn assert_single_invalidation_timer(
        core_ctx: &mut FakeCoreCtxImpl,
        bindings_ctx: &mut FakeBindingsCtxImpl,
        route: Ipv6DiscoveredRoute,
    ) {
        trigger_next_timer(core_ctx, bindings_ctx, route);
        assert_route_invalidated(core_ctx, bindings_ctx, route);
    }

    #[test]
    fn new_route_already_exists() {
        let FakeCtx { mut core_ctx, mut bindings_ctx } =
            FakeCtx::with_core_ctx(FakeCoreCtxImpl::default());

        // Fake the route already being present in the routing table.
        assert!(core_ctx.get_mut().route_table.route_table.insert(ROUTE1));

        // Clear events so we can assert on route-added events later.
        let _: Vec<crate::testutil::DispatchedEvent> = bindings_ctx.take_events();

        RouteDiscoveryHandler::update_route(
            &mut core_ctx,
            &mut bindings_ctx,
            &FakeDeviceId,
            ROUTE1,
            Some(NonZeroNdpLifetime::Finite(ONE_SECOND)),
        );

        // There should not be any add-route events dispatched.
        assert_eq!(bindings_ctx.take_events(), []);
        bindings_ctx.timer_ctx().assert_no_timers_installed();
    }

    #[test]
    fn invalidated_route_not_found() {
        let FakeCtx { mut core_ctx, mut bindings_ctx } =
            FakeCtx::with_core_ctx(FakeCoreCtxImpl::default());

        discover_new_route(&mut core_ctx, &mut bindings_ctx, ROUTE1, NonZeroNdpLifetime::Infinite);

        // Fake the route already being removed from underneath the route
        // discovery table.
        assert!(core_ctx.get_mut().route_table.route_table.remove(&ROUTE1));
        // Invalidating the route should ignore the fact that the route is not
        // in the route table.
        update_to_invalidate_check_invalidation(&mut core_ctx, &mut bindings_ctx, ROUTE1);
    }

    #[test]
    fn new_route_with_infinite_lifetime() {
        let FakeCtx { mut core_ctx, mut bindings_ctx } =
            FakeCtx::with_core_ctx(FakeCoreCtxImpl::default());

        discover_new_route(&mut core_ctx, &mut bindings_ctx, ROUTE1, NonZeroNdpLifetime::Infinite);
        bindings_ctx.timer_ctx().assert_no_timers_installed();
    }

    #[test]
    fn update_route_from_infinite_to_finite_lifetime() {
        let FakeCtx { mut core_ctx, mut bindings_ctx } =
            FakeCtx::with_core_ctx(FakeCoreCtxImpl::default());

        discover_new_route(&mut core_ctx, &mut bindings_ctx, ROUTE1, NonZeroNdpLifetime::Infinite);
        bindings_ctx.timer_ctx().assert_no_timers_installed();

        RouteDiscoveryHandler::update_route(
            &mut core_ctx,
            &mut bindings_ctx,
            &FakeDeviceId,
            ROUTE1,
            Some(NonZeroNdpLifetime::Finite(ONE_SECOND)),
        );
        bindings_ctx.timer_ctx().assert_some_timers_installed([(
            Ipv6DiscoveredRouteTimerId { device_id: FakeDeviceId, route: ROUTE1 },
            FakeInstant::from(ONE_SECOND.get()),
        )]);
        assert_single_invalidation_timer(&mut core_ctx, &mut bindings_ctx, ROUTE1);
    }

    fn update_to_invalidate_check_invalidation(
        core_ctx: &mut FakeCoreCtxImpl,
        bindings_ctx: &mut FakeBindingsCtxImpl,
        route: Ipv6DiscoveredRoute,
    ) {
        RouteDiscoveryHandler::update_route(core_ctx, bindings_ctx, &FakeDeviceId, ROUTE1, None);
        assert_route_invalidated(core_ctx, bindings_ctx, route);
    }

    #[test]
    fn invalidate_route_with_infinite_lifetime() {
        let FakeCtx { mut core_ctx, mut bindings_ctx } =
            FakeCtx::with_core_ctx(FakeCoreCtxImpl::default());

        discover_new_route(&mut core_ctx, &mut bindings_ctx, ROUTE1, NonZeroNdpLifetime::Infinite);
        bindings_ctx.timer_ctx().assert_no_timers_installed();

        update_to_invalidate_check_invalidation(&mut core_ctx, &mut bindings_ctx, ROUTE1);
    }
    #[test]
    fn new_route_with_finite_lifetime() {
        let FakeCtx { mut core_ctx, mut bindings_ctx } =
            FakeCtx::with_core_ctx(FakeCoreCtxImpl::default());

        discover_new_route(
            &mut core_ctx,
            &mut bindings_ctx,
            ROUTE1,
            NonZeroNdpLifetime::Finite(ONE_SECOND),
        );
        assert_single_invalidation_timer(&mut core_ctx, &mut bindings_ctx, ROUTE1);
    }

    #[test]
    fn update_route_from_finite_to_infinite_lifetime() {
        let FakeCtx { mut core_ctx, mut bindings_ctx } =
            FakeCtx::with_core_ctx(FakeCoreCtxImpl::default());

        discover_new_route(
            &mut core_ctx,
            &mut bindings_ctx,
            ROUTE1,
            NonZeroNdpLifetime::Finite(ONE_SECOND),
        );

        RouteDiscoveryHandler::update_route(
            &mut core_ctx,
            &mut bindings_ctx,
            &FakeDeviceId,
            ROUTE1,
            Some(NonZeroNdpLifetime::Infinite),
        );
        bindings_ctx.timer_ctx().assert_no_timers_installed();
    }

    #[test]
    fn update_route_from_finite_to_finite_lifetime() {
        let FakeCtx { mut core_ctx, mut bindings_ctx } =
            FakeCtx::with_core_ctx(FakeCoreCtxImpl::default());

        discover_new_route(
            &mut core_ctx,
            &mut bindings_ctx,
            ROUTE1,
            NonZeroNdpLifetime::Finite(ONE_SECOND),
        );

        RouteDiscoveryHandler::update_route(
            &mut core_ctx,
            &mut bindings_ctx,
            &FakeDeviceId,
            ROUTE1,
            Some(NonZeroNdpLifetime::Finite(TWO_SECONDS)),
        );
        bindings_ctx.timer_ctx().assert_timers_installed([(
            Ipv6DiscoveredRouteTimerId { device_id: FakeDeviceId, route: ROUTE1 },
            FakeInstant::from(TWO_SECONDS.get()),
        )]);

        assert_single_invalidation_timer(&mut core_ctx, &mut bindings_ctx, ROUTE1);
    }

    #[test]
    fn invalidate_route_with_finite_lifetime() {
        let FakeCtx { mut core_ctx, mut bindings_ctx } =
            FakeCtx::with_core_ctx(FakeCoreCtxImpl::default());

        discover_new_route(
            &mut core_ctx,
            &mut bindings_ctx,
            ROUTE1,
            NonZeroNdpLifetime::Finite(ONE_SECOND),
        );

        update_to_invalidate_check_invalidation(&mut core_ctx, &mut bindings_ctx, ROUTE1);
    }

    #[test]
    fn invalidate_all_routes() {
        let FakeCtx { mut core_ctx, mut bindings_ctx } =
            FakeCtx::with_core_ctx(FakeCoreCtxImpl::default());
        discover_new_route(
            &mut core_ctx,
            &mut bindings_ctx,
            ROUTE1,
            NonZeroNdpLifetime::Finite(ONE_SECOND),
        );
        discover_new_route(
            &mut core_ctx,
            &mut bindings_ctx,
            ROUTE2,
            NonZeroNdpLifetime::Finite(TWO_SECONDS),
        );

        RouteDiscoveryHandler::invalidate_routes(&mut core_ctx, &mut bindings_ctx, &FakeDeviceId);
        bindings_ctx.timer_ctx().assert_no_timers_installed();
        let route_table = &core_ctx.get_ref().route_table.route_table;
        assert!(route_table.is_empty(), "route_table={route_table:?}");
    }

    fn router_advertisement_buf(
        src_ip: LinkLocalUnicastAddr<Ipv6Addr>,
        router_lifetime_secs: u16,
        on_link_prefix: Subnet<Ipv6Addr>,
        on_link_prefix_flag: bool,
        on_link_prefix_valid_lifetime_secs: u32,
        more_specific_route: Option<(Subnet<Ipv6Addr>, u32)>,
    ) -> impl BufferMut {
        let src_ip: Ipv6Addr = src_ip.get();
        let dst_ip = Ipv6::ALL_NODES_LINK_LOCAL_MULTICAST_ADDRESS.get();
        let p = PrefixInformation::new(
            on_link_prefix.prefix(),
            on_link_prefix_flag,
            false, /* autonomous_address_configuration_flag */
            on_link_prefix_valid_lifetime_secs,
            0, /* preferred_lifetime */
            on_link_prefix.network(),
        );
        let more_specific_route_opt = more_specific_route.map(|(subnet, secs)| {
            NdpOptionBuilder::RouteInformation(RouteInformation::new(
                subnet,
                secs,
                RoutePreference::default(),
            ))
        });
        let options = [NdpOptionBuilder::PrefixInformation(p)];
        let options = options.iter().chain(more_specific_route_opt.as_ref());
        OptionSequenceBuilder::new(options)
            .into_serializer()
            .encapsulate(IcmpPacketBuilder::<Ipv6, _>::new(
                src_ip,
                dst_ip,
                IcmpUnusedCode,
                RouterAdvertisement::new(
                    0,     /* hop_limit */
                    false, /* managed_flag */
                    false, /* other_config_flag */
                    router_lifetime_secs,
                    0, /* reachable_time */
                    0, /* retransmit_timer */
                ),
            ))
            .encapsulate(Ipv6PacketBuilder::new(
                src_ip,
                dst_ip,
                crate::ip::icmp::REQUIRED_NDP_IP_PACKET_HOP_LIMIT,
                Ipv6Proto::Icmpv6,
            ))
            .serialize_vec_outer()
            .unwrap()
            .unwrap_b()
    }

    fn setup() -> (
        crate::testutil::FakeCtx,
        DeviceId<crate::testutil::FakeBindingsCtx>,
        FakeEventDispatcherConfig<Ipv6Addr>,
    ) {
        let FakeEventDispatcherConfig {
            local_mac,
            remote_mac: _,
            local_ip: _,
            remote_ip: _,
            subnet: _,
        } = Ipv6::FAKE_CONFIG;

        let mut ctx = crate::testutil::FakeCtx::default();
        let device_id = ctx
            .core_api()
            .device::<EthernetLinkDevice>()
            .add_device_with_default_state(
                EthernetCreationProperties {
                    mac: local_mac,
                    max_frame_size: IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
                },
                DEFAULT_INTERFACE_METRIC,
            )
            .into();
        let _: Ipv6DeviceConfigurationUpdate = ctx
            .core_api()
            .device_ip::<Ipv6>()
            .update_configuration(
                &device_id,
                Ipv6DeviceConfigurationUpdate {
                    ip_config: IpDeviceConfigurationUpdate {
                        ip_enabled: Some(true),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .unwrap();

        ctx.bindings_ctx.timer_ctx().assert_no_timers_installed();

        (ctx, device_id, Ipv6::FAKE_CONFIG)
    }

    fn as_secs(d: NonZeroDuration) -> u16 {
        d.get().as_secs().try_into().unwrap()
    }

    fn timer_id(
        route: Ipv6DiscoveredRoute,
        device_id: DeviceId<crate::testutil::FakeBindingsCtx>,
    ) -> TimerId<crate::testutil::FakeBindingsCtx> {
        TimerId(TimerIdInner::Ipv6Device(
            Ipv6DeviceTimerId::RouteDiscovery(Ipv6DiscoveredRouteTimerId { device_id, route })
                .into(),
        ))
    }

    const LINK_LOCAL_SUBNET: Subnet<Ipv6Addr> = net_declare::net_subnet_v6!("fe80::/64");

    fn add_link_local_route(
        ctx: &mut crate::testutil::FakeCtx,
        device: &DeviceId<crate::testutil::FakeBindingsCtx>,
    ) {
        ctx.test_api()
            .add_route(AddableEntryEither::from(AddableEntry::without_gateway(
                LINK_LOCAL_SUBNET,
                device.clone(),
                AddableMetric::MetricTracksInterface,
            )))
            .unwrap()
    }

    fn discovered_route_to_entry(
        device: &DeviceId<crate::testutil::FakeBindingsCtx>,
        Ipv6DiscoveredRoute { subnet, gateway }: Ipv6DiscoveredRoute,
    ) -> Entry<Ipv6Addr, DeviceId<crate::testutil::FakeBindingsCtx>> {
        Entry {
            subnet,
            device: device.clone(),
            gateway: gateway.map(|g| (*g).into_specified()),
            metric: Metric::MetricTracksInterface(DEFAULT_INTERFACE_METRIC),
        }
    }

    #[test]
    fn discovery_integration() {
        let (
            mut ctx,
            device_id,
            FakeEventDispatcherConfig {
                local_mac: _,
                remote_mac,
                local_ip: _,
                remote_ip: _,
                subnet,
            },
        ) = setup();

        add_link_local_route(&mut ctx, &device_id);

        let src_ip = remote_mac.to_ipv6_link_local().addr();

        let buf = |router_lifetime_secs,
                   on_link_prefix_flag,
                   prefix_valid_lifetime_secs,
                   more_specified_route_lifetime_secs| {
            router_advertisement_buf(
                src_ip,
                router_lifetime_secs,
                subnet,
                on_link_prefix_flag,
                prefix_valid_lifetime_secs,
                Some((subnet, more_specified_route_lifetime_secs)),
            )
        };

        let timer_id = |route| timer_id(route, device_id.clone());

        // Clear events so we can assert on route-added events later.
        let _: Vec<crate::testutil::DispatchedEvent> = ctx.bindings_ctx.take_events();

        // Do nothing as router with no valid lifetime has not been discovered
        // yet and prefix does not make on-link determination.
        ctx.test_api().receive_ip_packet::<Ipv6, _>(
            &device_id,
            Some(FrameDestination::Individual { local: true }),
            buf(0, false, as_secs(ONE_SECOND).into(), 0),
        );
        ctx.bindings_ctx.timer_ctx().assert_no_timers_installed();

        // Discover a default router only as on-link prefix has no valid
        // lifetime.
        ctx.test_api().receive_ip_packet::<Ipv6, _>(
            &device_id,
            Some(FrameDestination::Individual { local: true }),
            buf(as_secs(ONE_SECOND), true, 0, 0),
        );
        let gateway_route =
            Ipv6DiscoveredRoute { subnet: IPV6_DEFAULT_SUBNET, gateway: Some(src_ip) };
        ctx.bindings_ctx.timer_ctx().assert_timers_installed([(
            timer_id(gateway_route),
            FakeInstant::from(ONE_SECOND.get()),
        )]);

        let gateway_route_entry = discovered_route_to_entry(&device_id, gateway_route);
        assert_eq!(
            ctx.bindings_ctx.take_events(),
            [crate::testutil::DispatchedEvent::IpLayerIpv6(IpLayerEvent::AddRoute(
                gateway_route_entry.clone().map_device_id(|d| d.downgrade()).into(),
            ))]
        );

        // Discover an on-link prefix and update valid lifetime for default
        // router.
        ctx.test_api().receive_ip_packet::<Ipv6, _>(
            &device_id,
            Some(FrameDestination::Individual { local: true }),
            buf(as_secs(TWO_SECONDS), true, as_secs(ONE_SECOND).into(), 0),
        );
        let on_link_route = Ipv6DiscoveredRoute { subnet, gateway: None };
        let on_link_route_entry = discovered_route_to_entry(&device_id, on_link_route);
        ctx.bindings_ctx.timer_ctx().assert_timers_installed([
            (timer_id(gateway_route), FakeInstant::from(TWO_SECONDS.get())),
            (timer_id(on_link_route), FakeInstant::from(ONE_SECOND.get())),
        ]);

        assert_eq!(
            ctx.bindings_ctx.take_events(),
            [crate::testutil::DispatchedEvent::IpLayerIpv6(IpLayerEvent::AddRoute(
                on_link_route_entry.clone().map_device_id(|d| d.downgrade()).into(),
            ))]
        );

        // Discover more-specific route.
        ctx.test_api().receive_ip_packet::<Ipv6, _>(
            &device_id,
            Some(FrameDestination::Individual { local: true }),
            buf(
                as_secs(TWO_SECONDS),
                true,
                as_secs(ONE_SECOND).into(),
                as_secs(THREE_SECONDS).into(),
            ),
        );
        let more_specific_route = Ipv6DiscoveredRoute { subnet, gateway: Some(src_ip) };
        let more_specific_route_entry = discovered_route_to_entry(&device_id, more_specific_route);
        ctx.bindings_ctx.timer_ctx().assert_timers_installed([
            (timer_id(gateway_route), FakeInstant::from(TWO_SECONDS.get())),
            (timer_id(on_link_route), FakeInstant::from(ONE_SECOND.get())),
            (timer_id(more_specific_route), FakeInstant::from(THREE_SECONDS.get())),
        ]);

        assert_eq!(
            ctx.bindings_ctx.take_events(),
            [crate::testutil::DispatchedEvent::IpLayerIpv6(IpLayerEvent::AddRoute(
                more_specific_route_entry.clone().map_device_id(|d| d.downgrade()).into(),
            ))]
        );

        // Invalidate default router and more specific route, and update valid
        // lifetime for on-link prefix.
        ctx.test_api().receive_ip_packet::<Ipv6, _>(
            &device_id,
            Some(FrameDestination::Individual { local: true }),
            buf(0, true, as_secs(TWO_SECONDS).into(), 0),
        );
        ctx.bindings_ctx.timer_ctx().assert_timers_installed([(
            timer_id(on_link_route),
            FakeInstant::from(TWO_SECONDS.get()),
        )]);
        {
            let crate::ip::types::Entry { subnet, device, gateway, metric: _ } =
                gateway_route_entry;
            let event1 = IpLayerEvent::RemoveRoutes { subnet, device: device.downgrade(), gateway };
            let crate::ip::types::Entry { subnet, device, gateway, metric: _ } =
                more_specific_route_entry;
            let event2 = IpLayerEvent::RemoveRoutes { subnet, device: device.downgrade(), gateway };
            let events = ctx.bindings_ctx.take_events();
            assert_eq!(events.len(), 2);
            assert!(events.contains(&crate::testutil::DispatchedEvent::IpLayerIpv6(event1)));
            assert!(events.contains(&crate::testutil::DispatchedEvent::IpLayerIpv6(event2)));
        }

        // Do nothing as prefix does not make on-link determination and router
        // with valid lifetime is not discovered.
        ctx.test_api().receive_ip_packet::<Ipv6, _>(
            &device_id,
            Some(FrameDestination::Individual { local: true }),
            buf(0, false, 0, 0),
        );
        ctx.bindings_ctx.timer_ctx().assert_timers_installed([(
            timer_id(on_link_route),
            FakeInstant::from(TWO_SECONDS.get()),
        )]);
        assert_eq!(ctx.bindings_ctx.take_events(), []);

        // Invalidate on-link prefix.
        ctx.test_api().receive_ip_packet::<Ipv6, _>(
            &device_id,
            Some(FrameDestination::Individual { local: true }),
            buf(0, true, 0, 0),
        );
        ctx.bindings_ctx.timer_ctx().assert_no_timers_installed();
        {
            let crate::ip::types::Entry { subnet, device, gateway, metric: _ } =
                on_link_route_entry;
            assert_eq!(
                ctx.bindings_ctx.take_events(),
                [crate::testutil::DispatchedEvent::IpLayerIpv6(IpLayerEvent::RemoveRoutes {
                    subnet,
                    device: device.downgrade(),
                    gateway
                }),]
            );
        }
    }

    #[test]
    fn discovery_integration_infinite_to_finite_to_infinite_lifetime() {
        let (
            mut ctx,
            device_id,
            FakeEventDispatcherConfig {
                local_mac: _,
                remote_mac,
                local_ip: _,
                remote_ip: _,
                subnet,
            },
        ) = setup();

        add_link_local_route(&mut ctx, &device_id);

        let src_ip = remote_mac.to_ipv6_link_local().addr();

        let buf = |router_lifetime_secs, on_link_prefix_flag, prefix_valid_lifetime_secs| {
            router_advertisement_buf(
                src_ip,
                router_lifetime_secs,
                subnet,
                on_link_prefix_flag,
                prefix_valid_lifetime_secs,
                None,
            )
        };

        let timer_id = |route| timer_id(route, device_id.clone());

        let gateway_route =
            Ipv6DiscoveredRoute { subnet: IPV6_DEFAULT_SUBNET, gateway: Some(src_ip) };
        let gateway_route_entry = discovered_route_to_entry(&device_id, gateway_route);
        let on_link_route = Ipv6DiscoveredRoute { subnet, gateway: None };
        let on_link_route_entry = discovered_route_to_entry(&device_id, on_link_route);

        // Clear events so we can assert on route-added events later.
        let _: Vec<crate::testutil::DispatchedEvent> = ctx.bindings_ctx.take_events();

        // Router with finite lifetime and on-link prefix with infinite
        // lifetime.
        let router_lifetime_secs = u16::MAX;
        let prefix_lifetime_secs = u32::MAX;
        ctx.test_api().receive_ip_packet::<Ipv6, _>(
            &device_id,
            Some(FrameDestination::Individual { local: true }),
            buf(router_lifetime_secs, true, prefix_lifetime_secs),
        );
        ctx.bindings_ctx.timer_ctx().assert_timers_installed([(
            timer_id(gateway_route),
            FakeInstant::from(Duration::from_secs(router_lifetime_secs.into())),
        )]);
        assert_eq!(
            ctx.bindings_ctx.take_events(),
            [
                crate::testutil::DispatchedEvent::IpLayerIpv6(IpLayerEvent::AddRoute(
                    gateway_route_entry.clone().map_device_id(|d| d.downgrade()).into(),
                )),
                crate::testutil::DispatchedEvent::IpLayerIpv6(IpLayerEvent::AddRoute(
                    on_link_route_entry.clone().map_device_id(|d| d.downgrade()).into(),
                )),
            ]
        );

        // Router and prefix with finite lifetimes.
        let router_lifetime_secs = u16::MAX - 1;
        let prefix_lifetime_secs = u32::MAX - 1;
        ctx.test_api().receive_ip_packet::<Ipv6, _>(
            &device_id,
            Some(FrameDestination::Individual { local: true }),
            buf(router_lifetime_secs, true, prefix_lifetime_secs),
        );
        ctx.bindings_ctx.timer_ctx().assert_timers_installed([
            (
                timer_id(gateway_route),
                FakeInstant::from(Duration::from_secs(router_lifetime_secs.into())),
            ),
            (
                timer_id(on_link_route),
                FakeInstant::from(Duration::from_secs(prefix_lifetime_secs.into())),
            ),
        ]);
        assert_eq!(ctx.bindings_ctx.take_events(), []);

        // Router with finite lifetime and on-link prefix with infinite
        // lifetime.
        let router_lifetime_secs = u16::MAX;
        let prefix_lifetime_secs = u32::MAX;
        ctx.test_api().receive_ip_packet::<Ipv6, _>(
            &device_id,
            Some(FrameDestination::Individual { local: true }),
            buf(router_lifetime_secs, true, prefix_lifetime_secs),
        );
        ctx.bindings_ctx.timer_ctx().assert_timers_installed([(
            timer_id(gateway_route),
            FakeInstant::from(Duration::from_secs(router_lifetime_secs.into())),
        )]);
        assert_eq!(ctx.bindings_ctx.take_events(), []);

        // Router and prefix invalidated.
        let router_lifetime_secs = 0;
        let prefix_lifetime_secs = 0;
        ctx.test_api().receive_ip_packet::<Ipv6, _>(
            &device_id,
            Some(FrameDestination::Individual { local: true }),
            buf(router_lifetime_secs, true, prefix_lifetime_secs),
        );
        ctx.bindings_ctx.timer_ctx().assert_no_timers_installed();

        {
            let crate::ip::types::Entry { subnet, device, gateway, metric: _ } =
                gateway_route_entry;
            let event1 = IpLayerEvent::RemoveRoutes { subnet, device: device.downgrade(), gateway };
            let crate::ip::types::Entry { subnet, device, gateway, metric: _ } =
                on_link_route_entry;
            let event2 = IpLayerEvent::RemoveRoutes { subnet, device: device.downgrade(), gateway };
            assert_eq!(
                ctx.bindings_ctx.take_events(),
                [
                    crate::testutil::DispatchedEvent::IpLayerIpv6(event1),
                    crate::testutil::DispatchedEvent::IpLayerIpv6(event2)
                ]
            );
        }
    }

    #[test]
    fn flush_routes_on_interface_disabled_integration() {
        let (
            mut ctx,
            device_id,
            FakeEventDispatcherConfig {
                local_mac: _,
                remote_mac,
                local_ip: _,
                remote_ip: _,
                subnet,
            },
        ) = setup();
        add_link_local_route(&mut ctx, &device_id);

        let src_ip = remote_mac.to_ipv6_link_local().addr();
        let gateway_route =
            Ipv6DiscoveredRoute { subnet: IPV6_DEFAULT_SUBNET, gateway: Some(src_ip) };
        let gateway_route_entry = discovered_route_to_entry(&device_id, gateway_route);
        let on_link_route = Ipv6DiscoveredRoute { subnet, gateway: None };
        let on_link_route_entry = discovered_route_to_entry(&device_id, on_link_route);

        // Clear events so we can assert on route-added events later.
        let _: Vec<crate::testutil::DispatchedEvent> = ctx.bindings_ctx.take_events();

        let timer_id = |route| timer_id(route, device_id.clone());

        // Discover both an on-link prefix and default router.
        ctx.test_api().receive_ip_packet::<Ipv6, _>(
            &device_id,
            Some(FrameDestination::Individual { local: true }),
            router_advertisement_buf(
                src_ip,
                as_secs(TWO_SECONDS),
                subnet,
                true,
                as_secs(ONE_SECOND).into(),
                None,
            ),
        );
        ctx.bindings_ctx.timer_ctx().assert_timers_installed([
            (timer_id(gateway_route), FakeInstant::from(TWO_SECONDS.get())),
            (timer_id(on_link_route), FakeInstant::from(ONE_SECOND.get())),
        ]);
        assert_eq!(
            ctx.bindings_ctx.take_events(),
            [
                crate::testutil::DispatchedEvent::IpLayerIpv6(IpLayerEvent::AddRoute(
                    gateway_route_entry.clone().map_device_id(|d| d.downgrade()).into(),
                )),
                crate::testutil::DispatchedEvent::IpLayerIpv6(IpLayerEvent::AddRoute(
                    on_link_route_entry.clone().map_device_id(|d| d.downgrade()).into(),
                )),
            ]
        );

        // Disable the interface.
        let _: Ipv6DeviceConfigurationUpdate = ctx
            .core_api()
            .device_ip::<Ipv6>()
            .update_configuration(
                &device_id,
                Ipv6DeviceConfigurationUpdate {
                    ip_config: IpDeviceConfigurationUpdate {
                        ip_enabled: Some(false),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .unwrap();
        ctx.bindings_ctx.timer_ctx().assert_no_timers_installed();

        {
            let crate::ip::types::Entry { subnet, device, gateway, metric: _ } =
                gateway_route_entry;
            let event1 = IpLayerEvent::RemoveRoutes { subnet, device: device.downgrade(), gateway };
            let crate::ip::types::Entry { subnet, device, gateway, metric: _ } =
                on_link_route_entry;
            let event2 = IpLayerEvent::RemoveRoutes { subnet, device: device.downgrade(), gateway };
            let events = ctx.bindings_ctx.take_events();
            let (a, b, c) = assert_matches!(&events[..], [a, b, c] => (a, b, c));
            assert!([a, b].contains(&&crate::testutil::DispatchedEvent::IpLayerIpv6(event1)));
            assert!([a, b].contains(&&crate::testutil::DispatchedEvent::IpLayerIpv6(event2)));
            assert_eq!(
                c,
                &crate::testutil::DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::EnabledChanged {
                    device: device.downgrade(),
                    ip_enabled: false
                })
            );
        }
    }
}
