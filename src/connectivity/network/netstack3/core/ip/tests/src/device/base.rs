// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use alloc::collections::HashSet;
use alloc::vec;
use core::num::{NonZeroU16, NonZeroU8};
use core::time::Duration;

use assert_matches::assert_matches;
use ip_test_macro::ip_test;
use net_declare::{net_ip_v4, net_ip_v6, net_mac};
use net_types::ip::{AddrSubnet, Ip, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr, Mtu};
use net_types::{LinkLocalAddr, SpecifiedAddr, UnicastAddr, Witness};
use test_case::test_case;

use netstack3_base::testutil::{assert_empty, FakeInstant, TestIpExt};
use netstack3_base::{InstantContext as _, IpAddressId as _};
use netstack3_core::device::{
    DeviceId, EthernetCreationProperties, EthernetLinkDevice, LoopbackCreationProperties,
    LoopbackDevice, MaxEthernetFrameSize,
};
use netstack3_core::testutil::{
    Ctx, CtxPairExt as _, DispatchedEvent, FakeBindingsCtx, FakeCtx, DEFAULT_INTERFACE_METRIC,
};
use netstack3_core::{IpExt, StackStateBuilder, TimerId};
use netstack3_device::testutil::IPV6_MIN_IMPLIED_MAX_FRAME_SIZE;
use netstack3_ip::device::{
    AddIpAddrSubnetError, AddressRemovedReason, CommonAddressProperties, DadTimerId,
    IpAddressState, IpDeviceConfiguration, IpDeviceConfigurationUpdate, IpDeviceEvent,
    IpDeviceFlags, IpDeviceStateContext, Ipv4DeviceConfigurationUpdate,
    Ipv6DeviceConfigurationUpdate, Ipv6DeviceHandler, Ipv6DeviceTimerId, Lifetime,
    PreferredLifetime, RsTimerId, SetIpAddressPropertiesError, SlaacConfigurationUpdate,
    TemporarySlaacAddressConfiguration, UpdateIpConfigurationError,
};
use netstack3_ip::gmp::{IgmpConfigMode, MldConfigMode, MldTimerId};
use netstack3_ip::nud::{self, LinkResolutionResult};

