// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A networking stack.

#![no_std]
// In case we roll the toolchain and something we're using as a feature has been
// stabilized.
#![allow(stable_features)]
#![deny(missing_docs, unreachable_patterns)]
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
pub mod data_structures;
pub mod device;
pub mod error;
pub mod ip;
mod lock_ordering;
pub mod socket;
pub mod sync;
#[cfg(any(test, feature = "testutils"))]
pub mod testutil;
mod trace;
pub mod transport;

use alloc::vec::Vec;
use core::{fmt::Debug, marker::PhantomData, time};

use derivative::Derivative;
use lock_order::Locked;
use net_types::{
    ip::{AddrSubnetEither, IpAddr, Ipv4, Ipv6, SubnetEither},
    SpecifiedAddr,
};
use packet::{Buf, BufferMut, EmptyBuf};
use tracing::trace;

use crate::{
    context::{CounterContext, EventContext, RngContext, TimerContext, TracingContext},
    device::{DeviceId, DeviceLayerState, DeviceLayerTimerId},
    ip::{
        device::{DualStackDeviceHandler, Ipv4DeviceTimerId, Ipv6DeviceTimerId},
        icmp::{BufferIcmpContext, IcmpContext},
        IpLayerTimerId, Ipv4State, Ipv6State,
    },
    transport::{TransportLayerState, TransportLayerTimerId},
};
pub(crate) use trace::trace_duration;

/// Map an expression over either version of one or more addresses.
///
/// `map_addr_version!` when given a value of a type which is an enum with two
/// variants - `V4` and `V6` - matches on the variants, and for both variants,
/// invokes an expression on the inner contents. `$addr` is both the name of the
/// variable to match on, and the name that the address will be bound to for the
/// scope of the expression.
///
/// `map_addr_version!` when given a list of values and their types (all enums
/// with variants `V4` and `V6`), matches on the tuple of values and invokes the
/// `$match` expression when all values are of the same variant. Otherwise the
/// `$mismatch` expression is invoked.
///
/// To make it concrete, the expression `map_addr_version!(bar: Foo; blah(bar))`
/// desugars to:
///
/// ```rust,ignore
/// match bar {
///     Foo::V4(bar) => blah(bar),
///     Foo::V6(bar) => blah(bar),
/// }
/// ```
///
/// Also,
/// `map_addr_version!((foo: Foo, bar: Bar); blah(foo, bar), unreachable!())`
/// desugars to:
///
/// ```rust,ignore
/// match (foo, bar) {
///     (Foo::V4(foo), Bar::V4(bar)) => blah(foo, bar),
///     (Foo::V6(foo), Bar::V6(bar)) => blah(foo, bar),
///     _ => unreachable!(),
/// }
/// ```
#[macro_export]
macro_rules! map_addr_version {
    ($addr:ident: $ty:tt; $expr:expr) => {
        match $addr {
            $ty::V4($addr) => $expr,
            $ty::V6($addr) => $expr,
        }
    };
    ($addr:ident: $ty:tt; $expr_v4:expr, $expr_v6:expr) => {
        match $addr {
            $ty::V4($addr) => $expr_v4,
            $ty::V6($addr) => $expr_v6,
        }
    };
    (( $( $addr:ident : $ty:tt ),+ ); $match:expr, $mismatch:expr) => {
        match ( $( $addr ),+ ) {
            ( $( $ty::V4( $addr ) ),+ ) => $match,
            ( $( $ty::V6( $addr ) ),+ ) => $match,
            _ => $mismatch,
        }
    };
    (( $( $addr:ident : $ty:tt ),+ ); $match_v4:expr, $match_v6:expr, $mismatch:expr) => {
        match ( $( $addr ),+ ) {
            ( $( $ty::V4( $addr ) ),+ ) => $match_v4,
            ( $( $ty::V6( $addr ) ),+ ) => $match_v6,
            _ => $mismatch,
        }
    };
    (( $( $addr:ident : $ty:tt ),+ ); $match:expr, $mismatch:expr,) => {
        map_addr_version!(($( $addr: $ty ),+); $match, $mismatch)
    };
}

