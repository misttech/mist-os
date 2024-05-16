// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implementations of traits defined in foreign modules for the types defined
//! in the device socket module.

use dense_map::EntryKey;
use lock_order::{
    lock::{DelegatedOrderedLockAccess, LockLevelFor},
    relation::LockBefore,
    wrap::prelude::*,
};

use crate::{
    device::{
        self,
        socket::{
            AllSockets, AnyDeviceSockets, DeviceSocketAccessor, DeviceSocketContext,
            DeviceSocketContextTypes, DeviceSockets, HeldSockets, PrimaryId, SocketState,
            SocketStateAccessor, StrongId, Target,
        },
        DeviceId, WeakDeviceId,
    },
    for_any_device_id,
    sync::{Mutex, PrimaryRc},
    BindingsContext, BindingsTypes, CoreCtx, StackState,
};

impl<BC: BindingsContext, L> DeviceSocketContextTypes for CoreCtx<'_, BC, L> {
    type SocketId = StrongId<BC::SocketState, WeakDeviceId<BC>>;
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::AllDeviceSockets>>
    DeviceSocketContext<BC> for CoreCtx<'_, BC, L>
{
    type SocketTablesCoreCtx<'a> = CoreCtx<'a, BC, crate::lock_ordering::AnyDeviceSockets>;

    fn create_socket(&mut self, state: BC::SocketState) -> Self::SocketId {
        let mut sockets = self.lock();
        let AllSockets(sockets) = &mut *sockets;
        let entry = sockets.push_with(|index| {
            PrimaryId(PrimaryRc::new(SocketState {
                all_sockets_index: index,
                external_state: state,
                target: Mutex::new(Target::default()),
            }))
        });
        let PrimaryId(primary) = &entry.get();
        StrongId(PrimaryRc::clone_strong(primary))
    }

    fn remove_socket(&mut self, socket: Self::SocketId) {
        let mut state = self.lock();
        let AllSockets(sockets) = &mut *state;

        let PrimaryId(primary) = sockets.remove(socket.get_key_index()).expect("unknown socket ID");
        // Make sure to drop the strong ID before trying to unwrap the primary
        // ID.
        drop(socket);

        let _: SocketState<_, _> = PrimaryRc::unwrap(primary);
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
        StrongId(strong): &Self::SocketId,
        cb: F,
    ) -> R {
        let SocketState { external_state, target, all_sockets_index: _ } = &**strong;
        cb(external_state, &*target.lock())
    }

    fn with_socket_state_mut<
        F: FnOnce(&BC::SocketState, &mut Target<Self::WeakDeviceId>) -> R,
        R,
    >(
        &mut self,
        StrongId(primary): &Self::SocketId,
        cb: F,
    ) -> R {
        let SocketState { external_state, target, all_sockets_index: _ } = &**primary;
        cb(external_state, &mut *target.lock())
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
    DelegatedOrderedLockAccess<AnyDeviceSockets<StrongId<BT::SocketState, WeakDeviceId<BT>>>>
    for StackState<BT>
{
    type Inner = HeldSockets<BT>;
    fn delegate_ordered_lock_access(&self) -> &Self::Inner {
        &self.device.shared_sockets
    }
}

impl<BT: BindingsTypes>
    DelegatedOrderedLockAccess<AllSockets<PrimaryId<BT::SocketState, WeakDeviceId<BT>>>>
    for StackState<BT>
{
    type Inner = HeldSockets<BT>;
    fn delegate_ordered_lock_access(&self) -> &Self::Inner {
        &self.device.shared_sockets
    }
}

impl<BT: BindingsTypes> LockLevelFor<StackState<BT>> for crate::lock_ordering::AnyDeviceSockets {
    type Data = AnyDeviceSockets<StrongId<BT::SocketState, WeakDeviceId<BT>>>;
}

impl<BT: BindingsTypes> LockLevelFor<StackState<BT>> for crate::lock_ordering::AllDeviceSockets {
    type Data = AllSockets<PrimaryId<BT::SocketState, WeakDeviceId<BT>>>;
}
