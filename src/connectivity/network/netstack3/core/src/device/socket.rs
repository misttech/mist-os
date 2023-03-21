// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Link-layer sockets (analogous to Linux's AF_PACKET sockets).

use alloc::vec::Vec;
use core::{fmt::Debug, hash::Hash, num::NonZeroU16};

use derivative::Derivative;
use lock_order::{lock::RwLockFor, relation::LockBefore, Locked, Unlocked};

use crate::{
    data_structures::id_map::IdMap,
    device::{with_ethernet_state, with_loopback_state, DeviceId, WeakDeviceId},
    SyncCtx,
};

/// A selector for frames based on link-layer protocol number.
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub enum Protocol {
    /// Select all frames, regardless of protocol number.
    All,
    /// Select frames with the given protocol number.
    Specific(NonZeroU16),
}

/// Selector for devices to receive packets on.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum DeviceSelector<D> {
    /// Select packets received on any device.
    AnyDevice,
    /// Select packets received on the given device.
    SpecificDevice(D),
}

/// Non-sync context for packet sockets.
pub trait NonSyncContext {}

impl<C> NonSyncContext for C {}

/// Identifier for a socket.
///
/// This is intentionally not cloneable since each socket ID is a reference to
/// unique state held elsewhere.
#[derive(Debug, Hash, Eq, PartialEq)]
pub struct SocketId(usize);

/// Holds sockets that are not bound to a particular device.
///
/// Sockets are held in one of two places: in `AnyDeviceSockets` if they are not
/// bound to a particular device, or in the [`DeviceSockets`] instance for the
/// device to which they are bound.
#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub(crate) struct Sockets<D>(
    // TODO(https://fxbug.dev/84345): Make socket IDs shared references instead
    // of indexes into this state.
    IdMap<SocketState<D>>,
);

#[derive(Derivative)]
#[derivative(Default(bound = ""))]
struct SocketState<D> {
    protocol: Option<Protocol>,
    device: Option<D>,
}

/// Per-device state for packet sockets.
///
/// Holds sockets that are bound to a particular device. An instance of this
/// should be held in the state for each device in the system.
#[derive(Default)]
#[cfg_attr(test, derive(Debug, Eq, PartialEq))]
pub(crate) struct DeviceSockets(
    // TODO(https://fxbug.dev/84345): Make socket IDs shared references instead
    // of indexes into this state.
    Vec<usize>,
);

/// Temporary parallel implementation of [`crate::ip::IpDeviceIdContext`]. Once
/// that one is not IP-specific, this trait should be removed in favor of that
/// one.
// TODO(https://fxbug.dev/123704): Use IpDeviceIdContext instead of this once it
// has been generalized.
pub(crate) trait DeviceIdContext {
    type SocketDeviceId: Debug;
    type WeakSocketDeviceId: Debug;

    fn downgrade_device_id(&mut self, device_id: &Self::SocketDeviceId)
        -> Self::WeakSocketDeviceId;

    fn upgrade_weak_device_id(
        &mut self,
        weak_device_id: &Self::WeakSocketDeviceId,
    ) -> Option<Self::SocketDeviceId>;
}

pub(crate) trait SyncContext<C: NonSyncContext>: DeviceSocketAccessor {
    type DeviceSocketAccessor<'a>: DeviceSocketAccessor<
        SocketDeviceId = Self::SocketDeviceId,
        WeakSocketDeviceId = Self::WeakSocketDeviceId,
    >;

