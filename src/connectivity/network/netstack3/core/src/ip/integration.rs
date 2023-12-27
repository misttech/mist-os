// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The integrations for protocols built on top of IP.

use lock_order::{relation::LockBefore, Locked};
use net_types::{
    ip::{Ip, Ipv4, Ipv6},
    MulticastAddr,
};

use crate::{
    ip::{
        device::{self, IpDeviceIpExt, IpDeviceNonSyncContext},
        path_mtu::{PmtuCache, PmtuStateContext},
        reassembly::{FragmentStateContext, IpPacketFragmentCache},
        MulticastMembershipHandler,
    },
    NonSyncContext, SyncCtx,
};

impl<I, BC, L> FragmentStateContext<I, BC::Instant> for Locked<&SyncCtx<BC>, L>
where
    I: Ip,
    BC: NonSyncContext,
    L: LockBefore<crate::lock_ordering::IpStateFragmentCache<I>>,
{
    fn with_state_mut<O, F: FnOnce(&mut IpPacketFragmentCache<I, BC::Instant>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        let mut cache = self.lock::<crate::lock_ordering::IpStateFragmentCache<I>>();
        cb(&mut cache)
    }
}

impl<BC: NonSyncContext, L: LockBefore<crate::lock_ordering::IpStatePmtuCache<Ipv4>>>
    PmtuStateContext<Ipv4, BC::Instant> for Locked<&SyncCtx<BC>, L>
{
    fn with_state_mut<O, F: FnOnce(&mut PmtuCache<Ipv4, BC::Instant>) -> O>(&mut self, cb: F) -> O {
        let mut cache = self.lock::<crate::lock_ordering::IpStatePmtuCache<Ipv4>>();
        cb(&mut cache)
    }
}

impl<BC: NonSyncContext, L: LockBefore<crate::lock_ordering::IpStatePmtuCache<Ipv6>>>
    PmtuStateContext<Ipv6, BC::Instant> for Locked<&SyncCtx<BC>, L>
{
    fn with_state_mut<O, F: FnOnce(&mut PmtuCache<Ipv6, BC::Instant>) -> O>(&mut self, cb: F) -> O {
        let mut cache = self.lock::<crate::lock_ordering::IpStatePmtuCache<Ipv6>>();
        cb(&mut cache)
    }
}

impl<
        I: Ip + IpDeviceIpExt,
        BC: NonSyncContext + IpDeviceNonSyncContext<I, Self::DeviceId>,
        L: LockBefore<crate::lock_ordering::IpState<I>>,
    > MulticastMembershipHandler<I, BC> for Locked<&SyncCtx<BC>, L>
where
    Self: device::IpDeviceConfigurationContext<I, BC>,
{
    fn join_multicast_group(
        &mut self,
        bindings_ctx: &mut BC,
        device: &Self::DeviceId,
        addr: MulticastAddr<I::Addr>,
    ) {
        crate::ip::device::join_ip_multicast::<I, _, _>(self, bindings_ctx, device, addr)
    }

    fn leave_multicast_group(
        &mut self,
        bindings_ctx: &mut BC,
        device: &Self::DeviceId,
        addr: MulticastAddr<I::Addr>,
    ) {
        crate::ip::device::leave_ip_multicast::<I, _, _>(self, bindings_ctx, device, addr)
    }
}