/// A builder for [`StackState`].
#[derive(Default, Clone)]
pub struct StackStateBuilder {
    transport: transport::TransportStateBuilder,
    ipv4: ip::Ipv4StateBuilder,
    ipv6: ip::Ipv6StateBuilder,
}

impl StackStateBuilder {
    /// Get the builder for the transport layer state.
    pub fn transport_builder(&mut self) -> &mut transport::TransportStateBuilder {
        &mut self.transport
    }

    /// Get the builder for the IPv4 state.
    pub fn ipv4_builder(&mut self) -> &mut ip::Ipv4StateBuilder {
        &mut self.ipv4
    }

    /// Get the builder for the IPv6 state.
    pub fn ipv6_builder(&mut self) -> &mut ip::Ipv6StateBuilder {
        &mut self.ipv6
    }

    /// Consume this builder and produce a `StackState`.
    pub fn build_with_ctx<C: NonSyncContext>(self, ctx: &mut C) -> StackState<C> {
        StackState {
            transport: self.transport.build_with_ctx(ctx),
            ipv4: self.ipv4.build(),
            ipv6: self.ipv6.build(),
            device: DeviceLayerState::new(),
        }
    }
}

/// The state associated with the network stack.
pub struct StackState<C: NonSyncContext> {
    transport: TransportLayerState<C>,
    ipv4: Ipv4State<C::Instant, DeviceId<C>>,
    ipv6: Ipv6State<C::Instant, DeviceId<C>>,
    device: DeviceLayerState<C>,
}

impl<C: NonSyncContext + Default> Default for StackState<C> {
    fn default() -> StackState<C> {
        StackStateBuilder::default().build_with_ctx(&mut Default::default())
    }
}

/// The non synchronized context for the stack with a buffer.
pub trait BufferNonSyncContextInner<B: BufferMut>:
    transport::udp::BufferNonSyncContext<Ipv4, B>
    + transport::udp::BufferNonSyncContext<Ipv6, B>
    + BufferIcmpContext<Ipv4, B>
    + BufferIcmpContext<Ipv6, B>
{
}
impl<
        B: BufferMut,
        C: transport::udp::BufferNonSyncContext<Ipv4, B>
            + transport::udp::BufferNonSyncContext<Ipv6, B>
            + BufferIcmpContext<Ipv4, B>
            + BufferIcmpContext<Ipv6, B>,
    > BufferNonSyncContextInner<B> for C
{
}

/// The non synchronized context for the stack with a buffer.
pub trait BufferNonSyncContext<B: BufferMut>:
    NonSyncContext + BufferNonSyncContextInner<B>
{
}
impl<B: BufferMut, C: NonSyncContext + BufferNonSyncContextInner<B>> BufferNonSyncContext<B> for C {}

/// The non-synchronized context for the stack.
pub trait NonSyncContext:
    CounterContext
    + device::DeviceLayerEventDispatcher
    + BufferNonSyncContextInner<Buf<Vec<u8>>>
    + BufferNonSyncContextInner<EmptyBuf>
    + RngContext
    + TimerContext<TimerId<Self>>
    + EventContext<ip::device::IpDeviceEvent<DeviceId<Self>, Ipv4>>
    + EventContext<ip::device::IpDeviceEvent<DeviceId<Self>, Ipv6>>
    + EventContext<ip::IpLayerEvent<DeviceId<Self>, Ipv4>>
    + EventContext<ip::IpLayerEvent<DeviceId<Self>, Ipv6>>
    + transport::udp::NonSyncContext<Ipv4>
    + transport::udp::NonSyncContext<Ipv6>
    + IcmpContext<Ipv4>
    + IcmpContext<Ipv6>
    + transport::tcp::socket::NonSyncContext
    + device::DeviceLayerEventDispatcher
    + device::socket::NonSyncContext<DeviceId<Self>>
    + TracingContext
    + 'static
{
}
impl<
        C: CounterContext
            + BufferNonSyncContextInner<Buf<Vec<u8>>>
            + BufferNonSyncContextInner<EmptyBuf>
            + RngContext
            + TimerContext<TimerId<Self>>
            + EventContext<ip::device::IpDeviceEvent<DeviceId<Self>, Ipv4>>
            + EventContext<ip::device::IpDeviceEvent<DeviceId<Self>, Ipv6>>
            + EventContext<ip::IpLayerEvent<DeviceId<Self>, Ipv4>>
            + EventContext<ip::IpLayerEvent<DeviceId<Self>, Ipv6>>
            + transport::udp::NonSyncContext<Ipv4>
            + transport::udp::NonSyncContext<Ipv6>
            + IcmpContext<Ipv4>
            + IcmpContext<Ipv6>
            + transport::tcp::socket::NonSyncContext
            + device::DeviceLayerEventDispatcher
            + device::socket::NonSyncContext<DeviceId<Self>>
            + TracingContext
            + 'static,
    > NonSyncContext for C
{
}

