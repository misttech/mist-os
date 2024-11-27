// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use lock_order::lock::{DelegatedOrderedLockAccess, LockLevelFor};
use lock_order::relation::LockBefore;
use net_types::ip::{Ip, Ipv4, Ipv6};
use net_types::SpecifiedAddr;
use netstack3_base::IpDeviceAddr;
use netstack3_device::DeviceId;
use netstack3_filter::{FilterContext, FilterImpl, FilterIpContext, NatContext, State};
use netstack3_ip::{FilterHandlerProvider, IpLayerIpExt, IpSasHandler, IpStateInner};

use crate::context::prelude::*;
use crate::context::WrapLockLevel;
use crate::{BindingsContext, BindingsTypes, CoreCtx, StackState};

#[netstack3_macros::instantiate_ip_impl_block(I)]
impl<'a, I: IpExt, BC: BindingsContext, L: LockBefore<crate::lock_ordering::FilterState<I>>>
    FilterHandlerProvider<I, BC> for CoreCtx<'a, BC, L>
{
    type Handler<'b>
        = FilterImpl<'b, CoreCtx<'a, BC, L>>
    where
        Self: 'b;

    fn filter_handler(&mut self) -> Self::Handler<'_> {
        FilterImpl(self)
    }
}

#[netstack3_macros::instantiate_ip_impl_block(I)]
impl<I: IpExt, BC: BindingsContext, L: LockBefore<crate::lock_ordering::FilterState<I>>>
    FilterIpContext<I, BC> for CoreCtx<'_, BC, L>
{
    type NatCtx<'a> = CoreCtx<'a, BC, WrapLockLevel<crate::lock_ordering::FilterState<I>>>;

    fn with_filter_state_and_nat_ctx<O, F: FnOnce(&State<I, BC>, &mut Self::NatCtx<'_>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        let (state, mut locked) = self.read_lock_and::<crate::lock_ordering::FilterState<I>>();
        cb(&state, &mut locked)
    }
}

#[netstack3_macros::instantiate_ip_impl_block(I)]
impl<I: IpExt, BC: BindingsContext, L: LockBefore<crate::lock_ordering::IpState<I>>>
    NatContext<I, BC> for CoreCtx<'_, BC, L>
{
    fn get_local_addr_for_remote(
        &mut self,
        device_id: &Self::DeviceId,
        remote: Option<SpecifiedAddr<<I as Ip>::Addr>>,
    ) -> Option<IpDeviceAddr<<I as Ip>::Addr>> {
        IpSasHandler::<I, _>::get_local_addr_for_remote(self, device_id, remote)
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::FilterState<Ipv4>>> FilterContext<BC>
    for CoreCtx<'_, BC, L>
{
    fn with_all_filter_state_mut<O, F: FnOnce(&mut State<Ipv4, BC>, &mut State<Ipv6, BC>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        let (mut v4, mut locked) = self.write_lock_and::<crate::lock_ordering::FilterState<Ipv4>>();
        let mut v6 = locked.write_lock::<crate::lock_ordering::FilterState<Ipv6>>();
        cb(&mut v4, &mut v6)
    }
}

impl<I: IpLayerIpExt, BT: BindingsTypes> DelegatedOrderedLockAccess<State<I, BT>>
    for StackState<BT>
{
    type Inner = IpStateInner<I, DeviceId<BT>, BT>;
    fn delegate_ordered_lock_access(&self) -> &Self::Inner {
        self.inner_ip_state()
    }
}

impl<I: IpLayerIpExt, BT: BindingsTypes> LockLevelFor<StackState<BT>>
    for crate::lock_ordering::FilterState<I>
{
    type Data = State<I, BT>;
}
