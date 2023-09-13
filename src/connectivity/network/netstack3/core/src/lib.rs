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
use core::{fmt::Debug, time};

use derivative::Derivative;
use lock_order::Locked;
use net_types::{
    ip::{
        AddrSubnetEither, GenericOverIp, Ip, IpAddr, IpInvariant, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr,
        Subnet,
    },
    SpecifiedAddr,
};
use packet::{Buf, BufferMut, EmptyBuf};
use tracing::trace;

use crate::{
    context::{
        CounterContext, EventContext, InstantContext, RngContext, TimerContext, TracingContext,
    },
    device::{
        ethernet::EthernetLinkDevice, DeviceId, DeviceLayerState, DeviceLayerTimerId, WeakDeviceId,
    },
    ip::{
        device::{
            state::AddrSubnetAndManualConfigEither, DualStackDeviceHandler, Ipv4DeviceTimerId,
            Ipv6DeviceTimerId,
        },
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

/// A context trait determining the types to be used for reference notifications.
pub trait ReferenceNotifiers {
    /// The receiver for shared reference destruction notifications.
    type ReferenceReceiver<T: 'static>: 'static;
    /// The notifier for shared reference destruction notifications.
    type ReferenceNotifier<T: Send + 'static>: sync::RcNotifier<T> + 'static;

    /// Creates a new Notifier/Receiver pair for `T`.
    ///
    /// `debug_references` is given to provide information on outstanding
    /// references that caused the notifier to be requested.
    fn new_reference_notifier<T: Send + 'static, D: Debug>(
        debug_references: D,
    ) -> (Self::ReferenceNotifier<T>, Self::ReferenceReceiver<T>);
}

/// The non-synchronized context for the stack.
pub trait NonSyncContext: CounterContext
    + BufferNonSyncContextInner<Buf<Vec<u8>>>
    + BufferNonSyncContextInner<EmptyBuf>
    + RngContext
    + TimerContext<TimerId<Self>>
    + EventContext<ip::device::IpDeviceEvent<DeviceId<Self>, Ipv4, <Self as InstantContext>::Instant>>
    + EventContext<ip::device::IpDeviceEvent<DeviceId<Self>, Ipv6, <Self as InstantContext>::Instant>>
    + EventContext<ip::IpLayerEvent<DeviceId<Self>, Ipv4>>
    + EventContext<ip::IpLayerEvent<DeviceId<Self>, Ipv6>>
    + transport::udp::NonSyncContext<Ipv4>
    + transport::udp::NonSyncContext<Ipv6>
    + IcmpContext<Ipv4>
    + IcmpContext<Ipv6>
    + transport::tcp::socket::NonSyncContext
    + ip::device::nud::LinkResolutionContext<EthernetLinkDevice>
    + device::DeviceLayerEventDispatcher
    + device::socket::NonSyncContext<DeviceId<Self>>
    + ReferenceNotifiers
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
            + EventContext<
                ip::device::IpDeviceEvent<DeviceId<Self>, Ipv4, <Self as InstantContext>::Instant>,
            > + EventContext<
                ip::device::IpDeviceEvent<DeviceId<Self>, Ipv6, <Self as InstantContext>::Instant>,
            > + EventContext<ip::IpLayerEvent<DeviceId<Self>, Ipv4>>
            + EventContext<ip::IpLayerEvent<DeviceId<Self>, Ipv6>>
            + transport::udp::NonSyncContext<Ipv4>
            + transport::udp::NonSyncContext<Ipv6>
            + IcmpContext<Ipv4>
            + IcmpContext<Ipv6>
            + transport::tcp::socket::NonSyncContext
            + ip::device::nud::LinkResolutionContext<EthernetLinkDevice>
            + device::DeviceLayerEventDispatcher
            + device::socket::NonSyncContext<DeviceId<Self>>
            + TracingContext
            + ReferenceNotifiers
            + 'static,
    > NonSyncContext for C
{
}

/// The synchronized context.
pub struct SyncCtx<NonSyncCtx: NonSyncContext> {
    /// Contains the state of the stack.
    pub state: StackState<NonSyncCtx>,
}

impl<NonSyncCtx: NonSyncContext> SyncCtx<NonSyncCtx> {
    /// Create a new `SyncCtx`.
    pub fn new(non_sync_ctx: &mut NonSyncCtx) -> SyncCtx<NonSyncCtx> {
        SyncCtx { state: StackStateBuilder::default().build_with_ctx(non_sync_ctx) }
    }
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

    /// Returns the amount of time elapsed from another instant to this one,
    /// saturating at zero.
    fn saturating_duration_since(&self, earlier: Self) -> time::Duration;

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
    addr_sub: impl Into<AddrSubnetAndManualConfigEither<NonSyncCtx::Instant>>,
) -> Result<(), error::ExistsError> {
    crate::device::add_ip_addr_subnet(&sync_ctx, ctx, device, addr_sub.into())
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

/// Selects the device to use for gateway routes when the device was unspecified
/// by the client.
/// This can be used to construct an `Entry` from an `AddableEntry` the same
/// way that the core routing table does.
pub fn select_device_for_gateway<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    gateway: SpecifiedAddr<IpAddr>,
) -> Option<DeviceId<NonSyncCtx>> {
    let mut sync_ctx = Locked::new(sync_ctx);
    let gateway: IpAddr<SpecifiedAddr<Ipv4Addr>, SpecifiedAddr<Ipv6Addr>> = gateway.into();
    map_addr_version!(
        gateway: IpAddr;
        ip::forwarding::select_device_for_gateway::<Ipv4, _, _>(&mut sync_ctx, gateway),
        ip::forwarding::select_device_for_gateway::<Ipv6, _, _>(&mut sync_ctx, gateway)
    )
}

/// Set the routes in the routing table.
///
/// While doing a full `set` of the routing table with each modification is
/// suboptimal for performance, it simplifies the API exposed by core for route
/// table modifications to allow for evolution of the routing table in the
/// future.
///
/// Rather than passing a list of routing table Entries, callers pass a closure
/// that produces such a list, given a closure that allows upgrading a
/// `AddableEntry` holding a `WeakDeviceId` to an `Entry` holding a strong
/// `DeviceId`.
// TODO(https://fxbug.dev/132990): Once we can await device teardown, we can
// hold strong DeviceIds instead and get rid of this complicated closure.
pub fn set_routes<I: Ip, T, NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    entries: &mut dyn FnMut(
        ip::types::EntryUpgrader<'_, I::Addr, DeviceId<NonSyncCtx>, WeakDeviceId<NonSyncCtx>>,
    ) -> (
        Vec<ip::types::EntryAndGeneration<I::Addr, DeviceId<NonSyncCtx>>>,
        T,
    ),
) -> T {
    #[derive(GenericOverIp)]
    struct Wrap<'a, I: Ip, NonSyncCtx: NonSyncContext, T>(
        &'a mut dyn FnMut(
            ip::types::EntryUpgrader<'_, I::Addr, DeviceId<NonSyncCtx>, WeakDeviceId<NonSyncCtx>>,
        ) -> (
            Vec<ip::types::EntryAndGeneration<I::Addr, DeviceId<NonSyncCtx>>>,
            T,
        ),
    );

    let IpInvariant(t) = I::map_ip(
        (IpInvariant((sync_ctx, ctx)), Wrap(entries)),
        |(IpInvariant((sync_ctx, ctx)), Wrap(entries))| {
            let mut sync_ctx = Locked::new(sync_ctx);
            IpInvariant(ip::forwarding::set_routes::<Ipv4, _, _, _>(&mut sync_ctx, ctx, entries))
        },
        |(IpInvariant((sync_ctx, ctx)), Wrap(entries))| {
            let mut sync_ctx = Locked::new(sync_ctx);
            IpInvariant(ip::forwarding::set_routes::<Ipv6, _, _, _>(&mut sync_ctx, ctx, entries))
        },
    );
    t
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