#[test]
fn enable_disable_ipv4() {
    let mut ctx = FakeCtx::new_with_builder(StackStateBuilder::default());

    let local_mac = Ipv4::TEST_ADDRS.local_mac;
    let ethernet_device_id =
        ctx.core_api().device::<EthernetLinkDevice>().add_device_with_default_state(
            EthernetCreationProperties {
                mac: local_mac,
                max_frame_size: MaxEthernetFrameSize::from_mtu(Ipv4::MINIMUM_LINK_MTU).unwrap(),
            },
            DEFAULT_INTERFACE_METRIC,
        );
    let device_id = ethernet_device_id.clone().into();

    assert_eq!(ctx.bindings_ctx.take_events()[..], []);

    let set_ipv4_enabled = |ctx: &mut FakeCtx, enabled, expected_prev| {
        assert_eq!(
            ctx.test_api().set_ip_device_enabled::<Ipv4>(&device_id, enabled),
            expected_prev
        );
    };

    set_ipv4_enabled(&mut ctx, true, false);

    let weak_device_id = device_id.downgrade();
    assert_eq!(
        ctx.bindings_ctx.take_events()[..],
        [DispatchedEvent::IpDeviceIpv4(IpDeviceEvent::EnabledChanged {
            device: weak_device_id.clone(),
            ip_enabled: true,
        })]
    );

    let addr = SpecifiedAddr::new(net_ip_v4!("192.0.2.1")).expect("addr should be unspecified");
    let mac = net_mac!("02:23:45:67:89:ab");

    ctx.core_api()
        .neighbor::<Ipv4, EthernetLinkDevice>()
        .insert_static_entry(&ethernet_device_id, *addr, mac)
        .unwrap();
    assert_eq!(
        ctx.bindings_ctx.take_events(),
        [DispatchedEvent::NeighborIpv4(nud::Event {
            device: ethernet_device_id.downgrade(),
            addr,
            kind: nud::EventKind::Added(nud::EventState::Static(mac)),
            at: ctx.bindings_ctx.now(),
        })]
    );
    assert_matches!(
        ctx.core_api().neighbor::<Ipv4, EthernetLinkDevice>().resolve_link_addr(
            &ethernet_device_id,
            &addr,
        ),
        LinkResolutionResult::Resolved(got) => assert_eq!(got, mac)
    );

    set_ipv4_enabled(&mut ctx, false, true);
    assert_eq!(
        ctx.bindings_ctx.take_events(),
        [
            DispatchedEvent::NeighborIpv4(nud::Event {
                device: ethernet_device_id.downgrade(),
                addr,
                kind: nud::EventKind::Removed,
                at: ctx.bindings_ctx.now(),
            }),
            DispatchedEvent::IpDeviceIpv4(IpDeviceEvent::EnabledChanged {
                device: weak_device_id.clone(),
                ip_enabled: false,
            })
        ]
    );

    // Assert that static ARP entries are flushed on link down.
    nud::testutil::assert_neighbor_unknown::<Ipv4, _, _, _>(
        &mut ctx.core_ctx(),
        ethernet_device_id,
        addr,
    );

    let ipv4_addr_subnet = AddrSubnet::new(Ipv4Addr::new([192, 168, 0, 1]), 24).unwrap();
    ctx.core_api()
        .device_ip::<Ipv4>()
        .add_ip_addr_subnet(&device_id, ipv4_addr_subnet.clone())
        .expect("failed to add IPv4 Address");
    assert_eq!(
        ctx.bindings_ctx.take_events()[..],
        [DispatchedEvent::IpDeviceIpv4(IpDeviceEvent::AddressAdded {
            device: weak_device_id.clone(),
            addr: ipv4_addr_subnet.clone(),
            state: IpAddressState::Unavailable,
            valid_until: Lifetime::Infinite,
            preferred_lifetime: PreferredLifetime::preferred_forever(),
        })]
    );

    set_ipv4_enabled(&mut ctx, true, false);
    assert_eq!(
        ctx.bindings_ctx.take_events()[..],
        [
            DispatchedEvent::IpDeviceIpv4(IpDeviceEvent::AddressStateChanged {
                device: weak_device_id.clone(),
                addr: ipv4_addr_subnet.addr(),
                state: IpAddressState::Assigned,
            }),
            DispatchedEvent::IpDeviceIpv4(IpDeviceEvent::EnabledChanged {
                device: weak_device_id.clone(),
                ip_enabled: true,
            }),
        ]
    );
    // Verify that a redundant "enable" does not generate any events.
    set_ipv4_enabled(&mut ctx, true, true);
    assert_eq!(ctx.bindings_ctx.take_events()[..], []);

    let valid_until = Lifetime::Finite(FakeInstant::from(Duration::from_secs(2)));
    let preferred_lifetime =
        PreferredLifetime::preferred_until(FakeInstant::from(Duration::from_secs(1)));
    let properties = CommonAddressProperties { valid_until, preferred_lifetime };

    ctx.core_api()
        .device_ip::<Ipv4>()
        .set_addr_properties(&device_id, ipv4_addr_subnet.addr(), properties)
        .expect("set properties should succeed");
    assert_eq!(
        ctx.bindings_ctx.take_events()[..],
        [DispatchedEvent::IpDeviceIpv4(IpDeviceEvent::AddressPropertiesChanged {
            device: weak_device_id.clone(),
            addr: ipv4_addr_subnet.addr(),
            valid_until,
            preferred_lifetime
        })]
    );

    // Verify that a redundant "set properties" does not generate any events.
    ctx.core_api()
        .device_ip::<Ipv4>()
        .set_addr_properties(&device_id, ipv4_addr_subnet.addr(), properties)
        .expect("set properties should succeed");
    assert_eq!(ctx.bindings_ctx.take_events()[..], []);

    set_ipv4_enabled(&mut ctx, false, true);
    assert_eq!(
        ctx.bindings_ctx.take_events()[..],
        [
            DispatchedEvent::IpDeviceIpv4(IpDeviceEvent::AddressStateChanged {
                device: weak_device_id.clone(),
                addr: ipv4_addr_subnet.addr(),
                state: IpAddressState::Unavailable,
            }),
            DispatchedEvent::IpDeviceIpv4(IpDeviceEvent::EnabledChanged {
                device: weak_device_id,
                ip_enabled: false,
            }),
        ]
    );
    // Verify that a redundant "disable" does not generate any events.
    set_ipv4_enabled(&mut ctx, false, false);
    assert_eq!(ctx.bindings_ctx.take_events()[..], []);
}

fn enable_ipv6_device(
    ctx: &mut FakeCtx,
    device_id: &DeviceId<FakeBindingsCtx>,
    ll_addr: AddrSubnet<Ipv6Addr, LinkLocalAddr<UnicastAddr<Ipv6Addr>>>,
    expected_prev: bool,
) {
    assert_eq!(ctx.test_api().set_ip_device_enabled::<Ipv6>(device_id, true), expected_prev);

    assert_eq!(
        IpDeviceStateContext::<Ipv6, _>::with_address_ids(
            &mut ctx.core_ctx(),
            device_id,
            |addrs, _core_ctx| {
                addrs.map(|addr_id| addr_id.addr_sub().addr().get()).collect::<HashSet<_>>()
            }
        ),
        HashSet::from([ll_addr.ipv6_unicast_addr()]),
        "enabled device expected to generate link-local address"
    );
}