    /// Executes the provided callback with immutable access to socket state.
    fn with_sockets<
        F: FnOnce(&Sockets<Self::WeakSocketDeviceId>, &mut Self::DeviceSocketAccessor<'_>) -> R,
        R,
    >(
        &mut self,
        cb: F,
    ) -> R;

    /// Executes the provided callback with mutable access to socket state.
    fn with_sockets_mut<
        F: FnOnce(&mut Sockets<Self::WeakSocketDeviceId>, &mut Self::DeviceSocketAccessor<'_>) -> R,
        R,
    >(
        &mut self,
        cb: F,
    ) -> R;
}

pub(crate) trait DeviceSocketAccessor: DeviceIdContext {
    /// Executes the provided callback with immutable access to device-specific socket state.
    fn with_device_sockets<F: FnOnce(&DeviceSockets) -> R, R>(
        &mut self,
        device: &Self::SocketDeviceId,
        cb: F,
    ) -> R;

    // Executes the provided callback with mutable access to device-specific socket state.
    fn with_device_sockets_mut<F: FnOnce(&mut DeviceSockets) -> R, R>(
        &mut self,
        device: &Self::SocketDeviceId,
        cb: F,
    ) -> R;
}

/// Internal implementation trait that allows abstracting over device ID types.
trait SocketHandler<D, C: NonSyncContext> {
    /// Creates a new packet socket.
    fn create(&mut self) -> SocketId;

    /// Sets the device for a packet socket without affecting the protocol.
    fn set_device(&mut self, socket: &mut SocketId, device: DeviceSelector<&D>);

    /// Sets both the device and protocol for a packet socket.
    fn set_device_and_protocol(
        &mut self,
        id: &mut SocketId,
        device: DeviceSelector<&D>,
        protocol: Protocol,
    );

    /// Removes a packet socket.
    fn remove(&mut self, id: SocketId);
}

enum MaybeUpdate<T> {
    NoChange,
    NewValue(T),
}

fn update_device_and_protocol<SC: SyncContext<C>, C: NonSyncContext>(
    sync_ctx: &mut SC,
    socket: &SocketId,
    new_device: DeviceSelector<&SC::SocketDeviceId>,
    protocol_update: MaybeUpdate<Protocol>,
) {
    let SocketId(index) = socket;
    sync_ctx.with_sockets_mut(|Sockets(sockets), sync_ctx| {
        let SocketState { protocol, device } =
            sockets.get_mut(*index).unwrap_or_else(|| panic!("invalid socket ID {:?}", socket));

        match protocol_update {
            MaybeUpdate::NewValue(p) => *protocol = Some(p),
            MaybeUpdate::NoChange => (),
        };

        // Remove the reference to the socket from the old device if there is
        // one.
        match &device {
            Some(device) => {
                if let Some(device) = sync_ctx.upgrade_weak_device_id(device) {
                    let _index: usize = sync_ctx.with_device_sockets_mut(
                        &device,
                        |DeviceSockets(device_sockets)| {
                            device_sockets.swap_remove(
                                device_sockets
                                    .iter()
                                    .position(|i| i == index)
                                    .unwrap_or_else(|| panic!("invalid socket ID {:?}", socket)),
                            )
                        },
                    );
                }
            }
            None => (),
        };

        // Add the reference to the new device, if there is one.
        match &new_device {
            DeviceSelector::SpecificDevice(new_device) => sync_ctx
                .with_device_sockets_mut(new_device, |DeviceSockets(device_sockets)| {
                    device_sockets.push(*index)
                }),
            DeviceSelector::AnyDevice => (),
        };

        *device = match new_device {
            DeviceSelector::AnyDevice => None,
            DeviceSelector::SpecificDevice(d) => Some(sync_ctx.downgrade_device_id(d)),
        };
    });
}

impl<SC: SyncContext<C>, C: NonSyncContext> SocketHandler<SC::SocketDeviceId, C> for SC
where
    SC::SocketDeviceId: Clone + Debug + Eq + Hash,
{
    fn create(&mut self) -> SocketId {
        let index =
            self.with_sockets_mut(|Sockets(sockets), _: &mut SC::DeviceSocketAccessor<'_>| {
                sockets.push(SocketState::default())
            });
        SocketId(index)
    }

    fn set_device(&mut self, socket: &mut SocketId, device: DeviceSelector<&SC::SocketDeviceId>) {
        update_device_and_protocol(self, socket, device, MaybeUpdate::NoChange)
    }

    fn set_device_and_protocol(
        &mut self,
        socket: &mut SocketId,
        device: DeviceSelector<&SC::SocketDeviceId>,
        protocol: Protocol,
    ) {
        update_device_and_protocol(self, socket, device, MaybeUpdate::NewValue(protocol))
    }

    fn remove(&mut self, id: SocketId) {
        let SocketId(index) = id;
        self.with_sockets_mut(|Sockets(sockets), sync_ctx| {
            let SocketState { device, protocol: _ } =
                sockets.remove(index).unwrap_or_else(|| panic!("invalid socket ID {id:?}"));

            let device = match device {
                None => return,
                Some(device) => device,
            };

            match sync_ctx.upgrade_weak_device_id(&device) {
                None => {
                    // The device was removed earlier so there's no state that
                    // needs to be cleaned up.
                    return;
                }
                Some(strong_device) => {
                    sync_ctx.with_device_sockets_mut(&strong_device, |DeviceSockets(sockets)| {
                        let _: usize = sockets.swap_remove(
                            sockets
                                .iter()
                                .position(|p| *p == index)
                                .unwrap_or_else(|| panic!("invalid socket ID {id:?}")),
                        );
                    })
                }
            }
        })
    }
}

/// Creates an packet socket with no protocol set configured for all devices.
pub fn create<C: crate::NonSyncContext>(sync_ctx: &SyncCtx<C>) -> SocketId {
    let mut sync_ctx = Locked::new(sync_ctx);
    SocketHandler::create(&mut sync_ctx)
}

/// Sets the device for which a packet socket will receive packets.
pub fn set_device<C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    id: &mut SocketId,
    device: DeviceSelector<&DeviceId<C>>,
) {
    let mut sync_ctx = Locked::new(sync_ctx);
    SocketHandler::set_device(&mut sync_ctx, id, device)
}