/// A common type returned by functions that perform bounded amounts of work.
///
/// This exists so cooperative task execution and yielding can be sensibly
/// performed when dealing with long work queues.
#[derive(Debug, Eq, PartialEq)]
pub enum WorkQueueReport {
    /// All the available work was done.
    AllDone,
    /// There's still pending work to do, execution was cut short.
    Pending,
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use ip_test_macro::ip_test;
    use net_declare::{net_subnet_v4, net_subnet_v6};
    use net_types::{
        ip::{AddrSubnet, Ip, IpAddress, Ipv4, Ipv6},
        Witness,
    };
    use test_case::test_case;

    use super::*;
    use crate::{
        context::testutil::FakeInstant,
        ip::{
            device::state::{Ipv4AddrConfig, Ipv6AddrManualConfig, Lifetime},
            forwarding::AddRouteError,
            types::{AddableEntry, AddableEntryEither, AddableMetric, Entry, Metric, RawMetric},
        },
        testutil::{
            Ctx, FakeCtx, TestIpExt, DEFAULT_INTERFACE_METRIC, IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
        },
    };

    fn test_add_remove_ip_addresses<I: Ip + TestIpExt>(
        addr_config: Option<I::ManualAddressConfig<FakeInstant>>,
    ) {
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

        let ip = I::get_other_ip_address(1).get();
        let prefix = config.subnet.prefix();
        let addr_subnet = AddrSubnetEither::new(ip.into(), prefix).unwrap();

        // IP doesn't exist initially.
        assert_eq!(
            get_all_ip_addr_subnets(&sync_ctx, &device).into_iter().find(|&a| a == addr_subnet),
            None
        );

        // Add IP (OK).
        if let Some(addr_config) = addr_config {
            add_ip_addr_subnet(
                &sync_ctx,
                &mut non_sync_ctx,
                &device,
                AddrSubnetAndManualConfigEither::new::<I>(
                    AddrSubnet::new(ip, prefix).unwrap(),
                    addr_config,
                ),
            )
            .unwrap();
        } else {
            let () =
                add_ip_addr_subnet(&sync_ctx, &mut non_sync_ctx, &device, addr_subnet).unwrap();
        }
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
        let wrong_addr_subnet = AddrSubnetEither::new(ip.into(), prefix - 1).unwrap();
        assert_eq!(
            add_ip_addr_subnet(&sync_ctx, &mut non_sync_ctx, &device, wrong_addr_subnet)
                .unwrap_err(),
            error::ExistsError
        );
        assert_eq!(
            get_all_ip_addr_subnets(&sync_ctx, &device).into_iter().find(|&a| a == addr_subnet),
            Some(addr_subnet)
        );

        let ip: SpecifiedAddr<IpAddr> = SpecifiedAddr::new(ip.into()).unwrap();
        // Del IP (ok).
        let () = del_ip_addr(&sync_ctx, &mut non_sync_ctx, &device, ip).unwrap();
        assert_eq!(
            get_all_ip_addr_subnets(&sync_ctx, &device).into_iter().find(|&a| a == addr_subnet),
            None
        );

        // Del IP again (not found).
        assert_eq!(
            del_ip_addr(&sync_ctx, &mut non_sync_ctx, &device, ip).unwrap_err(),
            error::NotFoundError
        );
        assert_eq!(
            get_all_ip_addr_subnets(&sync_ctx, &device).into_iter().find(|&a| a == addr_subnet),
            None
        );
    }

    #[test_case(None; "with no AddressConfig specified")]
    #[test_case(Some(Ipv4AddrConfig {
        valid_until: Lifetime::Finite(FakeInstant::from(Duration::from_secs(1)))
    }); "with AddressConfig specified")]
    fn test_add_remove_ipv4_addresses(addr_config: Option<Ipv4AddrConfig<FakeInstant>>) {
        test_add_remove_ip_addresses::<Ipv4>(addr_config);
    }

    #[test_case(None; "with no AddressConfig specified")]
    #[test_case(Some(Ipv6AddrManualConfig {
        valid_until: Lifetime::Finite(FakeInstant::from(Duration::from_secs(1)))
    }); "with AddressConfig specified")]
    fn test_add_remove_ipv6_addresses(addr_config: Option<Ipv6AddrManualConfig<FakeInstant>>) {
        test_add_remove_ip_addresses::<Ipv6>(addr_config);
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
            Ctx::new_with_builder(crate::StackStateBuilder::default());
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