#[test]
fn enable_disable_ipv6() {
    let mut ctx = FakeCtx::new_with_builder(StackStateBuilder::default());
    ctx.bindings_ctx.timer_ctx().assert_no_timers_installed();
    let local_mac = Ipv6::TEST_ADDRS.local_mac;
    let ethernet_device_id =
        ctx.core_api().device::<EthernetLinkDevice>().add_device_with_default_state(
            EthernetCreationProperties {
                mac: local_mac,
                max_frame_size: IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
            },
            DEFAULT_INTERFACE_METRIC,
        );
    let device_id = ethernet_device_id.clone().into();
    let ll_addr = local_mac.to_ipv6_link_local();
    let _: Ipv6DeviceConfigurationUpdate = ctx
        .core_api()
        .device_ip::<Ipv6>()
        .update_configuration(
            &device_id,
            Ipv6DeviceConfigurationUpdate {
                // Doesn't matter as long as we perform DAD and router
                // solicitation.
                dad_transmits: Some(NonZeroU16::new(1)),
                max_router_solicitations: Some(NonZeroU8::new(1)),
                // Auto-generate a link-local address.
                slaac_config: SlaacConfigurationUpdate {
                    enable_stable_addresses: Some(true),
                    ..Default::default()
                },
                ip_config: IpDeviceConfigurationUpdate {
                    gmp_enabled: Some(true),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
    ctx.bindings_ctx.timer_ctx().assert_no_timers_installed();
    assert_eq!(ctx.bindings_ctx.take_events()[..], []);

    // Enable the device and observe an auto-generated link-local address,
    // router solicitation and DAD for the auto-generated address.
    let test_enable_device = |ctx: &mut FakeCtx, expected_prev| {
        enable_ipv6_device(ctx, &device_id, ll_addr, expected_prev);
        let timers = vec![
            (
                TimerId::from(
                    Ipv6DeviceTimerId::Rs(RsTimerId::new(device_id.downgrade())).into_common(),
                ),
                ..,
            ),
            (
                TimerId::from(
                    Ipv6DeviceTimerId::Dad(DadTimerId::new(
                        device_id.downgrade(),
                        IpDeviceStateContext::<Ipv6, _>::get_address_id(
                            &mut ctx.core_ctx(),
                            &device_id,
                            ll_addr.ipv6_unicast_addr().into(),
                        )
                        .unwrap()
                        .downgrade(),
                    ))
                    .into_common(),
                ),
                ..,
            ),
            (
                TimerId::from(
                    Ipv6DeviceTimerId::Mld(MldTimerId::new(device_id.downgrade())).into_common(),
                ),
                ..,
            ),
        ];
        ctx.bindings_ctx.timer_ctx().assert_timers_installed_range(timers);
    };
    test_enable_device(&mut ctx, false);
    let weak_device_id = device_id.downgrade();
    assert_eq!(
        ctx.bindings_ctx.take_events()[..],
        [
            DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::AddressAdded {
                device: weak_device_id.clone(),
                addr: ll_addr.to_witness(),
                state: IpAddressState::Tentative,
                valid_until: Lifetime::Infinite,
                preferred_lifetime: PreferredLifetime::preferred_forever(),
            }),
            DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::EnabledChanged {
                device: weak_device_id.clone(),
                ip_enabled: true,
            })
        ]
    );

    // Because the added address is from SLAAC, setting properties should fail.
    assert_matches!(
        ctx.core_api().device_ip::<Ipv6>().set_addr_properties(
            &device_id,
            ll_addr.addr().into(),
            CommonAddressProperties::default(),
        ),
        Err(SetIpAddressPropertiesError::NotManual)
    );

    let addr = SpecifiedAddr::new(net_ip_v6!("2001:db8::1")).expect("addr should be unspecified");
    let mac = net_mac!("02:23:45:67:89:ab");
    ctx.core_api()
        .neighbor::<Ipv6, _>()
        .insert_static_entry(&ethernet_device_id, *addr, mac)
        .unwrap();
    assert_eq!(
        ctx.bindings_ctx.take_events(),
        [DispatchedEvent::NeighborIpv6(nud::Event {
            device: ethernet_device_id.downgrade(),
            addr,
            kind: nud::EventKind::Added(nud::EventState::Static(mac)),
            at: ctx.bindings_ctx.now(),
        })]
    );
    assert_matches!(
        ctx.core_api().neighbor::<Ipv6, _>().resolve_link_addr(
            &ethernet_device_id,
            &addr,
        ),
        LinkResolutionResult::Resolved(got) => assert_eq!(got, mac)
    );

    let test_disable_device = |ctx: &mut FakeCtx, expected_prev| {
        assert_eq!(ctx.test_api().set_ip_device_enabled::<Ipv6>(&device_id, false), expected_prev);
        ctx.bindings_ctx.timer_ctx().assert_no_timers_installed();
    };
    test_disable_device(&mut ctx, true);
    assert_eq!(
        ctx.bindings_ctx.take_events(),
        [
            DispatchedEvent::NeighborIpv6(nud::Event {
                device: ethernet_device_id.downgrade(),
                addr,
                kind: nud::EventKind::Removed,
                at: ctx.bindings_ctx.now(),
            }),
            DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::AddressRemoved {
                device: weak_device_id.clone(),
                addr: ll_addr.addr().into(),
                reason: AddressRemovedReason::Manual,
            }),
            DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::EnabledChanged {
                device: weak_device_id.clone(),
                ip_enabled: false,
            })
        ]
    );

    let mut core_ctx = ctx.core_ctx();
    let core_ctx = &mut core_ctx;
    IpDeviceStateContext::<Ipv6, _>::with_address_ids(core_ctx, &device_id, |addrs, _core_ctx| {
        assert_empty(addrs);
    });

    // Assert that static NDP entry was removed on link down.
    nud::testutil::assert_neighbor_unknown::<Ipv6, _, _, _>(core_ctx, ethernet_device_id, addr);

    ctx.core_api()
        .device_ip::<Ipv6>()
        .add_ip_addr_subnet(&device_id, ll_addr.replace_witness().unwrap())
        .expect("add MAC based IPv6 link-local address");
    assert_eq!(
        IpDeviceStateContext::<Ipv6, _>::with_address_ids(
            &mut ctx.core_ctx(),
            &device_id,
            |addrs, _core_ctx| {
                addrs.map(|addr_id| addr_id.addr_sub().addr().get()).collect::<HashSet<_>>()
            }
        ),
        HashSet::from([ll_addr.ipv6_unicast_addr()])
    );
    assert_eq!(
        ctx.bindings_ctx.take_events()[..],
        [DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::AddressAdded {
            device: weak_device_id.clone(),
            addr: ll_addr.to_witness(),
            state: IpAddressState::Unavailable,
            valid_until: Lifetime::Infinite,
            preferred_lifetime: PreferredLifetime::preferred_forever(),
        })]
    );

    test_enable_device(&mut ctx, false);
    assert_eq!(
        ctx.bindings_ctx.take_events()[..],
        [
            DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::AddressStateChanged {
                device: weak_device_id.clone(),
                addr: ll_addr.addr().into(),
                state: IpAddressState::Tentative,
            }),
            DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::EnabledChanged {
                device: weak_device_id.clone(),
                ip_enabled: true,
            })
        ]
    );

    let valid_until = Lifetime::Finite(FakeInstant::from(Duration::from_secs(2)));
    let preferred_lifetime =
        PreferredLifetime::preferred_until(FakeInstant::from(Duration::from_secs(1)));
    let properties = CommonAddressProperties { valid_until, preferred_lifetime };
    ctx.core_api()
        .device_ip::<Ipv6>()
        .set_addr_properties(&device_id, ll_addr.addr().into(), properties)
        .expect("set properties should succeed");
    assert_eq!(
        ctx.bindings_ctx.take_events()[..],
        [DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::AddressPropertiesChanged {
            device: weak_device_id.clone(),
            addr: ll_addr.addr().into(),
            valid_until,
            preferred_lifetime,
        })]
    );

    // Verify that a redundant "set properties" does not generate any events.
    ctx.core_api()
        .device_ip::<Ipv6>()
        .set_addr_properties(&device_id, ll_addr.addr().into(), properties)
        .expect("set properties should succeed");
    assert_eq!(ctx.bindings_ctx.take_events()[..], []);

    test_disable_device(&mut ctx, true);
    // The address was manually added, don't expect it to be removed.
    assert_eq!(
        ctx.bindings_ctx.take_events()[..],
        [
            DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::AddressStateChanged {
                device: weak_device_id.clone(),
                addr: ll_addr.addr().into(),
                state: IpAddressState::Unavailable,
            }),
            DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::EnabledChanged {
                device: weak_device_id.clone(),
                ip_enabled: false,
            })
        ]
    );

    // Verify that a redundant "disable" does not generate any events.
    test_disable_device(&mut ctx, false);

    let (mut core_ctx, bindings_ctx) = ctx.contexts();
    let core_ctx = &mut core_ctx;
    assert_eq!(bindings_ctx.take_events()[..], []);

    assert_eq!(
        IpDeviceStateContext::<Ipv6, _>::with_address_ids(
            core_ctx,
            &device_id,
            |addrs, _core_ctx| {
                addrs.map(|addr_id| addr_id.addr_sub().addr().get()).collect::<HashSet<_>>()
            }
        ),
        HashSet::from([ll_addr.ipv6_unicast_addr()]),
        "manual addresses should not be removed on device disable"
    );

    test_enable_device(&mut ctx, false);
    assert_eq!(
        ctx.bindings_ctx.take_events()[..],
        [
            DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::AddressStateChanged {
                device: weak_device_id.clone(),
                addr: ll_addr.addr().into(),
                state: IpAddressState::Tentative,
            }),
            DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::EnabledChanged {
                device: weak_device_id,
                ip_enabled: true,
            })
        ]
    );

    // Verify that a redundant "enable" does not generate any events.
    test_enable_device(&mut ctx, true);
    assert_eq!(ctx.bindings_ctx.take_events()[..], []);

    // Disable device again so timers are cancelled.
    test_disable_device(&mut ctx, true);
    let _ = ctx.bindings_ctx.take_events();
}

