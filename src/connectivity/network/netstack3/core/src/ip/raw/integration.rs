// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implementations for raw IP sockets that integrate with traits/types from
//! foreign modules.

use lock_order::{lock::LockLevelFor, relation::LockBefore, wrap::LockedWrapperApi};

use crate::{
    ip::{
        base::IpLayerIpExt,
        raw::{
            RawIpSocketId, RawIpSocketLockedState, RawIpSocketMap, RawIpSocketMapContext,
            RawIpSocketState, RawIpSocketStateContext,
        },
    },
    lock_ordering, BindingsTypes, CoreCtx,
};

impl<I: IpLayerIpExt, BT: BindingsTypes, L: LockBefore<lock_ordering::RawIpSocketState<I>>>
    RawIpSocketStateContext<I, BT> for CoreCtx<'_, BT, L>
{
    fn with_locked_state<O, F: FnOnce(&RawIpSocketLockedState<I>) -> O>(
        &mut self,
        RawIpSocketId(state_rc): &RawIpSocketId<I, BT>,
        cb: F,
    ) -> O {
        let mut locked = self.adopt(&**state_rc);
        let guard = locked.read_lock_with::<lock_ordering::RawIpSocketState<I>, _>(|c| c.right());
        cb(&guard)
    }
    fn with_locked_state_mut<O, F: FnOnce(&mut RawIpSocketLockedState<I>) -> O>(
        &mut self,
        RawIpSocketId(state_rc): &RawIpSocketId<I, BT>,
        cb: F,
    ) -> O {
        let mut locked = self.adopt(&**state_rc);
        let mut guard =
            locked.write_lock_with::<lock_ordering::RawIpSocketState<I>, _>(|c| c.right());
        cb(&mut guard)
    }
}

impl<I: IpLayerIpExt, BT: BindingsTypes, L: LockBefore<lock_ordering::AllRawIpSockets<I>>>
    RawIpSocketMapContext<I, BT> for CoreCtx<'_, BT, L>
{
    fn with_socket_map<O, F: FnOnce(&RawIpSocketMap<I, BT>) -> O>(&mut self, cb: F) -> O {
        let sockets = self.read_lock::<lock_ordering::AllRawIpSockets<I>>();
        cb(&sockets)
    }
    fn with_socket_map_mut<O, F: FnOnce(&mut RawIpSocketMap<I, BT>) -> O>(&mut self, cb: F) -> O {
        let mut sockets = self.write_lock::<lock_ordering::AllRawIpSockets<I>>();
        cb(&mut sockets)
    }
}

impl<I: IpLayerIpExt, BT: BindingsTypes> LockLevelFor<RawIpSocketState<I, BT>>
    for lock_ordering::RawIpSocketState<I>
{
    type Data = RawIpSocketLockedState<I>;
}
