// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A networking stack.

#![no_std]
// In case we roll the toolchain and something we're using as a feature has been
// stabilized.
#![allow(stable_features)]
#![deny(missing_docs, unreachable_patterns, clippy::useless_conversion, clippy::redundant_clone)]
// Turn off checks for dead code, but only when building for benchmarking.
// benchmarking. This allows the benchmarks to be written as part of the crate,
// with access to test utilities, without a bunch of build errors due to unused
// code. These checks are turned back on in the 'benchmark' module.
#![cfg_attr(benchmark, allow(dead_code, unused_imports, unused_macros))]

// TODO(https://github.com/rust-lang-nursery/portability-wg/issues/11): remove
// this module.
extern crate fakealloc as alloc;

// TODO(https://github.com/dtolnay/thiserror/pull/64): remove this module.
extern crate fakestd as std;

#[macro_use]
mod macros;

mod algorithm;
#[cfg(test)]
pub mod benchmarks;
pub mod context;
pub(crate) mod convert;
pub mod counters;
pub mod data_structures;
pub mod device;
pub mod error;
pub mod ip;
mod lock_ordering;
pub mod socket;
pub mod state;
pub mod sync;
#[cfg(any(test, feature = "testutils"))]
pub mod testutil;
pub mod time;
mod trace;
pub mod transport;
pub mod work_queue;

use alloc::vec::Vec;

use lock_order::Locked;
use net_types::{
    ip::{GenericOverIp, Ip, IpAddr, IpInvariant, Ipv4, Ipv6, Ipv6Addr, Subnet},
    SpecifiedAddr,
};

use crate::{context::RngContext, device::DeviceId};
pub use context::{BindingsTypes, NonSyncContext, ReferenceNotifiers, SyncCtx};
pub use time::{handle_timer, Instant, TimerId};
pub use work_queue::WorkQueueReport;

pub(crate) use trace::trace_duration;

/// Selects the device to use for gateway routes when the device was unspecified
/// by the client.
/// This can be used to construct an `Entry` from an `AddableEntry` the same
/// way that the core routing table does.
pub fn select_device_for_gateway<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    gateway: SpecifiedAddr<IpAddr>,
) -> Option<DeviceId<NonSyncCtx>> {
    let mut sync_ctx = Locked::new(sync_ctx);
    match gateway.into() {
        IpAddr::V4(gateway) => {
            ip::forwarding::select_device_for_gateway::<Ipv4, _, _>(&mut sync_ctx, gateway)
        }
        IpAddr::V6(gateway) => {
            ip::forwarding::select_device_for_gateway::<Ipv6, _, _>(&mut sync_ctx, gateway)
        }
    }
}

/// Set the routes in the routing table.
///
/// While doing a full `set` of the routing table with each modification is
/// suboptimal for performance, it simplifies the API exposed by core for route
/// table modifications to allow for evolution of the routing table in the
/// future.
pub fn set_routes<I: Ip, NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    entries: Vec<ip::types::EntryAndGeneration<I::Addr, DeviceId<NonSyncCtx>>>,
) {
    #[derive(GenericOverIp)]
    #[generic_over_ip(I, Ip)]
    struct Wrap<I: Ip, NonSyncCtx: NonSyncContext>(
        Vec<ip::types::EntryAndGeneration<I::Addr, DeviceId<NonSyncCtx>>>,
    );

    let () = net_types::map_ip_twice!(I, (IpInvariant((sync_ctx, ctx)), Wrap(entries)), |(
        IpInvariant((sync_ctx, ctx)),
        Wrap(entries),
    )| {
        let mut sync_ctx = Locked::new(sync_ctx);
        ip::forwarding::set_routes::<I, _, _>(&mut sync_ctx, ctx, entries)
    },);
}

