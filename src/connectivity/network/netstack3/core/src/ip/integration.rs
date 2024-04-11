// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The integrations for protocols built on top of IP.

use lock_order::{relation::LockBefore, wrap::prelude::*};
use net_types::{
    ip::{Ip, Ipv4, Ipv6},
    MulticastAddr,
};

use crate::{
    ip::{
        device::{self, IpDeviceBindingsContext, IpDeviceIpExt},
        path_mtu::{PmtuCache, PmtuContext},
        reassembly::{FragmentContext, IpPacketFragmentCache},
        IpLayerBindingsContext, IpLayerContext, IpLayerIpExt, MulticastMembershipHandler,
        ResolveRouteError,
    },
    routes::ResolvedRoute,
    socket::address::SocketIpAddr,
    BindingsContext, BindingsTypes, CoreCtx,
};

impl<I, BT, L> FragmentContext<I, BT> for CoreCtx<'_, BT, L>
where
    I: IpLayerIpExt,
    BT: BindingsTypes,
    L: LockBefore<crate::lock_ordering::IpStateFragmentCache<I>>,
{
    fn with_state_mut<O, F: FnOnce(&mut IpPacketFragmentCache<I, BT>) -> O>(&mut self, cb: F) -> O {
        let mut cache = self.lock::<crate::lock_ordering::IpStateFragmentCache<I>>();
        cb(&mut cache)
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::IpStatePmtuCache<Ipv4>>>
    PmtuContext<Ipv4, BC> for CoreCtx<'_, BC, L>
{
    fn with_state_mut<O, F: FnOnce(&mut PmtuCache<Ipv4, BC>) -> O>(&mut self, cb: F) -> O {
        let mut cache = self.lock::<crate::lock_ordering::IpStatePmtuCache<Ipv4>>();
        cb(&mut cache)
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::IpStatePmtuCache<Ipv6>>>
    PmtuContext<Ipv6, BC> for CoreCtx<'_, BC, L>
{
    fn with_state_mut<O, F: FnOnce(&mut PmtuCache<Ipv6, BC>) -> O>(&mut self, cb: F) -> O {
        let mut cache = self.lock::<crate::lock_ordering::IpStatePmtuCache<Ipv6>>();
        cb(&mut cache)
    }
}

impl<
        I: Ip + IpDeviceIpExt + IpLayerIpExt,
        BC: BindingsContext
            + IpDeviceBindingsContext<I, Self::DeviceId>
            + IpLayerBindingsContext<I, Self::DeviceId>,
        L: LockBefore<crate::lock_ordering::IpState<I>>,
    > MulticastMembershipHandler<I, BC> for CoreCtx<'_, BC, L>
where
    Self: device::IpDeviceConfigurationContext<I, BC> + IpLayerContext<I, BC>,
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

    fn select_device_for_multicast_group(
        &mut self,
        addr: MulticastAddr<I::Addr>,
    ) -> Result<Self::DeviceId, ResolveRouteError> {
        let remote_ip = SocketIpAddr::new_from_multicast(addr);
        let ResolvedRoute { src_addr: _, device, local_delivery_device, next_hop: _ } =
            crate::ip::resolve_route_to_destination(self, None, None, Some(remote_ip))?;
        // NB: Because the original address is multicast, it cannot be assigned
        // to a local interface. Thus local delivery should never be requested.
        debug_assert!(local_delivery_device.is_none(), "{:?}", local_delivery_device);
        Ok(device)
    }
}
