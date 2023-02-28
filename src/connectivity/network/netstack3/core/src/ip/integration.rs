// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The integrations for protocols built on top of IP.

use lock_order::{relation::LockBefore, Locked};
use net_types::{
    ip::{Ip, IpInvariant, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr},
    MulticastAddr, SpecifiedAddr,
};
use packet::{BufferMut, Serializer};

use crate::{
    context::NonTestCtxMarker,
    ip::{
        self,
        path_mtu::{PmtuCache, PmtuStateContext},
        reassembly::FragmentStateContext,
        send_ipv4_packet_from_device, send_ipv6_packet_from_device,
        socket::{BufferIpSocketContext, IpSocketContext, IpSocketNonSyncContext},
        IpDeviceIdContext, IpLayerNonSyncContext, IpPacketFragmentCache, IpStateContext,
        Ipv4StateContext, MulticastMembershipHandler, SendIpPacketMeta,
    },
    NonSyncContext, SyncCtx,
};

impl<C: NonSyncContext> FragmentStateContext<Ipv4, C::Instant> for &'_ SyncCtx<C> {
    fn with_state_mut<O, F: FnOnce(&mut IpPacketFragmentCache<Ipv4, C::Instant>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        FragmentStateContext::<Ipv4, _>::with_state_mut(&mut Locked::new(*self), cb)
    }
}

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::IpStateFragmentCache<Ipv4>>>
    FragmentStateContext<Ipv4, C::Instant> for Locked<'_, SyncCtx<C>, L>
{
    fn with_state_mut<O, F: FnOnce(&mut IpPacketFragmentCache<Ipv4, C::Instant>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        let mut cache = self.lock::<crate::lock_ordering::IpStateFragmentCache<Ipv4>>();
        cb(&mut cache)
    }
}

impl<C: NonSyncContext> FragmentStateContext<Ipv6, C::Instant> for &'_ SyncCtx<C> {
    fn with_state_mut<O, F: FnOnce(&mut IpPacketFragmentCache<Ipv6, C::Instant>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        FragmentStateContext::<Ipv6, _>::with_state_mut(&mut Locked::new(*self), cb)
    }
}

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::IpStateFragmentCache<Ipv6>>>
    FragmentStateContext<Ipv6, C::Instant> for Locked<'_, SyncCtx<C>, L>
{
    fn with_state_mut<O, F: FnOnce(&mut IpPacketFragmentCache<Ipv6, C::Instant>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        let mut cache = self.lock::<crate::lock_ordering::IpStateFragmentCache<Ipv6>>();
        cb(&mut cache)
    }
}

impl<C: NonSyncContext> PmtuStateContext<Ipv4, C::Instant> for &'_ SyncCtx<C> {
    fn with_state_mut<O, F: FnOnce(&mut PmtuCache<Ipv4, C::Instant>) -> O>(&mut self, cb: F) -> O {
        PmtuStateContext::<Ipv4, _>::with_state_mut(&mut Locked::new(*self), cb)
    }
}

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::IpStatePmtuCache<Ipv4>>>
    PmtuStateContext<Ipv4, C::Instant> for Locked<'_, SyncCtx<C>, L>
{
    fn with_state_mut<O, F: FnOnce(&mut PmtuCache<Ipv4, C::Instant>) -> O>(&mut self, cb: F) -> O {
        let mut cache = self.lock::<crate::lock_ordering::IpStatePmtuCache<Ipv4>>();
        cb(&mut cache)
    }
}

impl<C: NonSyncContext> PmtuStateContext<Ipv6, C::Instant> for &'_ SyncCtx<C> {
    fn with_state_mut<O, F: FnOnce(&mut PmtuCache<Ipv6, C::Instant>) -> O>(&mut self, cb: F) -> O {
        PmtuStateContext::<Ipv6, _>::with_state_mut(&mut Locked::new(*self), cb)
    }
}

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::IpStatePmtuCache<Ipv6>>>
    PmtuStateContext<Ipv6, C::Instant> for Locked<'_, SyncCtx<C>, L>
{
    fn with_state_mut<O, F: FnOnce(&mut PmtuCache<Ipv6, C::Instant>) -> O>(&mut self, cb: F) -> O {
        let mut cache = self.lock::<crate::lock_ordering::IpStatePmtuCache<Ipv6>>();
        cb(&mut cache)
    }
}

impl<
        B: BufferMut,
        C: IpSocketNonSyncContext
            + IpLayerNonSyncContext<Ipv4, <SC as IpDeviceIdContext<Ipv4>>::DeviceId>,
        SC: ip::BufferIpDeviceContext<Ipv4, C, B>
            + Ipv4StateContext<C::Instant>
            + IpSocketContext<Ipv4, C>
            + NonTestCtxMarker,
    > BufferIpSocketContext<Ipv4, C, B> for SC
{
    fn send_ip_packet<S: Serializer<Buffer = B>>(
        &mut self,
        ctx: &mut C,
        meta: SendIpPacketMeta<
            Ipv4,
            &<SC as IpDeviceIdContext<Ipv4>>::DeviceId,
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
            + IpLayerNonSyncContext<Ipv6, <SC as IpDeviceIdContext<Ipv6>>::DeviceId>,
        SC: ip::BufferIpDeviceContext<Ipv6, C, B>
            + IpStateContext<Ipv6, C::Instant>
            + IpSocketContext<Ipv6, C>
            + NonTestCtxMarker,
    > BufferIpSocketContext<Ipv6, C, B> for SC
{
    fn send_ip_packet<S: Serializer<Buffer = B>>(
        &mut self,
        ctx: &mut C,
        meta: SendIpPacketMeta<
            Ipv6,
            &<SC as IpDeviceIdContext<Ipv6>>::DeviceId,
            SpecifiedAddr<Ipv6Addr>,
        >,
        body: S,
    ) -> Result<(), S> {
        send_ipv6_packet_from_device(self, ctx, meta.into(), body)
    }
}

impl<I: Ip, C: NonSyncContext> MulticastMembershipHandler<I, C> for &'_ SyncCtx<C> {
    fn join_multicast_group(
        &mut self,
        ctx: &mut C,
        device: &Self::DeviceId,
        addr: MulticastAddr<I::Addr>,
    ) {
        I::map_ip(
            (IpInvariant((self, ctx, device)), addr),
            |(IpInvariant((sync_ctx, ctx, device)), addr)| {
                crate::ip::device::join_ip_multicast::<Ipv4, _, _>(sync_ctx, ctx, device, addr)
            },
            |(IpInvariant((sync_ctx, ctx, device)), addr)| {
                crate::ip::device::join_ip_multicast::<Ipv6, _, _>(sync_ctx, ctx, device, addr)
            },
        )
    }

    fn leave_multicast_group(
        &mut self,
        ctx: &mut C,
        device: &Self::DeviceId,
        addr: MulticastAddr<I::Addr>,
    ) {
        I::map_ip(
            (IpInvariant((self, ctx, device)), addr),
            |(IpInvariant((sync_ctx, ctx, device)), addr)| {
                crate::ip::device::leave_ip_multicast::<Ipv4, _, _>(sync_ctx, ctx, device, addr)
            },
            |(IpInvariant((sync_ctx, ctx, device)), addr)| {
                crate::ip::device::leave_ip_multicast::<Ipv6, _, _>(sync_ctx, ctx, device, addr)
            },
        )
    }
}
