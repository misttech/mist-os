// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implementations for raw IP sockets that integrate with traits/types from
//! foreign modules.

use lock_order::lock::{LockLevelFor, UnlockedAccess};
use lock_order::relation::LockBefore;
use lock_order::wrap::{LockedWrapperApi, LockedWrapperUnlockedApi};
use netstack3_base::{CounterContext, ResourceCounterContext, WeakDeviceIdentifier};
use netstack3_device::WeakDeviceId;
use netstack3_ip::raw::{
    RawIpSocketCounters, RawIpSocketId, RawIpSocketLockedState, RawIpSocketMap,
    RawIpSocketMapContext, RawIpSocketState, RawIpSocketStateContext,
};

use crate::marker::IpExt;
use crate::{lock_ordering, BindingsContext, BindingsTypes, CoreCtx, StackState};

#[netstack3_macros::instantiate_ip_impl_block(I)]
impl<I: IpExt, BC: BindingsContext, L: LockBefore<lock_ordering::RawIpSocketState<I>>>
    RawIpSocketStateContext<I, BC> for CoreCtx<'_, BC, L>
{
    type SocketHandler<'a> = CoreCtx<'a, BC, lock_ordering::RawIpSocketState<I>>;

    fn with_locked_state<O, F: FnOnce(&RawIpSocketLockedState<I, Self::WeakDeviceId>) -> O>(
        &mut self,
        id: &RawIpSocketId<I, Self::WeakDeviceId, BC>,
        cb: F,
    ) -> O {
        let mut locked = self.adopt(id.state());
        let guard = locked.read_lock_with::<lock_ordering::RawIpSocketState<I>, _>(|c| c.right());
        cb(&guard)
    }
    fn with_locked_state_and_socket_handler<
        O,
        F: FnOnce(&RawIpSocketLockedState<I, Self::WeakDeviceId>, &mut Self::SocketHandler<'_>) -> O,
    >(
        &mut self,
        id: &RawIpSocketId<I, Self::WeakDeviceId, BC>,
        cb: F,
    ) -> O {
        let mut locked = self.adopt(id.state());
        let (state, mut core_ctx) =
            locked.read_lock_with_and::<lock_ordering::RawIpSocketState<I>, _>(|c| c.right());
        let mut core_ctx = core_ctx.cast_core_ctx();
        cb(&state, &mut core_ctx)
    }
    fn with_locked_state_mut<
        O,
        F: FnOnce(&mut RawIpSocketLockedState<I, Self::WeakDeviceId>) -> O,
    >(
        &mut self,
        id: &RawIpSocketId<I, Self::WeakDeviceId, BC>,
        cb: F,
    ) -> O {
        let mut locked = self.adopt(id.state());
        let mut guard =
            locked.write_lock_with::<lock_ordering::RawIpSocketState<I>, _>(|c| c.right());
        cb(&mut guard)
    }
}

#[netstack3_macros::instantiate_ip_impl_block(I)]
impl<I: IpExt, BC: BindingsContext, L: LockBefore<lock_ordering::AllRawIpSockets<I>>>
    RawIpSocketMapContext<I, BC> for CoreCtx<'_, BC, L>
{
    type StateCtx<'a> = CoreCtx<'a, BC, lock_ordering::AllRawIpSockets<I>>;

    fn with_socket_map_and_state_ctx<
        O,
        F: FnOnce(&RawIpSocketMap<I, Self::WeakDeviceId, BC>, &mut Self::StateCtx<'_>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (sockets, mut core_ctx) = self.read_lock_and::<lock_ordering::AllRawIpSockets<I>>();
        cb(&sockets, &mut core_ctx)
    }
    fn with_socket_map_mut<O, F: FnOnce(&mut RawIpSocketMap<I, Self::WeakDeviceId, BC>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        let mut sockets = self.write_lock::<lock_ordering::AllRawIpSockets<I>>();
        cb(&mut sockets)
    }
}

impl<I: IpExt, BT: BindingsTypes> LockLevelFor<RawIpSocketState<I, WeakDeviceId<BT>, BT>>
    for lock_ordering::RawIpSocketState<I>
{
    type Data = RawIpSocketLockedState<I, WeakDeviceId<BT>>;
}

impl<I: IpExt, BC: BindingsContext, L> CounterContext<RawIpSocketCounters<I>>
    for CoreCtx<'_, BC, L>
{
    fn with_counters<O, F: FnOnce(&RawIpSocketCounters<I>) -> O>(&self, cb: F) -> O {
        cb(self.unlocked_access::<crate::lock_ordering::RawIpSocketCounters<I>>())
    }
}

impl<BC: BindingsContext, I: IpExt> UnlockedAccess<crate::lock_ordering::RawIpSocketCounters<I>>
    for StackState<BC>
{
    type Data = RawIpSocketCounters<I>;
    type Guard<'l>
        = &'l RawIpSocketCounters<I>
    where
        Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        self.inner_ip_state().raw_ip_socket_counters()
    }
}

// NB: Implement for any `D` rather than `Self::WeakDeviceId` to avoid a
// circular dependency (e.g. referencing `Self` in the trait being implemented).
impl<I: IpExt, BC: BindingsContext, L, D: WeakDeviceIdentifier>
    ResourceCounterContext<RawIpSocketId<I, D, BC>, RawIpSocketCounters<I>> for CoreCtx<'_, BC, L>
{
    fn with_per_resource_counters<O, F: FnOnce(&RawIpSocketCounters<I>) -> O>(
        &mut self,
        id: &RawIpSocketId<I, D, BC>,
        cb: F,
    ) -> O {
        // NB: circumvent the lock ordering to access the counters, because it
        // spares jumping through some hoops, and
        // `crate::lock_ordering::RawIpSocketCounters<I>>` exists for unlocked
        // access.
        cb(id.state().counters())
    }
}
