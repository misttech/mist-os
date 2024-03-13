// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use lock_order::{lock::RwLockFor, relation::LockBefore, wrap::prelude::*};
use net_types::ip::{Ipv4, Ipv6};
use packet_formats::ip::IpExt;

use crate::{
    filter::{
        FilterBindingsTypes, FilterContext, FilterHandler, FilterImpl, FilterIpContext, ValidState,
    },
    BindingsContext, CoreCtx, StackState,
};

// TODO(https://fxbug.dev/42080992): Clean this up once it's used.
#[allow(dead_code)]
pub(crate) trait FilterHandlerProvider<I: IpExt, BC: FilterBindingsTypes> {
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
    fn with_filter_state<O, F: FnOnce(&ValidState<I, BC::DeviceClass>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        let state = self.read_lock::<crate::lock_ordering::FilterState<I>>();
        cb(&state)
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::FilterState<Ipv4>>> FilterContext<BC>
    for CoreCtx<'_, BC, L>
{
    fn with_all_filter_state_mut<
        O,
        F: FnOnce(&mut ValidState<Ipv4, BC::DeviceClass>, &mut ValidState<Ipv6, BC::DeviceClass>) -> O,
    >(
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
    type Data = ValidState<I, BC::DeviceClass>;

    type ReadGuard<'l> = crate::sync::RwLockReadGuard<'l, ValidState<I, BC::DeviceClass>>
    where
        Self: 'l;

    type WriteGuard<'l> = crate::sync::RwLockWriteGuard<'l, ValidState<I, BC::DeviceClass>>
    where
        Self: 'l;

    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.filter().read()
    }

    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.filter().write()
    }
}
