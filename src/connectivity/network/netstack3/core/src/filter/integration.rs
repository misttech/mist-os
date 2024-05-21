// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use lock_order::{lock::RwLockFor, relation::LockBefore, wrap::prelude::*};
use net_types::ip::{Ipv4, Ipv6};
use packet_formats::ip::IpExt;

use crate::{
    filter::{
        FilterBindingsContext, FilterContext, FilterHandler, FilterImpl, FilterIpContext, State,
    },
    BindingsContext, CoreCtx, StackState,
};

pub trait FilterHandlerProvider<I: IpExt, BC: FilterBindingsContext> {
    type Handler<'a>: FilterHandler<I, BC>
    where
        Self: 'a;

    fn filter_handler(&mut self) -> Self::Handler<'_>;
}

impl<'a, I: IpExt, BC: BindingsContext, L: LockBefore<crate::lock_ordering::FilterState<I>>>
    FilterHandlerProvider<I, BC> for CoreCtx<'a, BC, L>
{
    type Handler<'b> = FilterImpl<'b, CoreCtx<'a, BC, L>> where Self: 'b;

    fn filter_handler(&mut self) -> Self::Handler<'_> {
        FilterImpl(self)
    }
}

impl<I: IpExt, BC: BindingsContext, L: LockBefore<crate::lock_ordering::FilterState<I>>>
    FilterIpContext<I, BC> for CoreCtx<'_, BC, L>
{
    fn with_filter_state<O, F: FnOnce(&State<I, BC>) -> O>(&mut self, cb: F) -> O {
        let state = self.read_lock::<crate::lock_ordering::FilterState<I>>();
        cb(&state)
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
