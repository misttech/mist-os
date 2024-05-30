// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implementations of traits defined in foreign modules for the types defined
//! in the device socket module.

use lock_order::{
    lock::{DelegatedOrderedLockAccess, LockLevelFor},
    relation::LockBefore,
    wrap::prelude::*,
};

use crate::{
    device::{
        self, for_any_device_id,
        socket::{
            AllSockets, AnyDeviceSockets, DeviceSocketAccessor, DeviceSocketContext,
            DeviceSocketContextTypes, DeviceSocketId, DeviceSockets, HeldSockets,
            PrimaryDeviceSocketId, SocketStateAccessor, Target,
        },
        DeviceId, WeakDeviceId,
    },
    BindingsContext, BindingsTypes, CoreCtx, StackState,
};

impl<BT: BindingsTypes, L> DeviceSocketContextTypes<BT> for CoreCtx<'_, BT, L> {
    type SocketId = DeviceSocketId<BT::SocketState, WeakDeviceId<BT>>;
    fn new_primary(
        external_state: BT::SocketState,
    ) -> PrimaryDeviceSocketId<BT::SocketState, WeakDeviceId<BT>> {
        PrimaryDeviceSocketId::new(external_state)
    }
    fn unwrap_primary(
        primary: PrimaryDeviceSocketId<BT::SocketState, WeakDeviceId<BT>>,
    ) -> BT::SocketState {
        primary.unwrap().external_state
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::AllDeviceSockets>>
    DeviceSocketContext<BC> for CoreCtx<'_, BC, L>
{
    type SocketTablesCoreCtx<'a> = CoreCtx<'a, BC, crate::lock_ordering::AnyDeviceSockets>;

    fn with_all_device_sockets_mut<F: FnOnce(&mut AllSockets<Self::SocketId>) -> R, R>(
        &mut self,
        cb: F,
    ) -> R {
        let mut locked = self.lock::<crate::lock_ordering::AllDeviceSockets>();
        cb(&mut locked)
    }

    fn with_any_device_sockets<
        F: FnOnce(&AnyDeviceSockets<Self::SocketId>, &mut Self::SocketTablesCoreCtx<'_>) -> R,
        R,
    >(
        &mut self,
        cb: F,
    ) -> R {
        let (sockets, mut locked) = self.read_lock_and::<crate::lock_ordering::AnyDeviceSockets>();
        cb(&*sockets, &mut locked)
    }

    fn with_any_device_sockets_mut<
        F: FnOnce(&mut AnyDeviceSockets<Self::SocketId>, &mut Self::SocketTablesCoreCtx<'_>) -> R,
        R,
    >(
        &mut self,
        cb: F,
    ) -> R {
        let (mut sockets, mut locked) =
            self.write_lock_and::<crate::lock_ordering::AnyDeviceSockets>();
        cb(&mut *sockets, &mut locked)
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::DeviceSocketState>>
    SocketStateAccessor<BC> for CoreCtx<'_, BC, L>
{
    fn with_socket_state<F: FnOnce(&BC::SocketState, &Target<Self::WeakDeviceId>) -> R, R>(
        &mut self,
        id: &Self::SocketId,
        cb: F,
    ) -> R {
        let external_state = id.socket_state();
        let mut locked = self.adopt(id);
        let guard = locked.lock_with::<crate::lock_ordering::DeviceSocketState, _>(|c| c.right());
        cb(external_state, &*guard)
    }

    fn with_socket_state_mut<
        F: FnOnce(&BC::SocketState, &mut Target<Self::WeakDeviceId>) -> R,
        R,
    >(
        &mut self,
        id: &Self::SocketId,
        cb: F,
    ) -> R {
        let external_state = id.socket_state();
        let mut locked = self.adopt(id);
        let mut guard =
            locked.lock_with::<crate::lock_ordering::DeviceSocketState, _>(|c| c.right());
        cb(external_state, &mut *guard)
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::DeviceSockets>>
    DeviceSocketAccessor<BC> for CoreCtx<'_, BC, L>
{
    type DeviceSocketCoreCtx<'a> = CoreCtx<'a, BC, crate::lock_ordering::DeviceSockets>;

    fn with_device_sockets<
        F: FnOnce(&DeviceSockets<Self::SocketId>, &mut Self::DeviceSocketCoreCtx<'_>) -> R,
        R,
    >(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> R {
        for_any_device_id!(
            DeviceId,
            device,
            device => device::integration::with_device_state_and_core_ctx(
                self,
                device,
                |mut core_ctx_and_resource| {
                    let (device_sockets, mut locked) = core_ctx_and_resource
                        .read_lock_with_and::<crate::lock_ordering::DeviceSockets, _>(
                        |c| c.right(),
                    );
                    cb(&*device_sockets, &mut locked.cast_core_ctx())
                },
            )
        )
    }

    fn with_device_sockets_mut<
        F: FnOnce(&mut DeviceSockets<Self::SocketId>, &mut Self::DeviceSocketCoreCtx<'_>) -> R,
        R,
    >(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> R {
        for_any_device_id!(
            DeviceId,
            device,
            device => device::integration::with_device_state_and_core_ctx(
                self,
                device,
                |mut core_ctx_and_resource| {
                    let (mut device_sockets, mut locked) = core_ctx_and_resource
                        .write_lock_with_and::<crate::lock_ordering::DeviceSockets, _>(
                        |c| c.right(),
                    );
                    cb(&mut *device_sockets, &mut locked.cast_core_ctx())
                },
            )
        )
    }
}

impl<BT: BindingsTypes>
    DelegatedOrderedLockAccess<AnyDeviceSockets<DeviceSocketId<BT::SocketState, WeakDeviceId<BT>>>>
    for StackState<BT>
{
    type Inner = HeldSockets<BT>;
    fn delegate_ordered_lock_access(&self) -> &Self::Inner {
        &self.device.shared_sockets
    }
}

impl<BT: BindingsTypes>
    DelegatedOrderedLockAccess<AllSockets<DeviceSocketId<BT::SocketState, WeakDeviceId<BT>>>>
    for StackState<BT>
{
    type Inner = HeldSockets<BT>;
    fn delegate_ordered_lock_access(&self) -> &Self::Inner {
        &self.device.shared_sockets
    }
}

impl<BT: BindingsTypes> LockLevelFor<StackState<BT>> for crate::lock_ordering::AnyDeviceSockets {
    type Data = AnyDeviceSockets<DeviceSocketId<BT::SocketState, WeakDeviceId<BT>>>;
}

impl<BT: BindingsTypes> LockLevelFor<StackState<BT>> for crate::lock_ordering::AllDeviceSockets {
    type Data = AllSockets<DeviceSocketId<BT::SocketState, WeakDeviceId<BT>>>;
}

impl<BT: BindingsTypes> LockLevelFor<DeviceSocketId<BT::SocketState, WeakDeviceId<BT>>>
    for crate::lock_ordering::DeviceSocketState
{
    type Data = Target<WeakDeviceId<BT>>;
}