/// Requests that a route be added to the forwarding table.
pub(crate) fn request_context_add_route<NonSyncCtx: NonSyncContext>(
    ctx: &mut NonSyncCtx,
    entry: ip::types::AddableEntryEither<DeviceId<NonSyncCtx>>,
) {
    match entry {
        ip::types::AddableEntryEither::V4(entry) => {
            ip::forwarding::request_context_add_route::<Ipv4, _, _>(ctx, entry)
        }
        ip::types::AddableEntryEither::V6(entry) => {
            ip::forwarding::request_context_add_route::<Ipv6, _, _>(ctx, entry)
        }
    }
}

/// Requests that routes matching these specifiers be removed from the forwarding table.
pub(crate) fn request_context_del_routes_v6<NonSyncCtx: NonSyncContext>(
    ctx: &mut NonSyncCtx,
    subnet: Subnet<Ipv6Addr>,
    del_device: &DeviceId<NonSyncCtx>,
    del_gateway: Option<SpecifiedAddr<Ipv6Addr>>,
) {
    crate::ip::forwarding::request_context_del_routes::<Ipv6, _, _>(
        ctx,
        subnet,
        del_device.clone(),
        del_gateway,
    )
}

#[cfg(test)]
mod tests {
    use ip_test_macro::ip_test;
    use net_declare::{net_subnet_v4, net_subnet_v6};
    use net_types::ip::{Ip, IpAddress, Ipv4, Ipv4Addr, Ipv6};
    use test_case::test_case;

    use super::*;
    use crate::{
        ip::{
            forwarding::AddRouteError,
            types::{AddableEntry, AddableEntryEither, AddableMetric, Entry, Metric, RawMetric},
        },
        testutil::{Ctx, FakeCtx, TestIpExt, DEFAULT_INTERFACE_METRIC},
    };

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
        let FakeCtx { sync_ctx, mut non_sync_ctx } =
            Ctx::new_with_builder(crate::state::StackStateBuilder::default());
        non_sync_ctx.timer_ctx().assert_no_timers_installed();

        let gateway_subnet = I::map_ip(
            (),
            |()| net_subnet_v4!("10.0.0.0/16"),
            |()| net_subnet_v6!("::0a00:0000/112"),
        );

        let device_id: DeviceId<_> = crate::device::add_ethernet_device(
            &sync_ctx,
            I::FAKE_CONFIG.local_mac,
            crate::device::ethernet::MaxFrameSize::from_mtu(I::MINIMUM_LINK_MTU).unwrap(),
            DEFAULT_INTERFACE_METRIC,
        )
        .into();
        let gateway_device = device_id.clone();

        // Attempt to add the gateway route when there is no known route to the
        // gateway.
        assert_eq!(
            crate::testutil::add_route(
                &sync_ctx,
                &mut non_sync_ctx,
                AddableEntryEither::from(AddableEntry::with_gateway(
                    gateway_subnet,
                    gateway_device.clone(),
                    I::FAKE_CONFIG.remote_ip,
                    AddableMetric::ExplicitMetric(RawMetric(0))
                ))
            ),
            expected_first_result,
        );

        assert_eq!(
            crate::testutil::del_routes_to_subnet(
                &sync_ctx,
                &mut non_sync_ctx,
                gateway_subnet.into()
            ),
            expected_first_result.map_err(|_: AddRouteError| error::NetstackError::NotFound),
        );

        // Then, add a route to the gateway, and try again, expecting success.
        assert_eq!(
            crate::testutil::add_route(
                &sync_ctx,
                &mut non_sync_ctx,
                AddableEntryEither::from(AddableEntry::without_gateway(
                    I::FAKE_CONFIG.subnet,
                    device_id.clone(),
                    AddableMetric::ExplicitMetric(RawMetric(0))
                ))
            ),
            Ok(())
        );

