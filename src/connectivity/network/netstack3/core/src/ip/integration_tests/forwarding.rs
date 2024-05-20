// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ip_test_macro::ip_test;
use net_declare::{net_subnet_v4, net_subnet_v6};
use net_types::{
    ip::{Ip, IpAddress as _, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr},
    SpecifiedAddr,
};
use test_case::test_case;

use crate::{
    device::{
        ethernet::{EthernetCreationProperties, EthernetLinkDevice, MaxEthernetFrameSize},
        DeviceId,
    },
    error::NotFoundError,
    ip::{
        forwarding::AddRouteError,
        types::{AddableEntry, AddableEntryEither, AddableMetric, Entry, Metric, RawMetric},
    },
    testutil::{CtxPairExt as _, FakeCtx, TestIpExt, DEFAULT_INTERFACE_METRIC},
    StackStateBuilder,
};

#[ip_test]
#[test_case(true; "when there is an on-link route to the gateway")]
#[test_case(false; "when there is no on-link route to the gateway")]
fn select_device_for_gateway<I: Ip + TestIpExt>(on_link_route: bool) {
    let mut ctx = FakeCtx::new_with_builder(StackStateBuilder::default());

    let device_id: DeviceId<_> = ctx
        .core_api()
        .device::<EthernetLinkDevice>()
        .add_device_with_default_state(
            EthernetCreationProperties {
                mac: I::TEST_ADDRS.local_mac,
                max_frame_size: MaxEthernetFrameSize::from_mtu(I::MINIMUM_LINK_MTU).unwrap(),
            },
            DEFAULT_INTERFACE_METRIC,
        )
        .into();

    let gateway = SpecifiedAddr::new(
        // Set the last bit to make it an address inside the fake config's
        // subnet.
        I::map_ip::<_, I::Addr>(
            I::TEST_ADDRS.subnet.network(),
            |addr| {
                let mut bytes = addr.ipv4_bytes();
                bytes[bytes.len() - 1] = 1;
                Ipv4Addr::from(bytes)
            },
            |addr| {
                let mut bytes = addr.ipv6_bytes();
                bytes[bytes.len() - 1] = 1;
                Ipv6Addr::from(bytes)
            },
        )
        .to_ip_addr(),
    )
    .expect("should be specified");

    // Try to resolve a device for a gateway that we have no route to.
    assert_eq!(ctx.core_api().routes_any().select_device_for_gateway(gateway), None);

    // Add a route to the gateway.
    let route_to_add = if on_link_route {
        AddableEntryEither::from(AddableEntry::without_gateway(
            I::TEST_ADDRS.subnet,
            device_id.clone(),
            AddableMetric::ExplicitMetric(RawMetric(0)),
        ))
    } else {
        AddableEntryEither::from(AddableEntry::with_gateway(
            I::TEST_ADDRS.subnet,
            device_id.clone(),
            I::TEST_ADDRS.remote_ip,
            AddableMetric::ExplicitMetric(RawMetric(0)),
        ))
    };

    assert_eq!(ctx.test_api().add_route(route_to_add), Ok(()));

    // It still won't resolve successfully because the device is not enabled yet.
    assert_eq!(ctx.core_api().routes_any().select_device_for_gateway(gateway), None);

    ctx.test_api().enable_device(&device_id);

    // Now, try to resolve a device for the gateway.
    assert_eq!(
        ctx.core_api().routes_any().select_device_for_gateway(gateway),
        if on_link_route { Some(device_id) } else { None }
    );
}

struct AddGatewayRouteTestCase {
    enable_before_final_route_add: bool,
    expected_first_result: Result<(), AddRouteError>,
    expected_second_result: Result<(), AddRouteError>,
}

