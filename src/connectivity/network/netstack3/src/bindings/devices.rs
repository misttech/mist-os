// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{
    collections::hash_map::{self, HashMap},
    num::NonZeroU64,
    ops::{Deref as _, DerefMut as _},
};

use assert_matches::assert_matches;
use derivative::Derivative;
use fidl_fuchsia_net_interfaces as fnet_interfaces;
use fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin;
use futures::stream::StreamExt as _;
use net_types::{
    ethernet::Mac,
    ip::{IpAddr, Mtu},
    SpecifiedAddr, UnicastAddr,
};
use netstack3_core::{
    device::{handle_queued_rx_packets, loopback::LoopbackDeviceId, DeviceId},
    sync::{Mutex as CoreMutex, RwLock as CoreRwLock},
};

use crate::bindings::{
    interfaces_admin, util::NeedsDataNotifier, BindingsNonSyncCtxImpl, Ctx, DeviceIdExt as _,
    Netstack,
};

pub const LOOPBACK_MAC: Mac = Mac::new([0, 0, 0, 0, 0, 0]);

pub type BindingId = NonZeroU64;

/// Keeps tabs on devices.
///
/// `Devices` keeps a list of devices that are installed in the netstack with
/// an associated netstack core ID `C` used to reference the device.
///
/// The type parameter `C` is for the extra information associated with the
/// device. The type parameters are there to allow testing without dependencies
/// on `core`.
pub struct Devices<C> {
    id_map: CoreRwLock<HashMap<BindingId, C>>,
    last_id: CoreMutex<BindingId>,
}

impl<C> Default for Devices<C> {
    fn default() -> Self {
        Self { id_map: Default::default(), last_id: CoreMutex::new(BindingId::MIN) }
    }
}