/// The synchronized context.
#[derive(Default)]
pub struct SyncCtx<NonSyncCtx: NonSyncContext> {
    /// Contains the state of the stack.
    pub state: StackState<NonSyncCtx>,
    /// A marker for the non-synchronized context type.
    pub non_sync_ctx_marker: PhantomData<NonSyncCtx>,
}

/// The identifier for any timer event.
#[derive(Derivative)]
#[derivative(
    Clone(bound = ""),
    Eq(bound = ""),
    PartialEq(bound = ""),
    Hash(bound = ""),
    Debug(bound = "")
)]
pub struct TimerId<C: NonSyncContext>(TimerIdInner<C>);

#[derive(Derivative)]
#[derivative(
    Clone(bound = ""),
    Eq(bound = ""),
    PartialEq(bound = ""),
    Hash(bound = ""),
    Debug(bound = "")
)]
enum TimerIdInner<C: NonSyncContext> {
    /// A timer event in the device layer.
    DeviceLayer(DeviceLayerTimerId<C>),
    /// A timer event in the transport layer.
    TransportLayer(TransportLayerTimerId),
    /// A timer event in the IP layer.
    IpLayer(IpLayerTimerId),
    /// A timer event for an IPv4 device.
    Ipv4Device(Ipv4DeviceTimerId<DeviceId<C>>),
    /// A timer event for an IPv6 device.
    Ipv6Device(Ipv6DeviceTimerId<DeviceId<C>>),
    /// A no-op timer event (used for tests)
    #[cfg(test)]
    Nop(usize),
}

impl<C: NonSyncContext> From<DeviceLayerTimerId<C>> for TimerId<C> {
    fn from(id: DeviceLayerTimerId<C>) -> TimerId<C> {
        TimerId(TimerIdInner::DeviceLayer(id))
    }
}

impl<C: NonSyncContext> From<Ipv4DeviceTimerId<DeviceId<C>>> for TimerId<C> {
    fn from(id: Ipv4DeviceTimerId<DeviceId<C>>) -> TimerId<C> {
        TimerId(TimerIdInner::Ipv4Device(id))
    }
}

impl<C: NonSyncContext> From<Ipv6DeviceTimerId<DeviceId<C>>> for TimerId<C> {
    fn from(id: Ipv6DeviceTimerId<DeviceId<C>>) -> TimerId<C> {
        TimerId(TimerIdInner::Ipv6Device(id))
    }
}

impl<C: NonSyncContext> From<IpLayerTimerId> for TimerId<C> {
    fn from(id: IpLayerTimerId) -> TimerId<C> {
        TimerId(TimerIdInner::IpLayer(id))
    }
}

impl<C: NonSyncContext> From<TransportLayerTimerId> for TimerId<C> {
    fn from(id: TransportLayerTimerId) -> Self {
        TimerId(TimerIdInner::TransportLayer(id))
    }
}