/// Sets the device and protocol for which a socket will receive packets.
pub fn set_device_and_protocol<C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    id: &mut SocketId,
    device: DeviceSelector<&DeviceId<C>>,
    protocol: Protocol,
) {
    let mut sync_ctx = Locked::new(sync_ctx);
    SocketHandler::set_device_and_protocol(&mut sync_ctx, id, device, protocol)
}

/// Removes a bound socket.
pub fn remove<C: crate::NonSyncContext>(sync_ctx: &SyncCtx<C>, id: SocketId) {
    let mut sync_ctx = Locked::new(sync_ctx);
    SocketHandler::remove(&mut sync_ctx, id)
}

impl<C: crate::NonSyncContext> DeviceIdContext for &'_ SyncCtx<C> {
    type SocketDeviceId = DeviceId<C>;
    type WeakSocketDeviceId = WeakDeviceId<C>;

    fn downgrade_device_id(
        &mut self,
        device_id: &Self::SocketDeviceId,
    ) -> Self::WeakSocketDeviceId {
        device_id.downgrade()
    }

    fn upgrade_weak_device_id(
        &mut self,
        weak_device_id: &Self::WeakSocketDeviceId,
    ) -> Option<Self::SocketDeviceId> {
        weak_device_id.upgrade()
    }
}

// TODO(https://fxbug.dev/120973): Remove this impl.
impl<C: crate::NonSyncContext> DeviceSocketAccessor for &'_ SyncCtx<C> {
    fn with_device_sockets<F: FnOnce(&DeviceSockets) -> R, R>(
        &mut self,
        device: &Self::SocketDeviceId,
        cb: F,
    ) -> R {
        DeviceSocketAccessor::with_device_sockets(&mut Locked::new(*self), device, cb)
    }

    fn with_device_sockets_mut<F: FnOnce(&mut DeviceSockets) -> R, R>(
        &mut self,
        device: &Self::SocketDeviceId,
        cb: F,
    ) -> R {
        DeviceSocketAccessor::with_device_sockets_mut(&mut Locked::new(*self), device, cb)
    }
}