#[test]
fn notify_on_dad_failure_ipv6() {
    let mut ctx = FakeCtx::new_with_builder(StackStateBuilder::default());

    ctx.bindings_ctx.timer_ctx().assert_no_timers_installed();
    let local_mac = Ipv6::TEST_ADDRS.local_mac;
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
                dad_transmits: Some(NonZeroU16::new(1)),
                max_router_solicitations: Some(NonZeroU8::new(1)),
                // Auto-generate a link-local address.
                slaac_config: SlaacConfigurationUpdate {
                    enable_stable_addresses: Some(true),
                    ..Default::default()
                },
                ip_config: IpDeviceConfigurationUpdate {
                    gmp_enabled: Some(true),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
    ctx.bindings_ctx.timer_ctx().assert_no_timers_installed();
    let ll_addr = local_mac.to_ipv6_link_local();

    enable_ipv6_device(&mut ctx, &device_id, ll_addr, false);
    let weak_device_id = device_id.downgrade();
    assert_eq!(
        &ctx.bindings_ctx.take_events()[..],
        [
            DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::AddressAdded {
                device: weak_device_id.clone(),
                addr: ll_addr.to_witness(),
                state: IpAddressState::Tentative,
                valid_until: Lifetime::Infinite,
                preferred_lifetime: PreferredLifetime::preferred_forever(),
            }),
            DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::EnabledChanged {
                device: weak_device_id.clone(),
                ip_enabled: true,
            }),
        ]
    );

    let assigned_addr = AddrSubnet::new(net_ip_v6!("fe80::1"), 64).unwrap();
    ctx.core_api()
        .device_ip::<Ipv6>()
        .add_ip_addr_subnet(&device_id, assigned_addr)
        .expect("add succeeds");
    let Ctx { core_ctx, bindings_ctx } = &mut ctx;
    assert_eq!(
        bindings_ctx.take_events()[..],
        [DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::AddressAdded {
            device: weak_device_id.clone(),
            addr: assigned_addr.to_witness(),
            state: IpAddressState::Tentative,
            valid_until: Lifetime::Infinite,
            preferred_lifetime: PreferredLifetime::preferred_forever(),
        }),]
    );

    // When DAD fails, an event should be emitted and the address should be
    // removed.
    assert_eq!(
        Ipv6DeviceHandler::handle_received_neighbor_advertisement(
            &mut core_ctx.context(),
            bindings_ctx,
            &device_id,
            assigned_addr.ipv6_unicast_addr(),
        ),
        IpAddressState::Tentative,
    );

    assert_eq!(
        bindings_ctx.take_events()[..],
        [DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::AddressRemoved {
            device: weak_device_id,
            addr: assigned_addr.addr(),
            reason: AddressRemovedReason::DadFailed,
        }),]
    );

    assert_eq!(
        IpDeviceStateContext::<Ipv6, _>::with_address_ids(
            &mut core_ctx.context(),
            &device_id,
            |addrs, _core_ctx| {
                addrs.map(|addr_id| addr_id.addr_sub().addr().get()).collect::<HashSet<_>>()
            }
        ),
        HashSet::from([ll_addr.ipv6_unicast_addr()]),
        "manual addresses should be removed on DAD failure"
    );
    // Disable device and take all events to cleanup references.
    assert_eq!(ctx.test_api().set_ip_device_enabled::<Ipv6>(&device_id, false), true);
    let _ = ctx.bindings_ctx.take_events();
}