impl_timer_context!(
    C: NonSyncContext,
    TimerId<C>,
    DeviceLayerTimerId<C>,
    TimerId(TimerIdInner::DeviceLayer(id)),
    id
);
impl_timer_context!(
    C: NonSyncContext,
    TimerId<C>,
    IpLayerTimerId,
    TimerId(TimerIdInner::IpLayer(id)),
    id
);
impl_timer_context!(
    C: NonSyncContext,
    TimerId<C>,
    Ipv4DeviceTimerId<DeviceId<C>>,
    TimerId(TimerIdInner::Ipv4Device(id)),
    id
);
impl_timer_context!(
    C: NonSyncContext,
    TimerId<C>,
    Ipv6DeviceTimerId<DeviceId<C>>,
    TimerId(TimerIdInner::Ipv6Device(id)),
    id
);
impl_timer_context!(
    C: NonSyncContext,
    TimerId<C>,
    TransportLayerTimerId,
    TimerId(TimerIdInner::TransportLayer(id)),
    id
);

/// Handles a generic timer event.
pub fn handle_timer<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    id: TimerId<NonSyncCtx>,
) {
    trace!("handle_timer: dispatching timerid: {:?}", id);
    let mut sync_ctx = Locked::new(sync_ctx);

    match id {
        TimerId(TimerIdInner::DeviceLayer(x)) => {
            device::handle_timer(&mut sync_ctx, ctx, x);
        }
        TimerId(TimerIdInner::TransportLayer(x)) => {
            transport::handle_timer(&mut sync_ctx, ctx, x);
        }
        TimerId(TimerIdInner::IpLayer(x)) => {
            ip::handle_timer(&mut sync_ctx, ctx, x);
        }
        TimerId(TimerIdInner::Ipv4Device(x)) => {
            ip::device::handle_ipv4_timer(&mut sync_ctx, ctx, x);
        }
        TimerId(TimerIdInner::Ipv6Device(x)) => {
            ip::device::handle_ipv6_timer(&mut sync_ctx, ctx, x);
        }
        #[cfg(test)]
        TimerId(TimerIdInner::Nop(_)) => {
            ctx.increment_debug_counter("timer::nop");
        }
    }
}

/// A type representing an instant in time.
///
/// `Instant` can be implemented by any type which represents an instant in
/// time. This can include any sort of real-world clock time (e.g.,
/// [`std::time::Instant`]) or fake time such as in testing.
pub trait Instant: Sized + Ord + Copy + Clone + Debug + Send + Sync {
    /// Returns the amount of time elapsed from another instant to this one.
    ///
    /// # Panics
    ///
    /// This function will panic if `earlier` is later than `self`.
    fn duration_since(&self, earlier: Self) -> time::Duration;

    /// Returns `Some(t)` where `t` is the time `self + duration` if `t` can be
    /// represented as `Instant` (which means it's inside the bounds of the
    /// underlying data structure), `None` otherwise.
    fn checked_add(&self, duration: time::Duration) -> Option<Self>;

    /// Unwraps the result from `checked_add`.
    ///
    /// # Panics
    ///
    /// This function will panic if the addition makes the clock wrap around.
    fn add(&self, duration: time::Duration) -> Self {
        self.checked_add(duration).unwrap_or_else(|| {
            panic!("clock wraps around when adding {:?} to {:?}", duration, *self);
        })
    }

    /// Returns `Some(t)` where `t` is the time `self - duration` if `t` can be
    /// represented as `Instant` (which means it's inside the bounds of the
    /// underlying data structure), `None` otherwise.
    fn checked_sub(&self, duration: time::Duration) -> Option<Self>;
}

/// Get all IPv4 and IPv6 address/subnet pairs configured on a device
pub fn get_all_ip_addr_subnets<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    device: &DeviceId<NonSyncCtx>,
) -> Vec<AddrSubnetEither> {
    DualStackDeviceHandler::get_all_ip_addr_subnets(&mut Locked::new(sync_ctx), device)
}