        if enable_before_final_route_add {
            crate::device::testutil::enable_device(&sync_ctx, &mut non_sync_ctx, &device_id);
        }
        assert_eq!(
            crate::testutil::add_route(
                &sync_ctx,
                &mut non_sync_ctx,
                AddableEntryEither::from(AddableEntry::with_gateway(
                    gateway_subnet,
                    gateway_device,
                    I::FAKE_CONFIG.remote_ip,
                    AddableMetric::ExplicitMetric(RawMetric(0))
                ))
            ),
            expected_second_result,
        );
    }

    #[ip_test]
    #[test_case(true; "when there is an on-link route to the gateway")]
    #[test_case(false; "when there is no on-link route to the gateway")]
    fn select_device_for_gateway<I: Ip + TestIpExt>(on_link_route: bool) {
        let FakeCtx { sync_ctx, mut non_sync_ctx } =
            Ctx::new_with_builder(crate::state::StackStateBuilder::default());
        non_sync_ctx.timer_ctx().assert_no_timers_installed();

        let device_id: DeviceId<_> = crate::device::add_ethernet_device(
            &sync_ctx,
            I::FAKE_CONFIG.local_mac,
            crate::device::ethernet::MaxFrameSize::from_mtu(I::MINIMUM_LINK_MTU).unwrap(),
            DEFAULT_INTERFACE_METRIC,
        )
        .into();

        let gateway = SpecifiedAddr::new(
            // Set the last bit to make it an address inside the fake config's
            // subnet.
            I::map_ip::<_, I::Addr>(
                I::FAKE_CONFIG.subnet.network(),
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
        assert_eq!(crate::select_device_for_gateway(&sync_ctx, gateway), None);

        // Add a route to the gateway.
        let route_to_add = if on_link_route {
            AddableEntryEither::from(AddableEntry::without_gateway(
                I::FAKE_CONFIG.subnet,
                device_id.clone(),
                AddableMetric::ExplicitMetric(RawMetric(0)),
            ))
        } else {
            AddableEntryEither::from(AddableEntry::with_gateway(
                I::FAKE_CONFIG.subnet,
                device_id.clone(),
                I::FAKE_CONFIG.remote_ip,
                AddableMetric::ExplicitMetric(RawMetric(0)),
            ))
        };

        assert_eq!(crate::testutil::add_route(&sync_ctx, &mut non_sync_ctx, route_to_add), Ok(()));

        // It still won't resolve successfully because the device is not enabled yet.
        assert_eq!(crate::select_device_for_gateway(&sync_ctx, gateway), None);

        crate::device::testutil::enable_device(&sync_ctx, &mut non_sync_ctx, &device_id);

        // Now, try to resolve a device for the gateway.
        assert_eq!(
            crate::select_device_for_gateway(&sync_ctx, gateway),
            if on_link_route { Some(device_id) } else { None }
        );
    }

    #[ip_test]
    fn test_route_tracks_interface_metric<I: Ip + TestIpExt>() {
        let FakeCtx { sync_ctx, mut non_sync_ctx } =
            Ctx::new_with_builder(crate::state::StackStateBuilder::default());
        non_sync_ctx.timer_ctx().assert_no_timers_installed();

        let metric = RawMetric(9999);
        //let device_id = sync_ctx.state.device.add_ethernet_device(
        let device_id = crate::device::add_ethernet_device(
            &sync_ctx,
            I::FAKE_CONFIG.local_mac,
            crate::device::ethernet::MaxFrameSize::from_mtu(I::MINIMUM_LINK_MTU).unwrap(),
            metric,
        );
        assert_eq!(
            crate::testutil::add_route(
                &sync_ctx,
                &mut non_sync_ctx,
                AddableEntryEither::from(AddableEntry::without_gateway(
                    I::FAKE_CONFIG.subnet,
                    device_id.clone().into(),
                    AddableMetric::MetricTracksInterface
                ))
            ),
            Ok(())
        );
        assert_eq!(
            crate::ip::get_all_routes(&sync_ctx),
            &[Entry {
                subnet: I::FAKE_CONFIG.subnet,
                device: device_id.into(),
                gateway: None,
                metric: Metric::MetricTracksInterface(metric)
            }
            .into()]
        );
    }
}