#[netstack3_macros::context_ip_bounds(I, FakeBindingsCtx)]
#[ip_test(I)]
fn update_ip_device_configuration_err<I: IpExt>() {
    let mut ctx = FakeCtx::default();

    let loopback_device_id = ctx
        .core_api()
        .device::<LoopbackDevice>()
        .add_device_with_default_state(
            LoopbackCreationProperties { mtu: Mtu::new(u16::MAX as u32) },
            DEFAULT_INTERFACE_METRIC,
        )
        .into();

    let mut api = ctx.core_api().device_ip::<I>();
    let original_state = api.get_configuration(&loopback_device_id);
    assert_eq!(
        api.update_configuration(
            &loopback_device_id,
            IpDeviceConfigurationUpdate {
                ip_enabled: Some(!AsRef::<IpDeviceFlags>::as_ref(&original_state).ip_enabled),
                gmp_enabled: Some(
                    !AsRef::<IpDeviceConfiguration>::as_ref(&original_state).gmp_enabled
                ),
                unicast_forwarding_enabled: Some(true),
                multicast_forwarding_enabled: None,
            }
            .into(),
        )
        .unwrap_err(),
        UpdateIpConfigurationError::UnicastForwardingNotSupported,
    );
    assert_eq!(original_state, api.get_configuration(&loopback_device_id));
}