#[ip_test]
#[test_case(AddGatewayRouteTestCase {
    enable_before_final_route_add: false,
    expected_first_result: Ok(()),
    expected_second_result: Ok(()),
}; "with_specified_device_no_enable")]
#[test_case(AddGatewayRouteTestCase {
    enable_before_final_route_add: true,
    expected_first_result: Ok(()),
    expected_second_result: Ok(()),
}; "with_specified_device_enabled")]
fn add_gateway_route<I: Ip + TestIpExt>(test_case: AddGatewayRouteTestCase) {
    let AddGatewayRouteTestCase {
        enable_before_final_route_add,
        expected_first_result,
        expected_second_result,
    } = test_case;
    let mut ctx = FakeCtx::new_with_builder(StackStateBuilder::default());
    ctx.bindings_ctx.timer_ctx().assert_no_timers_installed();

    let gateway_subnet =
        I::map_ip((), |()| net_subnet_v4!("10.0.0.0/16"), |()| net_subnet_v6!("::0a00:0000/112"));

    let device_id: DeviceId<_> = ctx
        .core_api()
        .device::<EthernetLinkDevice>()
        .add_device_with_default_state(
            EthernetCreationProperties {
                mac: I::TEST_ADDRS.local_mac,
                max_frame_size: MaxEthernetFrameSize::from_mtu(I::MINIMUM_LINK_MTU).unwrap(),
            },
            DEFAULT_INTERFACE_METRIC,
        )
        .into();
    let gateway_device = device_id.clone();

    // Attempt to add the gateway route when there is no known route to the
    // gateway.
    assert_eq!(
        ctx.test_api().add_route(AddableEntryEither::from(AddableEntry::with_gateway(
            gateway_subnet,
            gateway_device.clone(),
            I::TEST_ADDRS.remote_ip,
            AddableMetric::ExplicitMetric(RawMetric(0))
        ))),
        expected_first_result,
    );

    assert_eq!(
        ctx.test_api().del_routes_to_subnet(gateway_subnet.into()),
        expected_first_result.map_err(|_: AddRouteError| NotFoundError),
    );

    // Then, add a route to the gateway, and try again, expecting success.
    assert_eq!(
        ctx.test_api().add_route(AddableEntryEither::from(AddableEntry::without_gateway(
            I::TEST_ADDRS.subnet,
            device_id.clone(),
            AddableMetric::ExplicitMetric(RawMetric(0))
        ))),
        Ok(())
    );

    if enable_before_final_route_add {
        ctx.test_api().enable_device(&device_id);
    }
    assert_eq!(
        ctx.test_api().add_route(AddableEntryEither::from(AddableEntry::with_gateway(
            gateway_subnet,
            gateway_device,
            I::TEST_ADDRS.remote_ip,
            AddableMetric::ExplicitMetric(RawMetric(0))
        ))),
        expected_second_result,
    );
}

#[ip_test]
fn test_route_tracks_interface_metric<I: Ip + TestIpExt>() {
    let mut ctx = FakeCtx::new_with_builder(StackStateBuilder::default());
    ctx.bindings_ctx.timer_ctx().assert_no_timers_installed();

    let metric = RawMetric(9999);
    let device_id = ctx.core_api().device::<EthernetLinkDevice>().add_device_with_default_state(
        EthernetCreationProperties {
            mac: I::TEST_ADDRS.local_mac,
            max_frame_size: MaxEthernetFrameSize::from_mtu(I::MINIMUM_LINK_MTU).unwrap(),
        },
        metric,
    );
    assert_eq!(
        ctx.test_api().add_route(AddableEntryEither::from(AddableEntry::without_gateway(
            I::TEST_ADDRS.subnet,
            device_id.clone().into(),
            AddableMetric::MetricTracksInterface
        ))),
        Ok(())
    );
    assert_eq!(
        ctx.core_api().routes_any().get_all_routes(),
        &[Entry {
            subnet: I::TEST_ADDRS.subnet,
            device: device_id.clone().into(),
            gateway: None,
            metric: Metric::MetricTracksInterface(metric)
        }
        .into()]
    );

    // Remove the device and routes to clear all dangling references.
    ctx.test_api().clear_routes_and_remove_ethernet_device(device_id);
}