// TODO(https://fxbug.dev/120973): Remove this impl.
impl<C: crate::NonSyncContext> SyncContext<C> for &'_ SyncCtx<C> {
    type DeviceSocketAccessor<'a> =
        <Locked<'a, SyncCtx<C>, Unlocked> as SyncContext<C>>::DeviceSocketAccessor<'a>;

    fn with_sockets<
        F: FnOnce(&Sockets<WeakDeviceId<C>>, &mut Self::DeviceSocketAccessor<'_>) -> R,
        R,
    >(
        &mut self,
        cb: F,
    ) -> R {
        SyncContext::with_sockets(&mut Locked::new(*self), cb)
    }

    fn with_sockets_mut<
        F: FnOnce(&mut Sockets<WeakDeviceId<C>>, &mut Self::DeviceSocketAccessor<'_>) -> R,
        R,
    >(
        &mut self,
        cb: F,
    ) -> R {
        SyncContext::with_sockets_mut(&mut Locked::new(*self), cb)
    }
}

impl<C: crate::NonSyncContext, L: LockBefore<crate::lock_ordering::AnyDeviceSockets>> SyncContext<C>
    for Locked<'_, SyncCtx<C>, L>
{
    type DeviceSocketAccessor<'a> = Locked<'a, SyncCtx<C>, crate::lock_ordering::AnyDeviceSockets>;

    fn with_sockets<
        F: FnOnce(&Sockets<WeakDeviceId<C>>, &mut Self::DeviceSocketAccessor<'_>) -> R,
        R,
    >(
        &mut self,
        cb: F,
    ) -> R {
        let (sockets, mut locked) = self.read_lock_and::<crate::lock_ordering::AnyDeviceSockets>();
        cb(&*sockets, &mut locked)
    }

    fn with_sockets_mut<
        F: FnOnce(&mut Sockets<WeakDeviceId<C>>, &mut Self::DeviceSocketAccessor<'_>) -> R,
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

impl<C: crate::NonSyncContext, L: LockBefore<crate::lock_ordering::DeviceSockets>>
    DeviceSocketAccessor for Locked<'_, SyncCtx<C>, L>
{
    fn with_device_sockets<F: FnOnce(&DeviceSockets) -> R, R>(
        &mut self,
        device: &Self::SocketDeviceId,
        cb: F,
    ) -> R {
        match device {
            DeviceId::Ethernet(device) => with_ethernet_state(self, device, |mut locked| {
                let device_sockets = locked.read_lock::<crate::lock_ordering::DeviceSockets>();
                cb(&*device_sockets)
            }),
            DeviceId::Loopback(device) => with_loopback_state(self, device, |mut locked| {
                let device_sockets = locked.read_lock::<crate::lock_ordering::DeviceSockets>();
                cb(&*device_sockets)
            }),
        }
    }

    fn with_device_sockets_mut<F: FnOnce(&mut DeviceSockets) -> R, R>(
        &mut self,
        device: &Self::SocketDeviceId,
        cb: F,
    ) -> R {
        match device {
            DeviceId::Ethernet(device) => with_ethernet_state(self, device, |mut locked| {
                let mut device_sockets = locked.write_lock::<crate::lock_ordering::DeviceSockets>();
                cb(&mut *device_sockets)
            }),
            DeviceId::Loopback(device) => with_loopback_state(self, device, |mut locked| {
                let mut device_sockets = locked.write_lock::<crate::lock_ordering::DeviceSockets>();
                cb(&mut *device_sockets)
            }),
        }
    }
}

impl<C: crate::NonSyncContext, L: LockBefore<crate::lock_ordering::DeviceSockets>> DeviceIdContext
    for Locked<'_, SyncCtx<C>, L>
{
    type SocketDeviceId = DeviceId<C>;
    type WeakSocketDeviceId = WeakDeviceId<C>;

    fn downgrade_device_id(
        &mut self,
        device_id: &Self::SocketDeviceId,
    ) -> Self::WeakSocketDeviceId {
        device_id.downgrade()
    }

    fn upgrade_weak_device_id(
        &mut self,
        weak_device_id: &Self::WeakSocketDeviceId,
    ) -> Option<Self::SocketDeviceId> {
        weak_device_id.upgrade()
    }
}

impl<C: crate::NonSyncContext> RwLockFor<crate::lock_ordering::AnyDeviceSockets> for SyncCtx<C> {
    type ReadData<'l> = crate::sync::RwLockReadGuard<'l, Sockets<WeakDeviceId<C>>>
        where Self: 'l;
    type WriteData<'l> = crate::sync::RwLockWriteGuard<'l, Sockets<WeakDeviceId<C>>>
        where Self: 'l;

    fn read_lock(&self) -> Self::ReadData<'_> {
        self.state.device.shared_sockets.read()
    }
    fn write_lock(&self) -> Self::WriteData<'_> {
        self.state.device.shared_sockets.write()
    }
}

#[cfg(test)]
mod tests {
    use alloc::{collections::HashMap, vec};
    use derivative::Derivative;
    use nonzero_ext::nonzero;
    use test_case::test_case;

    use crate::ip::testutil::FakeWeakDeviceId;
    use crate::{context::testutil::FakeSyncCtx, ip::testutil::MultipleDevicesId};

    use super::*;

    #[derive(Debug, Default)]
    struct FakeNonSyncCtx;

    #[derive(Derivative)]
    #[derivative(Default(bound = ""))]
    struct FakeSockets<D> {
        shared_sockets: Sockets<FakeWeakDeviceId<D>>,
        device_sockets: HashMap<D, DeviceSockets>,
    }

    impl<D: Eq + Hash> FakeSockets<D> {
        fn new(devices: impl IntoIterator<Item = D>) -> Self {
            let device_sockets =
                devices.into_iter().map(|d| (d, DeviceSockets::default())).collect();
            Self { shared_sockets: Default::default(), device_sockets }
        }

        fn remove_device(&mut self, device: &D) -> DeviceSockets {
            let Self { shared_sockets: _, device_sockets } = self;
            device_sockets.remove(device).unwrap()
        }
    }

    impl<D: Clone + Debug + Eq + Hash + 'static> DeviceSocketAccessor for HashMap<D, DeviceSockets> {
        fn with_device_sockets<F: FnOnce(&DeviceSockets) -> R, R>(
            &mut self,
            device: &Self::SocketDeviceId,
            cb: F,
        ) -> R {
            cb(self.get_mut(device).unwrap())
        }
        fn with_device_sockets_mut<F: FnOnce(&mut DeviceSockets) -> R, R>(
            &mut self,
            device: &Self::SocketDeviceId,
            cb: F,
        ) -> R {
            cb(self.get_mut(device).unwrap())
        }
    }

    impl<D: Clone + Debug + Eq + Hash + 'static> DeviceIdContext for HashMap<D, DeviceSockets> {
        type SocketDeviceId = D;
        type WeakSocketDeviceId = FakeWeakDeviceId<D>;
        fn downgrade_device_id(
            &mut self,
            device_id: &Self::SocketDeviceId,
        ) -> Self::WeakSocketDeviceId {
            FakeWeakDeviceId(device_id.clone())
        }
        fn upgrade_weak_device_id(
            &mut self,
            FakeWeakDeviceId(id): &Self::WeakSocketDeviceId,
        ) -> Option<Self::SocketDeviceId> {
            self.contains_key(id).then_some(id.clone())
        }
    }

    impl<D: Clone + Debug + Eq + Hash + 'static> DeviceSocketAccessor
        for FakeSyncCtx<FakeSockets<D>, (), D>
    {
        fn with_device_sockets<F: FnOnce(&DeviceSockets) -> R, R>(
            &mut self,
            device: &Self::SocketDeviceId,
            cb: F,
        ) -> R {
            DeviceSocketAccessor::with_device_sockets(
                &mut self.get_mut().device_sockets,
                device,
                cb,
            )
        }
        fn with_device_sockets_mut<F: FnOnce(&mut DeviceSockets) -> R, R>(
            &mut self,
            device: &Self::SocketDeviceId,
            cb: F,
        ) -> R {
            DeviceSocketAccessor::with_device_sockets_mut(
                &mut self.get_mut().device_sockets,
                device,
                cb,
            )
        }
    }

    impl<D: Clone + Debug + Eq + Hash + 'static> DeviceIdContext
        for FakeSyncCtx<FakeSockets<D>, (), D>
    {
        type SocketDeviceId = D;
        type WeakSocketDeviceId = FakeWeakDeviceId<D>;
        fn downgrade_device_id(
            &mut self,
            device_id: &Self::SocketDeviceId,
        ) -> Self::WeakSocketDeviceId {
            DeviceIdContext::downgrade_device_id(&mut self.get_mut().device_sockets, device_id)
        }
        fn upgrade_weak_device_id(
            &mut self,
            device_id: &Self::WeakSocketDeviceId,
        ) -> Option<Self::SocketDeviceId> {
            DeviceIdContext::upgrade_weak_device_id(&mut self.get_mut().device_sockets, device_id)
        }
    }

    impl<D: Clone + Debug + Eq + Hash + 'static> SyncContext<FakeNonSyncCtx>
        for FakeSyncCtx<FakeSockets<D>, (), D>
    {
        type DeviceSocketAccessor<'a> = HashMap<D, DeviceSockets>;

        fn with_sockets<
            F: FnOnce(&Sockets<FakeWeakDeviceId<D>>, &mut Self::DeviceSocketAccessor<'_>) -> R,
            R,
        >(
            &mut self,
            cb: F,
        ) -> R {
            let FakeSockets { shared_sockets, device_sockets } = self.get_mut();
            cb(shared_sockets, device_sockets)
        }
        fn with_sockets_mut<
            F: FnOnce(&mut Sockets<FakeWeakDeviceId<D>>, &mut Self::DeviceSocketAccessor<'_>) -> R,
            R,
        >(
            &mut self,
            cb: F,
        ) -> R {
            let FakeSockets { shared_sockets, device_sockets } = self.get_mut();
            cb(shared_sockets, device_sockets)
        }
    }

    const SOME_PROTOCOL: NonZeroU16 = nonzero!(2000u16);

    #[test]
    fn create_remove() {
        let mut sync_ctx = FakeSyncCtx::with_state(FakeSockets::new(MultipleDevicesId::all()));

        let bound = SocketHandler::<MultipleDevicesId, _>::create(&mut sync_ctx);

        SocketHandler::remove(&mut sync_ctx, bound);
    }

    #[test_case(DeviceSelector::AnyDevice)]
    #[test_case(DeviceSelector::SpecificDevice(&MultipleDevicesId::A))]
    fn set_device(device: DeviceSelector<&MultipleDevicesId>) {
        let mut sync_ctx = FakeSyncCtx::with_state(FakeSockets::new(MultipleDevicesId::all()));

        let mut bound = SocketHandler::create(&mut sync_ctx);
        SocketHandler::set_device(&mut sync_ctx, &mut bound, device.clone());

        let FakeSockets { device_sockets, shared_sockets: _ } = sync_ctx.get_ref();
        if let DeviceSelector::SpecificDevice(d) = device {
            let DeviceSockets(indexes) = device_sockets.get(&d).expect("device state exists");
            let SocketId(index) = bound;
            assert_eq!(indexes, &[index]);
        }
    }

    #[test]
    fn update_device() {
        let mut sync_ctx = FakeSyncCtx::with_state(FakeSockets::new(MultipleDevicesId::all()));

        let mut bound = SocketHandler::create(&mut sync_ctx);

        SocketHandler::set_device(
            &mut sync_ctx,
            &mut bound,
            DeviceSelector::SpecificDevice(&MultipleDevicesId::A),
        );

        // Now update the device and make sure the socket only appears in the
        // one device's list.
        SocketHandler::set_device(
            &mut sync_ctx,
            &mut bound,
            DeviceSelector::SpecificDevice(&MultipleDevicesId::B),
        );

        let FakeSockets { device_sockets, shared_sockets: _ } = sync_ctx.get_ref();
        let device_socket_lists = device_sockets
            .iter()
            .map(|(d, DeviceSockets(indexes))| (d, indexes.as_slice()))
            .collect::<HashMap<_, _>>();

        let SocketId(index) = bound;
        assert_eq!(
            device_socket_lists,
            HashMap::from([
                (&MultipleDevicesId::A, [].as_slice()),
                (&MultipleDevicesId::B, &[index])
            ])
        );
    }

    #[test_case(Protocol::All, DeviceSelector::AnyDevice)]
    #[test_case(Protocol::Specific(SOME_PROTOCOL), DeviceSelector::AnyDevice)]
    #[test_case(Protocol::All, DeviceSelector::SpecificDevice(&MultipleDevicesId::A))]
    #[test_case(
        Protocol::Specific(SOME_PROTOCOL),
        DeviceSelector::SpecificDevice(&MultipleDevicesId::A)
    )]
    fn create_set_device_and_protocol_remove_multiple(
        protocol: Protocol,
        device: DeviceSelector<&MultipleDevicesId>,
    ) {
        let mut sync_ctx = FakeSyncCtx::with_state(FakeSockets::new(MultipleDevicesId::all()));

        let mut sockets = [(); 3].map(|()| SocketHandler::create(&mut sync_ctx));
        for socket in &mut sockets {
            SocketHandler::set_device_and_protocol(&mut sync_ctx, socket, device.clone(), protocol)
        }

        for socket in sockets {
            SocketHandler::remove(&mut sync_ctx, socket)
        }
    }

    #[test]
    fn change_device_after_removal() {
        let mut sync_ctx = FakeSyncCtx::with_state(FakeSockets::new(MultipleDevicesId::all()));

        let mut bound = SocketHandler::create(&mut sync_ctx);
        // Set the device for the socket before removing the device state
        // entirely.
        const DEVICE_TO_REMOVE: MultipleDevicesId = MultipleDevicesId::A;
        SocketHandler::set_device(
            &mut sync_ctx,
            &mut bound,
            DeviceSelector::SpecificDevice(&DEVICE_TO_REMOVE),
        );

        // Now remove the device; this should cause future attempts to upgrade
        // the device ID to fail.
        let removed = sync_ctx.get_mut().remove_device(&DEVICE_TO_REMOVE);
        assert_eq!(
            removed,
            DeviceSockets(vec![{
                let SocketId(index) = bound;
                index
            }])
        );

        // Changing the device should gracefully handle the fact that the
        // earlier-bound device is now gone.
        SocketHandler::set_device(
            &mut sync_ctx,
            &mut bound,
            DeviceSelector::SpecificDevice(&MultipleDevicesId::B),
        );

        let FakeSockets { device_sockets, shared_sockets: _ } = sync_ctx.get_ref();
        let DeviceSockets(indexes) =
            device_sockets.get(&MultipleDevicesId::B).expect("device state exists");
        let SocketId(index) = bound;
        assert_eq!(indexes, &[index]);
    }
}
