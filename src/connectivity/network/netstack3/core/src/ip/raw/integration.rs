// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implementations for raw IP sockets that integrate with traits/types from
//! foreign modules.

use lock_order::{lock::RwLockFor, relation::LockBefore, wrap::LockedWrapperApi};

use crate::{
    ip::{
        base::IpLayerIpExt,
        raw::{
            RawIpSocketId, RawIpSocketLockedState, RawIpSocketMap, RawIpSocketMapContext,
            RawIpSocketState, RawIpSocketStateContext,
        },
    },
    lock_ordering,
    sync::{RwLockReadGuard, RwLockWriteGuard},
    BindingsTypes, CoreCtx,
};

impl<I: IpLayerIpExt> RwLockFor<lock_ordering::RawIpSocketState<I>> for RawIpSocketState<I> {
    type Data = RawIpSocketLockedState<I>;
    type ReadGuard<'l> = RwLockReadGuard<'l, Self::Data>
        where Self: 'l;
    type WriteGuard<'l> = RwLockWriteGuard<'l, Self::Data>
        where Self: 'l;

    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.locked_state.read()
    }

    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.locked_state.write()
    }
}

impl<I: IpLayerIpExt, BT: BindingsTypes, L: LockBefore<lock_ordering::RawIpSocketState<I>>>
    RawIpSocketStateContext<I> for CoreCtx<'_, BT, L>
{
    fn with_locked_state<O, F: FnOnce(&RawIpSocketLockedState<I>) -> O>(
        &mut self,
        RawIpSocketId(state): RawIpSocketId<I>,
        cb: F,
    ) -> O {
        let locked_state = state.read_lock();
        cb(&locked_state)
    }
    fn with_locked_state_mut<O, F: FnOnce(&mut RawIpSocketLockedState<I>) -> O>(
        &mut self,
        RawIpSocketId(state): RawIpSocketId<I>,
        cb: F,
    ) -> O {
        let mut locked_state = state.write_lock();
        cb(&mut locked_state)
    }
}

impl<I: IpLayerIpExt, BT: BindingsTypes, L: LockBefore<lock_ordering::AllRawIpSockets<I>>>
    RawIpSocketMapContext<I> for CoreCtx<'_, BT, L>
{
    fn with_socket_map<O, F: FnOnce(&RawIpSocketMap<I>) -> O>(&mut self, cb: F) -> O {
        let sockets = self.read_lock();
        cb(&sockets)
    }
    fn with_socket_map_mut<O, F: FnOnce(&mut RawIpSocketMap<I>) -> O>(&mut self, cb: F) -> O {
        let mut sockets = self.write_lock();
        cb(&mut sockets)
    }
}