impl<C> Devices<C>
where
    C: Clone + std::fmt::Debug + PartialEq,
{
    /// Allocates a new [`BindingId`].
    #[must_use]
    pub fn alloc_new_id(&self) -> BindingId {
        let Self { id_map: _, last_id } = self;
        let mut last_id = last_id.lock();
        let id = *last_id;
        *last_id = last_id.checked_add(1).expect("exhausted binding device IDs");
        id
    }

    /// Adds a new device.
    ///
    /// Adds a new device if the informed `core_id` is valid (i.e., not
    /// currently tracked by [`Devices`]). A new [`BindingId`] will be allocated
    /// and a [`DeviceInfo`] struct will be created with the provided `info` and
    /// IDs.
    pub fn add_device(&self, id: BindingId, core_id: C) {
        let Self { id_map, last_id: _ } = self;
        assert_matches!(id_map.write().insert(id, core_id.clone()), None);
    }

    /// Removes a device from the internal list.
    ///
    /// Removes a device from the internal [`Devices`] list and returns the
    /// associated [`DeviceInfo`] if `id` is found or `None` otherwise.
    pub fn remove_device(&self, id: BindingId) -> Option<C> {
        let Self { id_map, last_id: _ } = self;
        id_map.write().remove(&id)
    }

    /// Retrieve associated `core_id` for [`BindingId`].
    pub fn get_core_id(&self, id: BindingId) -> Option<C> {
        self.id_map.read().get(&id).cloned()
    }

    /// Call the provided callback with an iterator over the devices.
    pub fn with_devices<R>(&self, f: impl FnOnce(hash_map::Values<'_, BindingId, C>) -> R) -> R {
        let Self { id_map, last_id: _ } = self;
        f(id_map.read().values())
    }
}

impl Devices<DeviceId<BindingsNonSyncCtxImpl>> {
    /// Retrieves the device with the given name.
    pub fn get_device_by_name(&self, name: &str) -> Option<DeviceId<BindingsNonSyncCtxImpl>> {
        self.id_map
            .read()
            .iter()
            .find_map(|(_binding_id, c)| {
                (c.external_state().static_common_info().name == name).then_some(c)
            })
            .cloned()
    }
}

/// Device specific iformation.
#[derive(Debug)]
pub enum DeviceSpecificInfo<'a> {
    Netdevice(&'a NetdeviceInfo),
    Loopback(&'a LoopbackInfo),
}

impl DeviceSpecificInfo<'_> {
    pub fn static_common_info(&self) -> &StaticCommonInfo {
        match self {
            Self::Netdevice(i) => &i.static_common_info,
            Self::Loopback(i) => &i.static_common_info,
        }
    }

    pub fn with_common_info<O, F: FnOnce(&DynamicCommonInfo) -> O>(&self, cb: F) -> O {
        match self {
            Self::Netdevice(i) => {
                i.with_dynamic_info(|DynamicNetdeviceInfo { phy_up: _, common_info }| {
                    cb(common_info)
                })
            }
            Self::Loopback(i) => i.with_dynamic_info(cb),
        }
    }

    pub fn with_common_info_mut<O, F: FnOnce(&mut DynamicCommonInfo) -> O>(&self, cb: F) -> O {
        match self {
            Self::Netdevice(i) => {
                i.with_dynamic_info_mut(|DynamicNetdeviceInfo { phy_up: _, common_info }| {
                    cb(common_info)
                })
            }
            Self::Loopback(i) => i.with_dynamic_info_mut(cb),
        }
    }
}

pub(crate) fn spawn_rx_task(
    notifier: &NeedsDataNotifier,
    ns: &Netstack,
    device_id: &LoopbackDeviceId<BindingsNonSyncCtxImpl>,
) {
    let mut watcher = notifier.watcher();
    let device_id = device_id.downgrade();

    let ns = ns.clone();
    fuchsia_async::Task::spawn(async move {
        // Loop while we are woken up to handle enqueued RX packets.
        while let Some(device_id) = watcher.next().await.and_then(|()| device_id.upgrade()) {
            let mut ctx = ns.ctx.clone();
            let Ctx { sync_ctx, non_sync_ctx } = &mut ctx;
            handle_queued_rx_packets(sync_ctx, non_sync_ctx, &device_id)
        }
    })
    .detach()
}

/// Static information common to all devices.
#[derive(Debug)]
pub struct StaticCommonInfo {
    pub binding_id: BindingId,
    pub name: String,
}

/// Information common to all devices.
#[derive(Derivative)]
#[derivative(Debug)]
pub struct DynamicCommonInfo {
    pub mtu: Mtu,
    pub admin_enabled: bool,
    pub events: super::InterfaceEventProducer,
    // An attach point to send `fuchsia.net.interfaces.admin/Control` handles to the Interfaces
    // Admin worker.
    #[derivative(Debug = "ignore")]
    pub control_hook: futures::channel::mpsc::Sender<interfaces_admin::OwnedControlHandle>,
    pub(crate) addresses: HashMap<SpecifiedAddr<IpAddr>, AddressInfo>,
}

#[derive(Debug)]
pub(crate) struct AddressInfo {
    // The `AddressStateProvider` FIDL protocol worker.
    pub(crate) address_state_provider: FidlWorkerInfo<fnet_interfaces_admin::AddressRemovalReason>,
    // Sender for [`AddressAssignmentState`] change events published by Core;
    // the receiver is held by the `AddressStateProvider` worker. Note that an
    // [`UnboundedSender`] is used because it exposes a synchronous send API
    // which is required since Core is no-async.
    pub(crate) assignment_state_sender:
        futures::channel::mpsc::UnboundedSender<fnet_interfaces::AddressAssignmentState>,
}

/// Loopback device information.
#[derive(Derivative)]
#[derivative(Debug)]
pub struct LoopbackInfo {
    pub static_common_info: StaticCommonInfo,
    pub dynamic_common_info: std::sync::RwLock<DynamicCommonInfo>,
    #[derivative(Debug = "ignore")]
    pub rx_notifier: NeedsDataNotifier,
}

impl LoopbackInfo {
    pub fn with_dynamic_info<O, F: FnOnce(&DynamicCommonInfo) -> O>(&self, cb: F) -> O {
        cb(self.dynamic_common_info.read().unwrap().deref())
    }

    pub fn with_dynamic_info_mut<O, F: FnOnce(&mut DynamicCommonInfo) -> O>(&self, cb: F) -> O {
        cb(self.dynamic_common_info.write().unwrap().deref_mut())
    }
}

/// Information associated with FIDL Protocol workers.
#[derive(Debug)]
pub(crate) struct FidlWorkerInfo<R> {
    // The worker `Task`, wrapped in a `Shared` future so that it can be awaited
    // multiple times.
    pub worker: futures::future::Shared<fuchsia_async::Task<()>>,
    // Mechanism to cancel the worker with reason `R`. If `Some`, the worker is
    // active (and holds the `Receiver`). Otherwise, the worker has been
    // canceled.
    pub cancelation_sender: Option<futures::channel::oneshot::Sender<R>>,
}

#[derive(Debug)]
pub struct DynamicNetdeviceInfo {
    pub phy_up: bool,
    pub common_info: DynamicCommonInfo,
}

/// Network device information.
#[derive(Debug)]
pub struct NetdeviceInfo {
    pub handler: super::netdevice_worker::PortHandler,
    pub mac: UnicastAddr<Mac>,
    pub static_common_info: StaticCommonInfo,
    pub dynamic: std::sync::RwLock<DynamicNetdeviceInfo>,
}

impl NetdeviceInfo {
    pub fn with_dynamic_info<O, F: FnOnce(&DynamicNetdeviceInfo) -> O>(&self, cb: F) -> O {
        let dynamic = self.dynamic.read().unwrap();
        cb(dynamic.deref())
    }

    pub fn with_dynamic_info_mut<O, F: FnOnce(&mut DynamicNetdeviceInfo) -> O>(&self, cb: F) -> O {
        let mut dynamic = self.dynamic.write().unwrap();
        cb(dynamic.deref_mut())
    }
}