/// Set the IP address and subnet for a device.
pub fn add_ip_addr_subnet<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: &DeviceId<NonSyncCtx>,
    addr_sub: AddrSubnetEither,
) -> Result<(), error::ExistsError> {
    map_addr_version!(
        addr_sub: AddrSubnetEither;
        crate::device::add_ip_addr_subnet(&sync_ctx, ctx, device, addr_sub)
    )
}

/// Delete an IP address on a device.
pub fn del_ip_addr<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: &DeviceId<NonSyncCtx>,
    addr: SpecifiedAddr<IpAddr>,
) -> Result<(), error::NotFoundError> {
    let addr = addr.into();
    map_addr_version!(
        addr: IpAddr;
        crate::device::del_ip_addr(&sync_ctx, ctx, device, &addr)
    )
}

/// Adds a route to the forwarding table.
pub fn add_route<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    entry: ip::types::AddableEntryEither<DeviceId<NonSyncCtx>>,
) -> Result<(), ip::forwarding::AddRouteError> {
    let mut sync_ctx = Locked::new(sync_ctx);
    match entry {
        ip::types::AddableEntryEither::V4(entry) => {
            ip::forwarding::add_route::<Ipv4, _, _>(&mut sync_ctx, ctx, entry)
        }
        ip::types::AddableEntryEither::V6(entry) => {
            ip::forwarding::add_route::<Ipv6, _, _>(&mut sync_ctx, ctx, entry)
        }
    }
}

/// Delete a route from the forwarding table, returning `Err` if no
/// route was found to be deleted.
pub fn del_route<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    subnet: SubnetEither,
) -> error::Result<()> {
    let mut sync_ctx = Locked::new(sync_ctx);
    map_addr_version!(
        subnet: SubnetEither;
        crate::ip::forwarding::del_subnet_route::<Ipv4, _, _>(&mut sync_ctx, ctx, subnet),
        crate::ip::forwarding::del_subnet_route::<Ipv6, _, _>(&mut sync_ctx, ctx, subnet)
    )
    .map_err(From::from)
}

#[cfg(test)]
mod tests {
    use ip_test_macro::ip_test;
    use net_declare::{net_subnet_v4, net_subnet_v6};
    use net_types::{
        ip::{Ip, Ipv4, Ipv6},
        Witness,
    };
    use test_case::test_case;

    use super::*;
    use crate::{
        ip::{
            forwarding::AddRouteError,
            types::{AddableEntry, AddableEntryEither, AddableMetric, Entry, Metric, RawMetric},
        },
        testutil::{
            Ctx, FakeCtx, TestIpExt, DEFAULT_INTERFACE_METRIC, IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
        },
    };

