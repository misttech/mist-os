// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use lock_order::{lock::RwLockFor, relation::LockBefore, wrap::prelude::*};
use net_types::{
    ip::{Ip, Ipv4, Ipv6},
    NonMappedAddr, SpecifiedAddr,
};
use packet_formats::ip::IpExt;

use crate::{
    filter::{
        FilterBindingsContext, FilterContext, FilterHandler, FilterImpl, FilterIpContext,
        NatContext, State,
    },
    ip::IpDeviceStateContext,
    BindingsContext, CoreCtx, StackState,
};

pub trait FilterHandlerProvider<I: IpExt, BC: FilterBindingsContext> {
    type Handler<'a>: FilterHandler<I, BC>
    where
        Self: 'a;

    fn filter_handler(&mut self) -> Self::Handler<'_>;
}

#[netstack3_macros::instantiate_ip_impl_block(I)]
impl<'a, I: IpExt, BC: BindingsContext, L: LockBefore<crate::lock_ordering::FilterState<I>>>
    FilterHandlerProvider<I, BC> for CoreCtx<'a, BC, L>
{
    type Handler<'b> = FilterImpl<'b, CoreCtx<'a, BC, L>> where Self: 'b;

    fn filter_handler(&mut self) -> Self::Handler<'_> {
        FilterImpl(self)
    }
}

#[netstack3_macros::instantiate_ip_impl_block(I)]
impl<I: IpExt, BC: BindingsContext, L: LockBefore<crate::lock_ordering::FilterState<I>>>
    FilterIpContext<I, BC> for CoreCtx<'_, BC, L>
{
    type NatCtx<'a> = CoreCtx<'a, BC, crate::lock_ordering::FilterState<I>>;

    fn with_filter_state_and_nat_ctx<O, F: FnOnce(&State<I, BC>, &mut Self::NatCtx<'_>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        let (state, mut locked) = self.read_lock_and::<crate::lock_ordering::FilterState<I>>();
        cb(&state, &mut locked)
    }
}

#[netstack3_macros::instantiate_ip_impl_block(I)]
impl<I: IpExt, BC: BindingsContext, L: LockBefore<crate::lock_ordering::IpState<I>>> NatContext<I>
    for CoreCtx<'_, BC, L>
{
    fn get_local_addr_for_remote(
        &mut self,
        device_id: &Self::DeviceId,
        remote: Option<SpecifiedAddr<<I as Ip>::Addr>>,
    ) -> Option<NonMappedAddr<SpecifiedAddr<<I as Ip>::Addr>>> {
        IpDeviceStateContext::<I, BC>::get_local_addr_for_remote(self, device_id, remote)
            .map(|addr| addr.into_inner())
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

impl<I: IpExt, BC: BindingsContext> RwLockFor<crate::lock_ordering::FilterState<I>>
    for StackState<BC>
{
    type Data = State<I, BC>;

    type ReadGuard<'l> = crate::sync::RwLockReadGuard<'l, State<I, BC>>
    where
        Self: 'l;

    type WriteGuard<'l> = crate::sync::RwLockWriteGuard<'l, State<I, BC>>
    where
        Self: 'l;

    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.filter().read()
    }

    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.filter().write()
    }
}
