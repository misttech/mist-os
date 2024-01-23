// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use lock_order::{lock::RwLockFor, relation::LockBefore, wrap::prelude::*};
use packet_formats::ip::IpExt;

use crate::{
    filter::{FilterBindingsTypes, FilterContext, FilterHandler, FilterImpl, State},
    BindingsContext, CoreCtx, StackState,
};

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
    FilterContext<I, BC> for CoreCtx<'_, BC, L>
{
    fn with_filter_state<O, F: FnOnce(&State<I, BC::DeviceClass>) -> O>(&mut self, cb: F) -> O {
        let state = self.read_lock::<crate::lock_ordering::FilterState<I>>();
        cb(&state)
    }
}

impl<I: IpExt, BC: BindingsContext> RwLockFor<crate::lock_ordering::FilterState<I>>
    for StackState<BC>
{
    type Data = State<I, BC::DeviceClass>;

    type ReadGuard<'l> = crate::sync::RwLockReadGuard<'l, State<I, BC::DeviceClass>>
    where
        Self: 'l;

    type WriteGuard<'l> = crate::sync::RwLockWriteGuard<'l, State<I, BC::DeviceClass>>
    where
        Self: 'l;

    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.filter().read()
    }

    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.filter().write()
    }
}