    fn test_add_remove_ip_addresses<I: Ip + TestIpExt>() {
        let config = I::FAKE_CONFIG;
        let Ctx { sync_ctx, mut non_sync_ctx } = crate::testutil::FakeCtx::default();
        let device = crate::device::add_ethernet_device(
            &sync_ctx,
            config.local_mac,
            IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
            DEFAULT_INTERFACE_METRIC,
        )
        .into();
        crate::device::testutil::enable_device(&sync_ctx, &mut non_sync_ctx, &device);

        let ip: IpAddr = I::get_other_ip_address(1).get().into();
        let prefix = config.subnet.prefix();
        let addr_subnet = AddrSubnetEither::new(ip, prefix).unwrap();

        // IP doesn't exist initially.
        assert_eq!(
            get_all_ip_addr_subnets(&sync_ctx, &device).into_iter().find(|&a| a == addr_subnet),
            None
        );

        // Add IP (OK).
        let () = add_ip_addr_subnet(&sync_ctx, &mut non_sync_ctx, &device, addr_subnet).unwrap();
        assert_eq!(
            get_all_ip_addr_subnets(&sync_ctx, &device).into_iter().find(|&a| a == addr_subnet),
            Some(addr_subnet)
        );

        // Add IP again (already exists).
        assert_eq!(
            add_ip_addr_subnet(&sync_ctx, &mut non_sync_ctx, &device, addr_subnet).unwrap_err(),
            error::ExistsError
        );
        assert_eq!(
            get_all_ip_addr_subnets(&sync_ctx, &device).into_iter().find(|&a| a == addr_subnet),
            Some(addr_subnet)
        );

        // Add IP with different subnet (already exists).
        let wrong_addr_subnet = AddrSubnetEither::new(ip, prefix - 1).unwrap();
        assert_eq!(
            add_ip_addr_subnet(&sync_ctx, &mut non_sync_ctx, &device, wrong_addr_subnet)
                .unwrap_err(),
            error::ExistsError
        );
        assert_eq!(
            get_all_ip_addr_subnets(&sync_ctx, &device).into_iter().find(|&a| a == addr_subnet),
            Some(addr_subnet)
        );

        let ip = SpecifiedAddr::new(ip).unwrap();
        // Del IP (ok).
        let () = del_ip_addr(&sync_ctx, &mut non_sync_ctx, &device, ip.into()).unwrap();
        assert_eq!(
            get_all_ip_addr_subnets(&sync_ctx, &device).into_iter().find(|&a| a == addr_subnet),
            None
        );

        // Del IP again (not found).
        assert_eq!(
            del_ip_addr(&sync_ctx, &mut non_sync_ctx, &device, ip.into()).unwrap_err(),
            error::NotFoundError
        );
        assert_eq!(
            get_all_ip_addr_subnets(&sync_ctx, &device).into_iter().find(|&a| a == addr_subnet),
            None
        );
    }

    #[test]
    fn test_add_remove_ipv4_addresses() {
        test_add_remove_ip_addresses::<Ipv4>();
    }

    #[test]
    fn test_add_remove_ipv6_addresses() {
        test_add_remove_ip_addresses::<Ipv6>();
    }

    #[ip_test]
    #[test_case(false; "without_specified_device")]
    #[test_case(true; "with_specified_device")]
    fn add_gateway_route<I: Ip + TestIpExt>(should_specify_device: bool) {
        let FakeCtx { sync_ctx, mut non_sync_ctx } =
            Ctx::new_with_builder(crate::StackStateBuilder::default());
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
        let gateway_device = should_specify_device.then_some(device_id.clone());

        // Attempt to add the gateway route when there is no known route to the
        // gateway.
        assert_eq!(
            crate::add_route(
                &sync_ctx,
                &mut non_sync_ctx,
                AddableEntryEither::from(AddableEntry::with_gateway(
                    gateway_subnet,
                    gateway_device.clone(),
                    I::FAKE_CONFIG.remote_ip.into(),
                    AddableMetric::ExplicitMetric(RawMetric(0))
                ))
            ),
            Err(AddRouteError::GatewayNotNeighbor)
        );

        // Then, add a route to the gateway, and try again, expecting success.
        assert_eq!(
            crate::add_route(
                &sync_ctx,
                &mut non_sync_ctx,
                AddableEntryEither::from(AddableEntry::without_gateway(
                    I::FAKE_CONFIG.subnet.into(),
                    device_id.clone(),
                    AddableMetric::ExplicitMetric(RawMetric(0))
                ))
            ),
            Ok(())
        );
        assert_eq!(
            crate::add_route(
                &sync_ctx,
                &mut non_sync_ctx,
                AddableEntryEither::from(AddableEntry::with_gateway(
                    gateway_subnet,
                    gateway_device,
                    I::FAKE_CONFIG.remote_ip.into(),
                    AddableMetric::ExplicitMetric(RawMetric(0))
                ))
            ),
            Ok(())
        );
    }

    #[ip_test]
    fn test_route_tracks_interface_metric<I: Ip + TestIpExt>() {
        let FakeCtx { sync_ctx, mut non_sync_ctx } =
            Ctx::new_with_builder(crate::StackStateBuilder::default());
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
            crate::add_route(
                &sync_ctx,
                &mut non_sync_ctx,
                AddableEntryEither::from(AddableEntry::without_gateway(
                    I::FAKE_CONFIG.subnet.into(),
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
