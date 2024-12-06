// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Provides test utilities for generic multicast protocol implementations. The
//! utilities here allow testing GMP without regard for MLD/IGMP differences.

use alloc::vec::Vec;
use core::convert::Infallible as Never;
use core::time::Duration;

use net_declare::{net_ip_v4, net_ip_v6};
use net_types::ip::{Ipv4, Ipv4Addr, Ipv6, Ipv6Addr};
use net_types::MulticastAddr;
use netstack3_base::testutil::{FakeBindingsCtx, FakeDeviceId, FakeWeakDeviceId};
use netstack3_base::{
    AnyDevice, CtxPair, DeviceIdContext, HandleableTimer, IntoCoreTimerCtx, TimerBindingsTypes,
};
use packet_formats::utils::NonZeroDuration;

use crate::internal::gmp::{
    self, GmpContext, GmpContextInner, GmpGroupState, GmpMode, GmpState, GmpStateRef, GmpTimerId,
    GmpTypeLayout, IpExt, MulticastGroupSet,
};

pub(super) struct FakeGmpContext<I: IpExt> {
    pub inner: FakeGmpContextInner<I>,
    pub enabled: bool,
    pub groups: MulticastGroupSet<I::Addr, GmpGroupState<FakeGmpBindingsContext<I>>>,
    pub gmp: GmpState<I, FakeGmpBindingsContext<I>>,
    pub config: FakeGmpConfig,
}

impl<I: IpExt> FakeGmpContext<I> {}

impl<I: IpExt> DeviceIdContext<AnyDevice> for FakeGmpContext<I> {
    type DeviceId = FakeDeviceId;
    type WeakDeviceId = FakeWeakDeviceId<FakeDeviceId>;
}

pub(super) type FakeGmpBindingsContext<I> =
    FakeBindingsCtx<GmpTimerId<I, FakeWeakDeviceId<FakeDeviceId>>, (), (), ()>;

impl<I: IpExt> GmpTypeLayout<I, FakeGmpBindingsContext<I>> for FakeGmpContext<I> {
    type Actions = Never;
    type Config = FakeGmpConfig;
}

impl<I: IpExt> GmpContext<I, FakeGmpBindingsContext<I>> for FakeGmpContext<I> {
    type Inner<'a> = &'a mut FakeGmpContextInner<I>;

    fn with_gmp_state_mut_and_ctx<
        O,
        F: FnOnce(Self::Inner<'_>, GmpStateRef<'_, I, Self, FakeGmpBindingsContext<I>>) -> O,
    >(
        &mut self,
        _device: &Self::DeviceId,
        cb: F,
    ) -> O {
        let Self { inner, enabled, groups, gmp, config } = self;
        cb(inner, GmpStateRef { enabled: *enabled, groups, gmp, config })
    }
}

impl<I: IpExt> HandleableTimer<FakeGmpContext<I>, FakeGmpBindingsContext<I>>
    for GmpTimerId<I, FakeWeakDeviceId<FakeDeviceId>>
{
    fn handle(
        self,
        core_ctx: &mut FakeGmpContext<I>,
        bindings_ctx: &mut FakeGmpBindingsContext<I>,
        _timer: <FakeGmpBindingsContext<I> as TimerBindingsTypes>::UniqueTimerId,
    ) {
        gmp::handle_timer(core_ctx, bindings_ctx, self);
    }
}

#[derive(Default)]
pub(super) struct FakeGmpContextInner<I: IpExt> {
    pub v1_messages: Vec<(MulticastAddr<I::Addr>, gmp::v1::GmpMessageType)>,
}

impl<I: IpExt> DeviceIdContext<AnyDevice> for &'_ mut FakeGmpContextInner<I> {
    type DeviceId = FakeDeviceId;
    type WeakDeviceId = FakeWeakDeviceId<FakeDeviceId>;
}

impl<I: IpExt> GmpTypeLayout<I, FakeGmpBindingsContext<I>> for &'_ mut FakeGmpContextInner<I> {
    type Actions = Never;
    type Config = FakeGmpConfig;
}

impl<I: IpExt> GmpContextInner<I, FakeGmpBindingsContext<I>> for &'_ mut FakeGmpContextInner<I> {
    fn send_message_v1(
        &mut self,
        _bindings_ctx: &mut FakeGmpBindingsContext<I>,
        _device: &Self::DeviceId,
        group_addr: MulticastAddr<I::Addr>,
        msg_type: gmp::v1::GmpMessageType,
    ) {
        self.v1_messages.push((group_addr, msg_type));
    }

    fn run_actions(
        &mut self,
        _bindings_ctx: &mut FakeGmpBindingsContext<I>,
        _device: &Self::DeviceId,
        actions: Never,
    ) {
        match actions {}
    }

    fn handle_mode_change(
        &mut self,
        _bindings_ctx: &mut FakeGmpBindingsContext<I>,
        _device: &Self::DeviceId,
        _new_mode: GmpMode,
    ) {
    }
}

#[derive(Debug, Default)]
pub(super) struct FakeGmpConfig {}

impl gmp::v1::ProtocolConfig for FakeGmpConfig {
    fn unsolicited_report_interval(&self) -> Duration {
        Duration::from_secs(10)
    }

    fn send_leave_anyway(&self) -> bool {
        false
    }

    fn get_max_resp_time(&self, resp_time: Duration) -> Option<NonZeroDuration> {
        NonZeroDuration::new(resp_time)
    }

    type QuerySpecificActions = Never;

    fn do_query_received_specific(
        &self,
        _max_resp_time: Duration,
    ) -> Option<Self::QuerySpecificActions> {
        None
    }
}

impl gmp::v2::ProtocolConfig for FakeGmpConfig {
    fn query_response_interval(&self) -> NonZeroDuration {
        gmp::v2::DEFAULT_QUERY_RESPONSE_INTERVAL
    }
}

pub(super) type FakeCtx<I> = CtxPair<FakeGmpContext<I>, FakeGmpBindingsContext<I>>;

pub(super) fn new_context_with_mode<I: IpExt>(mode: GmpMode) -> FakeCtx<I> {
    FakeCtx::with_default_bindings_ctx(|bindings_ctx| {
        let mut core_ctx = FakeGmpContext {
            inner: FakeGmpContextInner::default(),
            enabled: true,
            groups: Default::default(),
            gmp: GmpState::new::<_, IntoCoreTimerCtx>(bindings_ctx, FakeWeakDeviceId(FakeDeviceId)),
            config: Default::default(),
        };
        core_ctx.gmp.mode = mode;
        core_ctx
    })
}

/// Extension trait so IP-independent tests can be written.
pub(super) trait TestIpExt: IpExt {
    const GROUP_ADDR1: MulticastAddr<Self::Addr>;
    const GROUP_ADDR2: MulticastAddr<Self::Addr>;
}

impl TestIpExt for Ipv4 {
    const GROUP_ADDR1: MulticastAddr<Ipv4Addr> =
        unsafe { MulticastAddr::new_unchecked(net_ip_v4!("224.0.0.4")) };
    const GROUP_ADDR2: MulticastAddr<Ipv4Addr> =
        unsafe { MulticastAddr::new_unchecked(net_ip_v4!("224.0.0.5")) };
}

impl TestIpExt for Ipv6 {
    const GROUP_ADDR1: MulticastAddr<Ipv6Addr> =
        unsafe { MulticastAddr::new_unchecked(net_ip_v6!("ff02::AA")) };
    const GROUP_ADDR2: MulticastAddr<Ipv6Addr> =
        unsafe { MulticastAddr::new_unchecked(net_ip_v6!("ff02::AB")) };
}
