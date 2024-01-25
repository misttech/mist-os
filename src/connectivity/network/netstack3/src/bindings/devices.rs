// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{
    collections::hash_map::{self, HashMap},
    fmt::{self, Debug, Display},
    num::NonZeroU64,
    ops::{Deref as _, DerefMut as _},
};

use assert_matches::assert_matches;
use derivative::Derivative;
use fidl_fuchsia_net_interfaces as fnet_interfaces;
use fuchsia_zircon as zx;
use net_types::{
    ethernet::Mac,
    ip::{IpAddr, Mtu},
    SpecifiedAddr, UnicastAddr,
};
use netstack3_core::{
    device::{
        DeviceId, DeviceSendFrameError, EthernetLinkDevice, LoopbackDevice, LoopbackDeviceId,
        PureIpDevice,
    },
    sync::{Mutex as CoreMutex, RwLock as CoreRwLock},
    types::WorkQueueReport,
};
use tracing::warn;

use crate::bindings::{
    interfaces_admin, neighbor_worker, util::NeedsDataNotifier, BindingsCtx, Ctx,
};

pub(crate) const LOOPBACK_MAC: Mac = Mac::new([0, 0, 0, 0, 0, 0]);

pub(crate) type BindingId = NonZeroU64;

/// Keeps tabs on devices.
///
/// `Devices` keeps a list of devices that are installed in the netstack with
/// an associated netstack core ID `C` used to reference the device.
///
/// The type parameter `C` is for the extra information associated with the
/// device. The type parameters are there to allow testing without dependencies
/// on `core`.
pub(crate) struct Devices<C> {
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
    pub(crate) fn alloc_new_id(&self) -> BindingId {
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
    pub(crate) fn add_device(&self, id: BindingId, core_id: C) {
        let Self { id_map, last_id: _ } = self;
        assert_matches!(id_map.write().insert(id, core_id), None);
    }

    /// Removes a device from the internal list.
    ///
    /// Removes a device from the internal [`Devices`] list and returns the
    /// associated [`DeviceInfo`] if `id` is found or `None` otherwise.
    pub(crate) fn remove_device(&self, id: BindingId) -> Option<C> {
        let Self { id_map, last_id: _ } = self;
        id_map.write().remove(&id)
    }

    /// Retrieve associated `core_id` for [`BindingId`].
    pub(crate) fn get_core_id(&self, id: BindingId) -> Option<C> {
        self.id_map.read().get(&id).cloned()
    }

    /// Call the provided callback with an iterator over the devices.
    pub(crate) fn with_devices<R>(
        &self,
        f: impl FnOnce(hash_map::Values<'_, BindingId, C>) -> R,
    ) -> R {
        let Self { id_map, last_id: _ } = self;
        f(id_map.read().values())
    }
}

impl Devices<DeviceId<BindingsCtx>> {
    /// Retrieves the device with the given name.
    pub(crate) fn get_device_by_name(&self, name: &str) -> Option<DeviceId<BindingsCtx>> {
        self.id_map
            .read()
            .iter()
            .find_map(|(_binding_id, c)| (c.bindings_id().name == name).then_some(c))
            .cloned()
    }
}

/// Device specific iformation.
#[derive(Debug)]
pub(crate) enum DeviceSpecificInfo<'a> {
    Netdevice(&'a NetdeviceInfo),
    Loopback(&'a LoopbackInfo),
    PureIp(&'a PureIpDeviceInfo),
}

impl DeviceSpecificInfo<'_> {
    pub(crate) fn static_common_info(&self) -> &StaticCommonInfo {
        match self {
            Self::Netdevice(i) => &i.static_common_info,
            Self::Loopback(i) => &i.static_common_info,
            Self::PureIp(i) => &i.static_common_info,
        }
    }

    pub(crate) fn with_common_info<O, F: FnOnce(&DynamicCommonInfo) -> O>(&self, cb: F) -> O {
        match self {
            Self::Netdevice(i) => i.with_dynamic_info(
                |DynamicNetdeviceInfo { phy_up: _, common_info, neighbor_event_sink: _ }| {
                    cb(common_info)
                },
            ),
            Self::Loopback(i) => i.with_dynamic_info(cb),
            Self::PureIp(i) => i.with_dynamic_info(cb),
        }
    }

    pub(crate) fn with_common_info_mut<O, F: FnOnce(&mut DynamicCommonInfo) -> O>(
        &self,
        cb: F,
    ) -> O {
        match self {
            Self::Netdevice(i) => i.with_dynamic_info_mut(
                |DynamicNetdeviceInfo { phy_up: _, common_info, neighbor_event_sink: _ }| {
                    cb(common_info)
                },
            ),
            Self::Loopback(i) => i.with_dynamic_info_mut(cb),
            Self::PureIp(i) => i.with_dynamic_info_mut(cb),
        }
    }
}

pub(crate) fn spawn_rx_task(
    notifier: &NeedsDataNotifier,
    mut ctx: Ctx,
    device_id: &LoopbackDeviceId<BindingsCtx>,
) -> fuchsia_async::Task<()> {
    let watcher = notifier.watcher();
    let device_id = device_id.downgrade();

    fuchsia_async::Task::spawn(crate::bindings::util::yielding_data_notifier_loop(
        watcher,
        move || {
            device_id
                .upgrade()
                .map(|device_id| ctx.api().receive_queue().handle_queued_frames(&device_id))
        },
    ))
}

pub(crate) fn spawn_tx_task(
    notifier: &NeedsDataNotifier,
    mut ctx: Ctx,
    device_id: DeviceId<BindingsCtx>,
) -> fuchsia_async::Task<()> {
    let watcher = notifier.watcher();
    let device_id = device_id.downgrade();

    fuchsia_async::Task::spawn(crate::bindings::util::yielding_data_notifier_loop(
        watcher,
        move || {
            // NB: We could write this function generically in terms of `D:
            // Device`, which is the type parameter given to instantiate the
            // transmit queue API. To do that, core would need to expose a
            // marker for CoreContext that is generic on `D`. That is doable but
            // not worth the extra code at this moment since bindings doesn't
            // have meaningful amounts of code that is generic over the device
            // type.
            device_id.upgrade().map(|device_id| {
                match &device_id {
                    DeviceId::Ethernet(device_id) => ctx
                        .api()
                        .transmit_queue::<EthernetLinkDevice>()
                        .transmit_queued_frames(device_id),
                    DeviceId::Loopback(device_id) => ctx
                        .api()
                        .transmit_queue::<LoopbackDevice>()
                        .transmit_queued_frames(device_id),
                    DeviceId::PureIp(device_id) => {
                        ctx.api().transmit_queue::<PureIpDevice>().transmit_queued_frames(device_id)
                    }
                }
                .unwrap_or_else(|DeviceSendFrameError::DeviceNotReady(())| {
                    warn!(
                        "TODO(https://fxbug.dev/42057204): Support waiting for TX buffers to be \
                            available, dropping packet for now on device={device_id:?}",
                    );
                    WorkQueueReport::AllDone
                })
            })
        },
    ))
}

/// Static information common to all devices.
#[derive(Derivative, Debug)]
pub(crate) struct StaticCommonInfo {
    #[derivative(Debug = "ignore")]
    pub(crate) tx_notifier: NeedsDataNotifier,
    pub(crate) authorization_token: zx::Event,
}

/// Information common to all devices.
#[derive(Derivative)]
#[derivative(Debug)]
pub(crate) struct DynamicCommonInfo {
    pub(crate) mtu: Mtu,
    pub(crate) admin_enabled: bool,
    pub(crate) events: super::InterfaceEventProducer,
    // An attach point to send `fuchsia.net.interfaces.admin/Control` handles to the Interfaces
    // Admin worker.
    #[derivative(Debug = "ignore")]
    pub(crate) control_hook: futures::channel::mpsc::Sender<interfaces_admin::OwnedControlHandle>,
    pub(crate) addresses: HashMap<SpecifiedAddr<IpAddr>, AddressInfo>,
}

#[derive(Debug)]
pub(crate) struct AddressInfo {
    // The `AddressStateProvider` FIDL protocol worker.
    pub(crate) address_state_provider:
        FidlWorkerInfo<interfaces_admin::AddressStateProviderCancellationReason>,
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
pub(crate) struct LoopbackInfo {
    pub(crate) static_common_info: StaticCommonInfo,
    pub(crate) dynamic_common_info: std::sync::RwLock<DynamicCommonInfo>,
    #[derivative(Debug = "ignore")]
    pub(crate) rx_notifier: NeedsDataNotifier,
}

impl LoopbackInfo {
    pub(crate) fn with_dynamic_info<O, F: FnOnce(&DynamicCommonInfo) -> O>(&self, cb: F) -> O {
        cb(self.dynamic_common_info.read().unwrap().deref())
    }

    pub(crate) fn with_dynamic_info_mut<O, F: FnOnce(&mut DynamicCommonInfo) -> O>(
        &self,
        cb: F,
    ) -> O {
        cb(self.dynamic_common_info.write().unwrap().deref_mut())
    }
}

/// Information associated with FIDL Protocol workers.
#[derive(Debug)]
pub(crate) struct FidlWorkerInfo<R> {
    // The worker `Task`, wrapped in a `Shared` future so that it can be awaited
    // multiple times.
    pub(crate) worker: futures::future::Shared<fuchsia_async::Task<()>>,
    // Mechanism to cancel the worker with reason `R`. If `Some`, the worker is
    // active (and holds the `Receiver`). Otherwise, the worker has been
    // canceled.
    pub(crate) cancelation_sender: Option<futures::channel::oneshot::Sender<R>>,
}

#[derive(Derivative)]
#[derivative(Debug)]
pub(crate) struct DynamicNetdeviceInfo {
    pub(crate) phy_up: bool,
    pub(crate) common_info: DynamicCommonInfo,
    #[derivative(Debug = "ignore")]
    pub(crate) neighbor_event_sink: futures::channel::mpsc::UnboundedSender<neighbor_worker::Event>,
}

/// Network device information.
#[derive(Debug)]
pub(crate) struct NetdeviceInfo {
    pub(crate) handler: super::netdevice_worker::PortHandler,
    pub(crate) mac: UnicastAddr<Mac>,
    pub(crate) static_common_info: StaticCommonInfo,
    pub(crate) dynamic: std::sync::RwLock<DynamicNetdeviceInfo>,
}

impl NetdeviceInfo {
    pub(crate) fn with_dynamic_info<O, F: FnOnce(&DynamicNetdeviceInfo) -> O>(&self, cb: F) -> O {
        let dynamic = self.dynamic.read().unwrap();
        cb(dynamic.deref())
    }

    pub(crate) fn with_dynamic_info_mut<O, F: FnOnce(&mut DynamicNetdeviceInfo) -> O>(
        &self,
        cb: F,
    ) -> O {
        let mut dynamic = self.dynamic.write().unwrap();
        cb(dynamic.deref_mut())
    }
}

#[derive(Debug)]
pub(crate) struct PureIpDeviceInfo {
    pub(crate) static_common_info: StaticCommonInfo,
    pub(crate) dynamic_common_info: std::sync::RwLock<DynamicCommonInfo>,
}

impl PureIpDeviceInfo {
    pub(crate) fn with_dynamic_info<O, F: FnOnce(&DynamicCommonInfo) -> O>(&self, cb: F) -> O {
        cb(self.dynamic_common_info.read().unwrap().deref())
    }

    pub(crate) fn with_dynamic_info_mut<O, F: FnOnce(&mut DynamicCommonInfo) -> O>(
        &self,
        cb: F,
    ) -> O {
        cb(self.dynamic_common_info.write().unwrap().deref_mut())
    }
}

pub(crate) struct DeviceIdAndName {
    pub(crate) id: BindingId,
    pub(crate) name: String,
}

impl Debug for DeviceIdAndName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { id, name } = self;
        write!(f, "{id}=>{name}")
    }
}

impl Display for DeviceIdAndName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Debug::fmt(self, f)
    }
}