#[test]
fn update_ipv4_configuration_return() {
    let mut ctx = FakeCtx::new_with_builder(StackStateBuilder::default());
    ctx.bindings_ctx.timer_ctx().assert_no_timers_installed();
    let local_mac = Ipv4::TEST_ADDRS.local_mac;
    let device_id = ctx
        .core_api()
        .device::<EthernetLinkDevice>()
        .add_device_with_default_state(
            EthernetCreationProperties {
                mac: local_mac,
                max_frame_size: MaxEthernetFrameSize::from_mtu(Ipv4::MINIMUM_LINK_MTU).unwrap(),
            },
            DEFAULT_INTERFACE_METRIC,
        )
        .into();

    let mut api = ctx.core_api().device_ip::<Ipv4>();
    // Perform no update.
    assert_eq!(
        api.update_configuration(&device_id, Ipv4DeviceConfigurationUpdate::default()),
        Ok(Ipv4DeviceConfigurationUpdate::default()),
    );

    // Enable all but forwarding. All features are initially disabled.
    assert_eq!(
        api.update_configuration(
            &device_id,
            Ipv4DeviceConfigurationUpdate {
                ip_config: IpDeviceConfigurationUpdate {
                    ip_enabled: Some(true),
                    unicast_forwarding_enabled: Some(false),
                    multicast_forwarding_enabled: Some(false),
                    gmp_enabled: Some(true),
                },
                igmp_mode: Some(IgmpConfigMode::V1),
            },
        ),
        Ok(Ipv4DeviceConfigurationUpdate {
            ip_config: IpDeviceConfigurationUpdate {
                ip_enabled: Some(false),
                unicast_forwarding_enabled: Some(false),
                multicast_forwarding_enabled: Some(false),
                gmp_enabled: Some(false),
            },
            igmp_mode: Some(IgmpConfigMode::V2),
        }),
    );

    // Change forwarding config.
    assert_eq!(
        api.update_configuration(
            &device_id,
            Ipv4DeviceConfigurationUpdate {
                ip_config: IpDeviceConfigurationUpdate {
                    ip_enabled: Some(true),
                    unicast_forwarding_enabled: Some(true),
                    multicast_forwarding_enabled: Some(true),
                    gmp_enabled: None,
                },
                igmp_mode: None,
            },
        ),
        Ok(Ipv4DeviceConfigurationUpdate {
            ip_config: IpDeviceConfigurationUpdate {
                ip_enabled: Some(true),
                unicast_forwarding_enabled: Some(false),
                multicast_forwarding_enabled: Some(false),
                gmp_enabled: None,
            },
            igmp_mode: None,
        }),
    );

    // No update to anything (GMP enabled set to already set
    // value).
    assert_eq!(
        api.update_configuration(
            &device_id,
            Ipv4DeviceConfigurationUpdate {
                ip_config: IpDeviceConfigurationUpdate {
                    ip_enabled: None,
                    unicast_forwarding_enabled: None,
                    multicast_forwarding_enabled: None,
                    gmp_enabled: Some(true),
                },
                igmp_mode: None,
            },
        ),
        Ok(Ipv4DeviceConfigurationUpdate {
            ip_config: IpDeviceConfigurationUpdate {
                ip_enabled: None,
                unicast_forwarding_enabled: None,
                multicast_forwarding_enabled: None,
                gmp_enabled: Some(true),
            },
            igmp_mode: None,
        }),
    );

    // Disable/change everything.
    assert_eq!(
        api.update_configuration(
            &device_id,
            Ipv4DeviceConfigurationUpdate {
                ip_config: IpDeviceConfigurationUpdate {
                    ip_enabled: Some(false),
                    unicast_forwarding_enabled: Some(false),
                    multicast_forwarding_enabled: Some(false),
                    gmp_enabled: Some(false),
                },
                igmp_mode: Some(IgmpConfigMode::V3),
            },
        ),
        Ok(Ipv4DeviceConfigurationUpdate {
            ip_config: IpDeviceConfigurationUpdate {
                ip_enabled: Some(true),
                unicast_forwarding_enabled: Some(true),
                multicast_forwarding_enabled: Some(true),
                gmp_enabled: Some(true),
            },
            igmp_mode: Some(IgmpConfigMode::V1),
        }),
    );
}

