// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use alloc::collections::HashMap;
use alloc::vec::Vec;
use core::convert::TryInto as _;
use core::time::Duration;

use assert_matches::assert_matches;
use net_types::ip::{Ipv6, Ipv6Addr, Subnet};
use net_types::{LinkLocalUnicastAddr, Witness as _};
use packet::{BufferMut, InnerPacketBuilder as _, Serializer as _};
use packet_formats::icmp::ndp::options::{NdpOptionBuilder, PrefixInformation, RouteInformation};
use packet_formats::icmp::ndp::{OptionSequenceBuilder, RoutePreference, RouterAdvertisement};
use packet_formats::icmp::{IcmpPacketBuilder, IcmpZeroCode};
use packet_formats::ip::Ipv6Proto;
use packet_formats::ipv6::Ipv6PacketBuilder;
use packet_formats::utils::NonZeroDuration;

use netstack3_base::testutil::{FakeInstant, TestAddrs, TestIpExt as _};
use netstack3_base::FrameDestination;
use netstack3_core::device::{DeviceId, EthernetCreationProperties, EthernetLinkDevice};
use netstack3_core::testutil::{
    CtxPairExt as _, DispatchedEvent, FakeBindingsCtx, FakeCtx, DEFAULT_INTERFACE_METRIC,
};
use netstack3_device::testutil::IPV6_MIN_IMPLIED_MAX_FRAME_SIZE;
use netstack3_ip::device::{
    IpDeviceBindingsContext, IpDeviceConfigurationUpdate, IpDeviceEvent,
    Ipv6DeviceConfigurationContext, Ipv6DeviceConfigurationUpdate, Ipv6DiscoveredRoute,
    Ipv6RouteDiscoveryBindingsContext, Ipv6RouteDiscoveryContext,
};
use netstack3_ip::{
    self as ip, AddableEntry, AddableEntryEither, AddableMetric, Entry, IpLayerEvent, Metric,
    IPV6_DEFAULT_SUBNET,
};

const ONE_SECOND: NonZeroDuration = NonZeroDuration::from_secs(1).unwrap();
const TWO_SECONDS: NonZeroDuration = NonZeroDuration::from_secs(2).unwrap();
const THREE_SECONDS: NonZeroDuration = NonZeroDuration::from_secs(3).unwrap();

fn setup() -> (FakeCtx, DeviceId<FakeBindingsCtx>, TestAddrs<Ipv6Addr>) {
    let TestAddrs { local_mac, remote_mac: _, local_ip: _, remote_ip: _, subnet: _ } =
        Ipv6::TEST_ADDRS;

    let mut ctx = FakeCtx::default();
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

    assert_timers_integration(&mut ctx.core_ctx(), &device_id, []);

    (ctx, device_id, Ipv6::TEST_ADDRS)
}

fn as_secs(d: NonZeroDuration) -> u16 {
    d.get().as_secs().try_into().unwrap()
}

const LINK_LOCAL_SUBNET: Subnet<Ipv6Addr> = net_declare::net_subnet_v6!("fe80::/64");

fn add_link_local_route(ctx: &mut FakeCtx, device: &DeviceId<FakeBindingsCtx>) {
    ctx.test_api()
        .add_route(AddableEntryEither::from(AddableEntry::without_gateway(
            LINK_LOCAL_SUBNET,
            device.clone(),
            AddableMetric::MetricTracksInterface,
        )))
        .unwrap()
}

fn discovered_route_to_entry(
    device: &DeviceId<FakeBindingsCtx>,
    Ipv6DiscoveredRoute { subnet, gateway }: Ipv6DiscoveredRoute,
) -> Entry<Ipv6Addr, DeviceId<FakeBindingsCtx>> {
    Entry {
        subnet,
        device: device.clone(),
        gateway: gateway.map(|g| (*g).into_specified()),
        metric: Metric::MetricTracksInterface(DEFAULT_INTERFACE_METRIC),
    }
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
            IcmpZeroCode,
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
            ip::icmp::REQUIRED_NDP_IP_PACKET_HOP_LIMIT,
            Ipv6Proto::Icmpv6,
        ))
        .serialize_vec_outer()
        .unwrap()
        .unwrap_b()
}

