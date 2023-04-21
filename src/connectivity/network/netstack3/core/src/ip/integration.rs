// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The integrations for protocols built on top of IP.

use lock_order::{relation::LockBefore, Locked};
use net_types::{
    ip::{Ip, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr},
    MulticastAddr, SpecifiedAddr,
};
use packet::{BufferMut, Serializer};

use crate::{
    context::NonTestCtxMarker,
    ip::{
        self,
        device::{self, IpDeviceIpExt, IpDeviceNonSyncContext},
        path_mtu::{PmtuCache, PmtuStateContext},
        reassembly::FragmentStateContext,
        send_ipv4_packet_from_device, send_ipv6_packet_from_device,
        socket::{BufferIpSocketContext, IpSocketContext, IpSocketNonSyncContext},
        AnyDevice, DeviceIdContext, IpLayerNonSyncContext, IpPacketFragmentCache, IpStateContext,
        Ipv4StateContext, MulticastMembershipHandler, SendIpPacketMeta,
    },
    NonSyncContext, SyncCtx,
};

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::IpStateFragmentCache<Ipv4>>>
    FragmentStateContext<Ipv4, C::Instant> for Locked<&SyncCtx<C>, L>
{
    fn with_state_mut<O, F: FnOnce(&mut IpPacketFragmentCache<Ipv4, C::Instant>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        let mut cache = self.lock::<crate::lock_ordering::IpStateFragmentCache<Ipv4>>();
        cb(&mut cache)
    }
}

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::IpStateFragmentCache<Ipv6>>>
    FragmentStateContext<Ipv6, C::Instant> for Locked<&SyncCtx<C>, L>
{
    fn with_state_mut<O, F: FnOnce(&mut IpPacketFragmentCache<Ipv6, C::Instant>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        let mut cache = self.lock::<crate::lock_ordering::IpStateFragmentCache<Ipv6>>();
        cb(&mut cache)
    }
}

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::IpStatePmtuCache<Ipv4>>>
    PmtuStateContext<Ipv4, C::Instant> for Locked<&SyncCtx<C>, L>
{
    fn with_state_mut<O, F: FnOnce(&mut PmtuCache<Ipv4, C::Instant>) -> O>(&mut self, cb: F) -> O {
        let mut cache = self.lock::<crate::lock_ordering::IpStatePmtuCache<Ipv4>>();
        cb(&mut cache)
    }
}

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::IpStatePmtuCache<Ipv6>>>
    PmtuStateContext<Ipv6, C::Instant> for Locked<&SyncCtx<C>, L>
{
    fn with_state_mut<O, F: FnOnce(&mut PmtuCache<Ipv6, C::Instant>) -> O>(&mut self, cb: F) -> O {
        let mut cache = self.lock::<crate::lock_ordering::IpStatePmtuCache<Ipv6>>();
        cb(&mut cache)
    }
}

impl<
        B: BufferMut,
        C: IpSocketNonSyncContext
            + IpLayerNonSyncContext<Ipv4, <SC as DeviceIdContext<AnyDevice>>::DeviceId>,
        SC: ip::BufferIpDeviceContext<Ipv4, C, B>
            + Ipv4StateContext<C>
            + IpSocketContext<Ipv4, C>
            + NonTestCtxMarker,
    > BufferIpSocketContext<Ipv4, C, B> for SC
{
    fn send_ip_packet<S: Serializer<Buffer = B>>(
        &mut self,
        ctx: &mut C,
        meta: SendIpPacketMeta<
            Ipv4,
            &<SC as DeviceIdContext<AnyDevice>>::DeviceId,
            SpecifiedAddr<Ipv4Addr>,
        >,
        body: S,
    ) -> Result<(), S> {
        send_ipv4_packet_from_device(self, ctx, meta.into(), body)
    }
}

impl<
        B: BufferMut,
        C: IpSocketNonSyncContext
            + IpLayerNonSyncContext<Ipv6, <SC as DeviceIdContext<AnyDevice>>::DeviceId>,
        SC: ip::BufferIpDeviceContext<Ipv6, C, B>
            + IpStateContext<Ipv6, C>
            + IpSocketContext<Ipv6, C>
            + NonTestCtxMarker,
    > BufferIpSocketContext<Ipv6, C, B> for SC
{
    fn send_ip_packet<S: Serializer<Buffer = B>>(
        &mut self,
        ctx: &mut C,
        meta: SendIpPacketMeta<
            Ipv6,
            &<SC as DeviceIdContext<AnyDevice>>::DeviceId,
            SpecifiedAddr<Ipv6Addr>,
        >,
        body: S,
    ) -> Result<(), S> {
        send_ipv6_packet_from_device(self, ctx, meta.into(), body)
    }
}

impl<
        I: Ip + IpDeviceIpExt,
        C: NonSyncContext + IpDeviceNonSyncContext<I, Self::DeviceId>,
        L: LockBefore<crate::lock_ordering::IpState<I>>,
    > MulticastMembershipHandler<I, C> for Locked<&SyncCtx<C>, L>
where
    Self: device::IpDeviceConfigurationContext<I, C>,
{
    fn join_multicast_group(
        &mut self,
        ctx: &mut C,
        device: &Self::DeviceId,
        addr: MulticastAddr<I::Addr>,
    ) {
        crate::ip::device::join_ip_multicast::<I, _, _>(self, ctx, device, addr)
    }

    fn leave_multicast_group(
        &mut self,
        ctx: &mut C,
        device: &Self::DeviceId,
        addr: MulticastAddr<I::Addr>,
    ) {
        crate::ip::device::leave_ip_multicast::<I, _, _>(self, ctx, device, addr)
    }
}