#[test]
fn update_ipv6_configuration_return() {
    let mut ctx = FakeCtx::new_with_builder(StackStateBuilder::default());
    ctx.bindings_ctx.timer_ctx().assert_no_timers_installed();
    let local_mac = Ipv6::TEST_ADDRS.local_mac;
    let device_id = ctx
        .core_api()
        .device::<EthernetLinkDevice>()
        .add_device_with_default_state(
            EthernetCreationProperties {
                mac: local_mac,
                max_frame_size: MaxEthernetFrameSize::from_mtu(Ipv6::MINIMUM_LINK_MTU).unwrap(),
            },
            DEFAULT_INTERFACE_METRIC,
        )
        .into();

    let mut api = ctx.core_api().device_ip::<Ipv6>();

    // Perform no update.
    assert_eq!(
        api.update_configuration(&device_id, Ipv6DeviceConfigurationUpdate::default()),
        Ok(Ipv6DeviceConfigurationUpdate::default()),
    );

    // Enable all but forwarding. All features are initially disabled.
    assert_eq!(
        api.update_configuration(
            &device_id,
            Ipv6DeviceConfigurationUpdate {
                dad_transmits: Some(NonZeroU16::new(1)),
                max_router_solicitations: Some(NonZeroU8::new(2)),
                slaac_config: SlaacConfigurationUpdate {
                    enable_stable_addresses: Some(true),
                    temporary_address_configuration: Some(
                        TemporarySlaacAddressConfiguration::enabled_with_rfc_defaults()
                    ),
                },
                ip_config: IpDeviceConfigurationUpdate {
                    ip_enabled: Some(true),
                    unicast_forwarding_enabled: Some(false),
                    multicast_forwarding_enabled: Some(false),
                    gmp_enabled: Some(true),
                },
                mld_mode: Some(MldConfigMode::V2),
            },
        ),
        Ok(Ipv6DeviceConfigurationUpdate {
            dad_transmits: Some(None),
            max_router_solicitations: Some(None),
            slaac_config: SlaacConfigurationUpdate {
                enable_stable_addresses: Some(false),
                temporary_address_configuration: Some(TemporarySlaacAddressConfiguration::Disabled),
            },
            ip_config: IpDeviceConfigurationUpdate {
                ip_enabled: Some(false),
                unicast_forwarding_enabled: Some(false),
                multicast_forwarding_enabled: Some(false),
                gmp_enabled: Some(false),
            },
            mld_mode: Some(MldConfigMode::V1),
        }),
    );

    // Change forwarding config.
    assert_eq!(
        api.update_configuration(
            &device_id,
            Ipv6DeviceConfigurationUpdate {
                dad_transmits: None,
                max_router_solicitations: None,
                slaac_config: SlaacConfigurationUpdate::default(),
                ip_config: IpDeviceConfigurationUpdate {
                    ip_enabled: Some(true),
                    unicast_forwarding_enabled: Some(true),
                    multicast_forwarding_enabled: Some(true),
                    gmp_enabled: None,
                },
                mld_mode: None,
            },
        ),
        Ok(Ipv6DeviceConfigurationUpdate {
            dad_transmits: None,
            max_router_solicitations: None,
            slaac_config: SlaacConfigurationUpdate::default(),
            ip_config: IpDeviceConfigurationUpdate {
                ip_enabled: Some(true),
                unicast_forwarding_enabled: Some(false),
                multicast_forwarding_enabled: Some(false),
                gmp_enabled: None,
            },
            mld_mode: None,
        }),
    );

    // No update to anything (GMP enabled set to already set
    // value).
    assert_eq!(
        api.update_configuration(
            &device_id,
            Ipv6DeviceConfigurationUpdate {
                dad_transmits: None,
                max_router_solicitations: None,
                slaac_config: SlaacConfigurationUpdate::default(),
                ip_config: IpDeviceConfigurationUpdate {
                    ip_enabled: None,
                    unicast_forwarding_enabled: None,
                    multicast_forwarding_enabled: None,
                    gmp_enabled: Some(true),
                },
                mld_mode: None,
            },
        ),
        Ok(Ipv6DeviceConfigurationUpdate {
            dad_transmits: None,
            max_router_solicitations: None,
            slaac_config: SlaacConfigurationUpdate::default(),
            ip_config: IpDeviceConfigurationUpdate {
                ip_enabled: None,
                unicast_forwarding_enabled: None,
                multicast_forwarding_enabled: None,
                gmp_enabled: Some(true),
            },
            mld_mode: None,
        }),
    );

    // Disable/change everything.
    assert_eq!(
        api.update_configuration(
            &device_id,
            Ipv6DeviceConfigurationUpdate {
                dad_transmits: Some(None),
                max_router_solicitations: Some(None),
                slaac_config: SlaacConfigurationUpdate {
                    enable_stable_addresses: Some(false),
                    temporary_address_configuration: Some(
                        TemporarySlaacAddressConfiguration::Disabled
                    ),
                },
                ip_config: IpDeviceConfigurationUpdate {
                    ip_enabled: Some(false),
                    unicast_forwarding_enabled: Some(false),
                    multicast_forwarding_enabled: Some(false),
                    gmp_enabled: Some(false),
                },
                mld_mode: Some(MldConfigMode::V1),
            },
        ),
        Ok(Ipv6DeviceConfigurationUpdate {
            dad_transmits: Some(NonZeroU16::new(1)),
            max_router_solicitations: Some(NonZeroU8::new(2)),
            slaac_config: SlaacConfigurationUpdate {
                enable_stable_addresses: Some(true),
                temporary_address_configuration: Some(
                    TemporarySlaacAddressConfiguration::enabled_with_rfc_defaults()
                ),
            },
            ip_config: IpDeviceConfigurationUpdate {
                ip_enabled: Some(true),
                unicast_forwarding_enabled: Some(true),
                multicast_forwarding_enabled: Some(true),
                gmp_enabled: Some(true),
            },
            mld_mode: Some(MldConfigMode::V2),
        }),
    );
}