// Assert internal timers in integration tests by going through the contexts
// to get the state.
#[track_caller]
fn assert_timers_integration<CC, BC, I>(core_ctx: &mut CC, device_id: &CC::DeviceId, timers: I)
where
    CC: Ipv6DeviceConfigurationContext<BC>,
    for<'a> CC::Ipv6DeviceStateCtx<'a>: Ipv6RouteDiscoveryContext<BC>,
    BC: IpDeviceBindingsContext<Ipv6, CC::DeviceId> + Ipv6RouteDiscoveryBindingsContext,
    I: IntoIterator<Item = (Ipv6DiscoveredRoute, BC::Instant)>,
{
    let want = timers.into_iter().collect::<HashMap<_, _>>();
    let got = core_ctx.with_ipv6_device_configuration(device_id, |_, mut core_ctx| {
        core_ctx.with_discovered_routes_mut(device_id, |state, _| {
            state.timers().iter().map(|(k, (), t)| (*k, *t)).collect::<HashMap<_, _>>()
        })
    });
    assert_eq!(got, want);
}

#[test]
fn discovery_integration() {
    let (
        mut ctx,
        device_id,
        TestAddrs { local_mac: _, remote_mac, local_ip: _, remote_ip: _, subnet },
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

    // Clear events so we can assert on route-added events later.
    let _: Vec<DispatchedEvent> = ctx.bindings_ctx.take_events();

    // Do nothing as router with no valid lifetime has not been discovered
    // yet and prefix does not make on-link determination.
    ctx.test_api().receive_ip_packet::<Ipv6, _>(
        &device_id,
        Some(FrameDestination::Individual { local: true }),
        buf(0, false, as_secs(ONE_SECOND).into(), 0),
    );
    assert_timers_integration(&mut ctx.core_ctx(), &device_id, []);

    // Discover a default router only as on-link prefix has no valid
    // lifetime.
    ctx.test_api().receive_ip_packet::<Ipv6, _>(
        &device_id,
        Some(FrameDestination::Individual { local: true }),
        buf(as_secs(ONE_SECOND), true, 0, 0),
    );
    let gateway_route = Ipv6DiscoveredRoute { subnet: IPV6_DEFAULT_SUBNET, gateway: Some(src_ip) };
    assert_timers_integration(
        &mut ctx.core_ctx(),
        &device_id,
        [(gateway_route, FakeInstant::from(ONE_SECOND.get()))],
    );

    let gateway_route_entry = discovered_route_to_entry(&device_id, gateway_route);
    assert_eq!(
        ctx.bindings_ctx.take_events(),
        [DispatchedEvent::IpLayerIpv6(IpLayerEvent::AddRoute(
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

    assert_timers_integration(
        &mut ctx.core_ctx(),
        &device_id,
        [
            (gateway_route, FakeInstant::from(TWO_SECONDS.get())),
            (on_link_route, FakeInstant::from(ONE_SECOND.get())),
        ],
    );
    assert_eq!(
        ctx.bindings_ctx.take_events(),
        [DispatchedEvent::IpLayerIpv6(IpLayerEvent::AddRoute(
            on_link_route_entry.clone().map_device_id(|d| d.downgrade()).into(),
        ))]
    );

    // Discover more-specific route.
    ctx.test_api().receive_ip_packet::<Ipv6, _>(
        &device_id,
        Some(FrameDestination::Individual { local: true }),
        buf(as_secs(TWO_SECONDS), true, as_secs(ONE_SECOND).into(), as_secs(THREE_SECONDS).into()),
    );
    let more_specific_route = Ipv6DiscoveredRoute { subnet, gateway: Some(src_ip) };
    let more_specific_route_entry = discovered_route_to_entry(&device_id, more_specific_route);
    assert_timers_integration(
        &mut ctx.core_ctx(),
        &device_id,
        [
            (gateway_route, FakeInstant::from(TWO_SECONDS.get())),
            (on_link_route, FakeInstant::from(ONE_SECOND.get())),
            (more_specific_route, FakeInstant::from(THREE_SECONDS.get())),
        ],
    );

    assert_eq!(
        ctx.bindings_ctx.take_events(),
        [DispatchedEvent::IpLayerIpv6(IpLayerEvent::AddRoute(
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
    assert_timers_integration(
        &mut ctx.core_ctx(),
        &device_id,
        [(on_link_route, FakeInstant::from(TWO_SECONDS.get()))],
    );
    {
        let ip::Entry { subnet, device, gateway, metric: _ } = gateway_route_entry;
        let event1 = IpLayerEvent::RemoveRoutes { subnet, device: device.downgrade(), gateway };
        let ip::Entry { subnet, device, gateway, metric: _ } = more_specific_route_entry;
        let event2 = IpLayerEvent::RemoveRoutes { subnet, device: device.downgrade(), gateway };
        let events = ctx.bindings_ctx.take_events();
        assert_eq!(events.len(), 2);
        assert!(events.contains(&DispatchedEvent::IpLayerIpv6(event1)));
        assert!(events.contains(&DispatchedEvent::IpLayerIpv6(event2)));
    }

    // Do nothing as prefix does not make on-link determination and router
    // with valid lifetime is not discovered.
    ctx.test_api().receive_ip_packet::<Ipv6, _>(
        &device_id,
        Some(FrameDestination::Individual { local: true }),
        buf(0, false, 0, 0),
    );
    assert_timers_integration(
        &mut ctx.core_ctx(),
        &device_id,
        [(on_link_route, FakeInstant::from(TWO_SECONDS.get()))],
    );
    assert_eq!(ctx.bindings_ctx.take_events(), []);

    // Invalidate on-link prefix.
    ctx.test_api().receive_ip_packet::<Ipv6, _>(
        &device_id,
        Some(FrameDestination::Individual { local: true }),
        buf(0, true, 0, 0),
    );
    assert_timers_integration(&mut ctx.core_ctx(), &device_id, []);
    {
        let ip::Entry { subnet, device, gateway, metric: _ } = on_link_route_entry;
        assert_eq!(
            ctx.bindings_ctx.take_events(),
            [DispatchedEvent::IpLayerIpv6(IpLayerEvent::RemoveRoutes {
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
        TestAddrs { local_mac: _, remote_mac, local_ip: _, remote_ip: _, subnet },
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

    let gateway_route = Ipv6DiscoveredRoute { subnet: IPV6_DEFAULT_SUBNET, gateway: Some(src_ip) };
    let gateway_route_entry = discovered_route_to_entry(&device_id, gateway_route);
    let on_link_route = Ipv6DiscoveredRoute { subnet, gateway: None };
    let on_link_route_entry = discovered_route_to_entry(&device_id, on_link_route);

    // Clear events so we can assert on route-added events later.
    let _: Vec<DispatchedEvent> = ctx.bindings_ctx.take_events();

    // Router with finite lifetime and on-link prefix with infinite
    // lifetime.
    let router_lifetime_secs = u16::MAX;
    let prefix_lifetime_secs = u32::MAX;
    ctx.test_api().receive_ip_packet::<Ipv6, _>(
        &device_id,
        Some(FrameDestination::Individual { local: true }),
        buf(router_lifetime_secs, true, prefix_lifetime_secs),
    );
    assert_timers_integration(
        &mut ctx.core_ctx(),
        &device_id,
        [(gateway_route, FakeInstant::from(Duration::from_secs(router_lifetime_secs.into())))],
    );
    assert_eq!(
        ctx.bindings_ctx.take_events(),
        [
            DispatchedEvent::IpLayerIpv6(IpLayerEvent::AddRoute(
                gateway_route_entry.clone().map_device_id(|d| d.downgrade()).into(),
            )),
            DispatchedEvent::IpLayerIpv6(IpLayerEvent::AddRoute(
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
    assert_timers_integration(
        &mut ctx.core_ctx(),
        &device_id,
        [
            (gateway_route, FakeInstant::from(Duration::from_secs(router_lifetime_secs.into()))),
            (on_link_route, FakeInstant::from(Duration::from_secs(prefix_lifetime_secs.into()))),
        ],
    );
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
    assert_timers_integration(
        &mut ctx.core_ctx(),
        &device_id,
        [(gateway_route, FakeInstant::from(Duration::from_secs(router_lifetime_secs.into())))],
    );
    assert_eq!(ctx.bindings_ctx.take_events(), []);

    // Router and prefix invalidated.
    let router_lifetime_secs = 0;
    let prefix_lifetime_secs = 0;
    ctx.test_api().receive_ip_packet::<Ipv6, _>(
        &device_id,
        Some(FrameDestination::Individual { local: true }),
        buf(router_lifetime_secs, true, prefix_lifetime_secs),
    );
    assert_timers_integration(&mut ctx.core_ctx(), &device_id, []);

    {
        let ip::Entry { subnet, device, gateway, metric: _ } = gateway_route_entry;
        let event1 = IpLayerEvent::RemoveRoutes { subnet, device: device.downgrade(), gateway };
        let ip::Entry { subnet, device, gateway, metric: _ } = on_link_route_entry;
        let event2 = IpLayerEvent::RemoveRoutes { subnet, device: device.downgrade(), gateway };
        assert_eq!(
            ctx.bindings_ctx.take_events(),
            [DispatchedEvent::IpLayerIpv6(event1), DispatchedEvent::IpLayerIpv6(event2)]
        );
    }
}

#[test]
fn flush_routes_on_interface_disabled_integration() {
    let (
        mut ctx,
        device_id,
        TestAddrs { local_mac: _, remote_mac, local_ip: _, remote_ip: _, subnet },
    ) = setup();
    add_link_local_route(&mut ctx, &device_id);

    let src_ip = remote_mac.to_ipv6_link_local().addr();
    let gateway_route = Ipv6DiscoveredRoute { subnet: IPV6_DEFAULT_SUBNET, gateway: Some(src_ip) };
    let gateway_route_entry = discovered_route_to_entry(&device_id, gateway_route);
    let on_link_route = Ipv6DiscoveredRoute { subnet, gateway: None };
    let on_link_route_entry = discovered_route_to_entry(&device_id, on_link_route);

    // Clear events so we can assert on route-added events later.
    let _: Vec<DispatchedEvent> = ctx.bindings_ctx.take_events();

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
    assert_timers_integration(
        &mut ctx.core_ctx(),
        &device_id,
        [
            (gateway_route, FakeInstant::from(TWO_SECONDS.get())),
            (on_link_route, FakeInstant::from(ONE_SECOND.get())),
        ],
    );
    assert_eq!(
        ctx.bindings_ctx.take_events(),
        [
            DispatchedEvent::IpLayerIpv6(IpLayerEvent::AddRoute(
                gateway_route_entry.clone().map_device_id(|d| d.downgrade()).into(),
            )),
            DispatchedEvent::IpLayerIpv6(IpLayerEvent::AddRoute(
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
    assert_timers_integration(&mut ctx.core_ctx(), &device_id, []);

    {
        let ip::Entry { subnet, device, gateway, metric: _ } = gateway_route_entry;
        let event1 = IpLayerEvent::RemoveRoutes { subnet, device: device.downgrade(), gateway };
        let ip::Entry { subnet, device, gateway, metric: _ } = on_link_route_entry;
        let event2 = IpLayerEvent::RemoveRoutes { subnet, device: device.downgrade(), gateway };
        let events = ctx.bindings_ctx.take_events();
        let (a, b, c) = assert_matches!(&events[..], [a, b, c] => (a, b, c));
        assert!([a, b].contains(&&DispatchedEvent::IpLayerIpv6(event1)));
        assert!([a, b].contains(&&DispatchedEvent::IpLayerIpv6(event2)));
        assert_eq!(
            c,
            &DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::EnabledChanged {
                device: device.downgrade(),
                ip_enabled: false
            })
        );
    }
}
