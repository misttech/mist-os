// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implementations for raw IP sockets that integrate with traits/types from
//! foreign modules.

use lock_order::{lock::LockLevelFor, relation::LockBefore, wrap::LockedWrapperApi};

use crate::{
    device::WeakDeviceId,
    ip::raw::{
        RawIpSocketId, RawIpSocketLockedState, RawIpSocketMap, RawIpSocketMapContext,
        RawIpSocketState, RawIpSocketStateContext, RawIpSocketsIpExt,
    },
    lock_ordering, BindingsTypes, CoreCtx,
};

#[netstack3_macros::instantiate_ip_impl_block(I)]
impl<
        I: RawIpSocketsIpExt,
        BT: BindingsTypes,
        L: LockBefore<lock_ordering::RawIpSocketState<I>>,
    > RawIpSocketStateContext<I, BT> for CoreCtx<'_, BT, L>
{
    fn with_locked_state<O, F: FnOnce(&RawIpSocketLockedState<I, Self::WeakDeviceId>) -> O>(
        &mut self,
        id: &RawIpSocketId<I, Self::WeakDeviceId, BT>,
        cb: F,
    ) -> O {
        let mut locked = self.adopt(id.state());
        let guard = locked.read_lock_with::<lock_ordering::RawIpSocketState<I>, _>(|c| c.right());
        cb(&guard)
    }
    fn with_locked_state_mut<
        O,
        F: FnOnce(&mut RawIpSocketLockedState<I, Self::WeakDeviceId>) -> O,
    >(
        &mut self,
        id: &RawIpSocketId<I, Self::WeakDeviceId, BT>,
        cb: F,
    ) -> O {
        let mut locked = self.adopt(id.state());
        let mut guard =
            locked.write_lock_with::<lock_ordering::RawIpSocketState<I>, _>(|c| c.right());
        cb(&mut guard)
    }
}

#[netstack3_macros::instantiate_ip_impl_block(I)]
impl<I: RawIpSocketsIpExt, BT: BindingsTypes, L: LockBefore<lock_ordering::AllRawIpSockets<I>>>
    RawIpSocketMapContext<I, BT> for CoreCtx<'_, BT, L>
{
    fn with_socket_map<O, F: FnOnce(&RawIpSocketMap<I, Self::WeakDeviceId, BT>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        let sockets = self.read_lock::<lock_ordering::AllRawIpSockets<I>>();
        cb(&sockets)
    }
    fn with_socket_map_mut<O, F: FnOnce(&mut RawIpSocketMap<I, Self::WeakDeviceId, BT>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        let mut sockets = self.write_lock::<lock_ordering::AllRawIpSockets<I>>();
        cb(&mut sockets)
    }
}

impl<I: RawIpSocketsIpExt, BT: BindingsTypes>
    LockLevelFor<RawIpSocketState<I, WeakDeviceId<BT>, BT>> for lock_ordering::RawIpSocketState<I>
{
    type Data = RawIpSocketLockedState<I, WeakDeviceId<BT>>;
}