#[test_case(false; "stable addresses enabled generates link local")]
#[test_case(true; "stable addresses disabled does not generate link local")]
fn configure_link_local_address_generation(enable_stable_addresses: bool) {
    let mut ctx = FakeCtx::new_with_builder(StackStateBuilder::default());
    ctx.bindings_ctx.timer_ctx().assert_no_timers_installed();
    let local_mac = Ipv6::TEST_ADDRS.local_mac;
    let device_id = ctx
        .core_api()
        .device::<EthernetLinkDevice>()
        .add_device_with_default_state(
            EthernetCreationProperties {
                mac: local_mac,
                max_frame_size: MaxEthernetFrameSize::from_mtu(Ipv4::MINIMUM_LINK_MTU).unwrap(),
            },
            DEFAULT_INTERFACE_METRIC,
        )
        .into();

    let new_config = Ipv6DeviceConfigurationUpdate {
        slaac_config: SlaacConfigurationUpdate {
            enable_stable_addresses: Some(enable_stable_addresses),
            ..Default::default()
        },
        ..Default::default()
    };

    let _prev: Ipv6DeviceConfigurationUpdate =
        ctx.core_api().device_ip::<Ipv6>().update_configuration(&device_id, new_config).unwrap();
    assert_eq!(ctx.test_api().set_ip_device_enabled::<Ipv6>(&device_id, true), false);

    let expected_addrs = if enable_stable_addresses {
        HashSet::from([local_mac.to_ipv6_link_local().ipv6_unicast_addr()])
    } else {
        HashSet::new()
    };

    assert_eq!(
        IpDeviceStateContext::<Ipv6, _>::with_address_ids(
            &mut ctx.core_ctx(),
            &device_id,
            |addrs, _core_ctx| {
                addrs.map(|addr_id| addr_id.addr_sub().addr().get()).collect::<HashSet<_>>()
            }
        ),
        expected_addrs,
    );
}

#[netstack3_macros::context_ip_bounds(I, FakeBindingsCtx)]
#[ip_test(I)]
fn disallow_loopback_addrs_on_non_loopback_interface<I: IpExt + TestIpExt>() {
    let mut ctx = FakeCtx::new_with_builder(StackStateBuilder::default());
    let device_id = ctx
        .core_api()
        .device::<EthernetLinkDevice>()
        .add_device_with_default_state(
            EthernetCreationProperties {
                mac: I::TEST_ADDRS.local_mac,
                max_frame_size: IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
            },
            DEFAULT_INTERFACE_METRIC,
        )
        .into();
    let result = ctx.core_api().device_ip::<I>().add_ip_addr_subnet(
        &device_id,
        AddrSubnet::new(I::LOOPBACK_ADDRESS.get(), I::LOOPBACK_SUBNET.prefix()).unwrap(),
    );
    assert_eq!(result, Err(AddIpAddrSubnetError::InvalidAddr));
}
