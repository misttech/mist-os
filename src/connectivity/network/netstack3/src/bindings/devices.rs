// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::hash_map::{self, HashMap};
use std::fmt::{self, Debug, Display};
use std::num::NonZeroU64;
use std::ops::{Deref as _, DerefMut as _};

use super::util::NeedsDataWatcher;
use super::DeviceIdExt;
use assert_matches::assert_matches;
use derivative::Derivative;
use futures::future::FusedFuture;
use futures::{FutureExt as _, StreamExt as _, TryFutureExt as _};
use net_types::ethernet::Mac;
use net_types::ip::{IpAddr, Mtu};
use net_types::{SpecifiedAddr, UnicastAddr};
use netstack3_core::device::{
    BatchSize, DeviceClassMatcher, DeviceId, DeviceIdAndNameMatcher, DeviceProvider,
    DeviceSendFrameError, EthernetLinkDevice, LoopbackDeviceId, PureIpDevice, WeakDeviceId,
};
use netstack3_core::sync::{Mutex as CoreMutex, RwLock as CoreRwLock};
use netstack3_core::types::WorkQueueReport;

use {
    fidl_fuchsia_hardware_network as fhardware_network,
    fidl_fuchsia_net_interfaces as fnet_interfaces,
};

use crate::bindings::power::TransmitSuspensionHandler;
use crate::bindings::util::NeedsDataNotifier;
use crate::bindings::{interfaces_admin, neighbor_worker, netdevice_worker, BindingsCtx, Ctx};

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

/// Owned device specific information
pub(crate) enum OwnedDeviceSpecificInfo {
    Loopback(LoopbackInfo),
    Ethernet(EthernetInfo),
    PureIp(PureIpDeviceInfo),
}

impl From<LoopbackInfo> for OwnedDeviceSpecificInfo {
    fn from(info: LoopbackInfo) -> Self {
        Self::Loopback(info)
    }
}

impl From<EthernetInfo> for OwnedDeviceSpecificInfo {
    fn from(info: EthernetInfo) -> Self {
        Self::Ethernet(info)
    }
}

impl From<PureIpDeviceInfo> for OwnedDeviceSpecificInfo {
    fn from(info: PureIpDeviceInfo) -> Self {
        Self::PureIp(info)
    }
}

/// Borrowed device specific information.
#[derive(Debug)]
pub(crate) enum DeviceSpecificInfo<'a> {
    Loopback(&'a LoopbackInfo),
    Ethernet(&'a EthernetInfo),
    PureIp(&'a PureIpDeviceInfo),
}

impl DeviceSpecificInfo<'_> {
    pub(crate) fn static_common_info(&self) -> &StaticCommonInfo {
        match self {
            Self::Loopback(i) => &i.static_common_info,
            Self::Ethernet(i) => &i.common_info,
            Self::PureIp(i) => &i.common_info,
        }
    }

    pub(crate) fn with_common_info<O, F: FnOnce(&DynamicCommonInfo) -> O>(&self, cb: F) -> O {
        match self {
            Self::Loopback(i) => i.with_dynamic_info(cb),
            Self::Ethernet(i) => i.with_dynamic_info(|dynamic| cb(&dynamic.netdevice.common_info)),
            Self::PureIp(i) => i.with_dynamic_info(|dynamic| cb(&dynamic.common_info)),
        }
    }

    pub(crate) fn with_common_info_mut<O, F: FnOnce(&mut DynamicCommonInfo) -> O>(
        &self,
        cb: F,
    ) -> O {
        match self {
            Self::Loopback(i) => i.with_dynamic_info_mut(cb),
            Self::Ethernet(i) => {
                i.with_dynamic_info_mut(|dynamic| cb(&mut dynamic.netdevice.common_info))
            }
            Self::PureIp(i) => i.with_dynamic_info_mut(|dynamic| cb(&mut dynamic.common_info)),
        }
    }
}

pub(crate) fn spawn_rx_task(
    notifier: &NeedsDataNotifier,
    mut ctx: Ctx,
    device_id: &LoopbackDeviceId<BindingsCtx>,
) -> fuchsia_async::Task<()> {
    let mut watcher = notifier.watcher();
    let device_id = device_id.downgrade();

    fuchsia_async::Task::spawn(async move {
        let mut yield_fut = futures::future::OptionFuture::default();
        loop {
            // Loop while we are woken up to handle enqueued RX packets.
            let r = futures::select! {
                w = watcher.next().fuse() => w,
                y = yield_fut => Some(y.expect("OptionFuture is only selected when non-empty")),
            };

            let r = r.and_then(|()| {
                device_id
                    .upgrade()
                    .map(|device_id| ctx.api().receive_queue().handle_queued_frames(&device_id))
            });

            match r {
                Some(WorkQueueReport::AllDone) => (),
                Some(WorkQueueReport::Pending) => {
                    // Yield the task to the executor once.
                    yield_fut = Some(async_utils::futures::YieldToExecutorOnce::new()).into();
                }
                None => break,
            }
        }
    })
}

#[derive(thiserror::Error, Debug)]
pub(crate) enum TxTaskError {
    #[error("netdevice error: {0}")]
    Netdevice(#[from] netdevice_client::Error),
    #[error("aborted")]
    Aborted,
}

pub(crate) struct TxTask {
    task: fuchsia_async::Task<Result<(), TxTaskError>>,
    cancel: futures::future::AbortHandle,
}

impl TxTask {
    pub(crate) fn new(
        ctx: Ctx,
        device_id: WeakDeviceId<BindingsCtx>,
        watcher: NeedsDataWatcher,
    ) -> Self {
        let (fut, cancel) = futures::future::abortable(tx_task(ctx, device_id, watcher));
        let fut = fut.map(|r| match r {
            Ok(o) => o,
            Err(futures::future::Aborted) => Err(TxTaskError::Aborted),
        });
        let task = fuchsia_async::Task::spawn(fut);
        Self { task, cancel }
    }

    pub(crate) fn into_future_and_cancellation(
        self,
    ) -> (impl FusedFuture<Output = Result<(), TxTaskError>>, futures::future::AbortHandle) {
        let Self { task, cancel } = self;
        (task.fuse(), cancel)
    }
}

#[derive(Default)]
pub(crate) struct TxTaskState {
    pub(crate) tx_buffers: Vec<netdevice_client::TxBuffer>,
}

impl TxTaskState {
    /// Collects up to `count` tx buffers from `port_handler` into the task
    /// state.
    ///
    /// Note: `count` is `BatchSize` since the buffers are expected to be fed
    /// into the transmit queue API which has a limited batch size, so we avoid
    /// over allocating.
    async fn collect_buffers(
        &mut self,
        port_handler: &netdevice_worker::PortHandler,
        count: BatchSize,
    ) -> Result<usize, TxTaskError> {
        let Self { tx_buffers, .. } = self;
        let mut iter = port_handler.alloc_tx_buffers().await?.take(count.into());
        iter.try_fold((0, tx_buffers), |(count, b), item| {
            b.push(item?);
            Ok((count + 1, b))
        })
        .map(|(count, _)| count)
    }
}

// NB: We could write this function generically in terms of `D: Device`, which
// is the type parameter given to instantiate the transmit queue API. To do
// that, core would need to expose a marker for CoreContext that is generic on
// `D`. That is doable but not worth the extra code at this moment since
// bindings doesn't have meaningful amounts of code that is generic over the
// device type.
pub(crate) async fn tx_task(
    mut ctx: Ctx,
    device_id: WeakDeviceId<BindingsCtx>,
    mut watcher: NeedsDataWatcher,
) -> Result<(), TxTaskError> {
    let mut yield_fut = futures::future::OptionFuture::default();
    let mut task_state = TxTaskState::default();
    let mut suspension_handler = TransmitSuspensionHandler::new(&ctx, device_id.clone()).await;

    // This control loop selects an action which may be:
    //   - suspension:
    //       * enter suspension handling, which allows the system to suspend and
    //         only finishes when the system resumes
    //   - send packets
    //       * determine packet batch size
    //       * enqueue packets and send them, waiting for the send result
    //       * if the send result is an error, yield and continue sending
    //         on the next iteration
    //   - continue sending packets after yielding in a previous batch.
    // The loop continues until `watcher` closes or the `device_id` is invalid.
    loop {
        enum Action<S> {
            TxAvailable,
            Suspension(S),
        }
        // Loop while we are woken up to handle enqueued TX packets.
        let action = futures::select_biased! {
            s = suspension_handler.wait().fuse() => Action::Suspension(s),
            w = watcher.next().fuse() => match w {
                Some(()) => Action::TxAvailable,
                // Notifying watcher is closed, ok to break.
                None => break Ok(()),
            },
            y = yield_fut => {
                let () = y.expect("OptionFuture is only selected when non-empty");
                Action::TxAvailable
            }
        };

        let device_id = match action {
            Action::TxAvailable => match device_id.upgrade() {
                Some(d) => d,
                // Device was removed from the stack, ok to break.
                None => break Ok(()),
            },
            Action::Suspension(s) => {
                // Suspension requested, finish up in-progress work and block
                // until we are allowed to resume.
                match futures::future::ready(s).and_then(|s| s.handle_suspension()).await {
                    Ok(()) => {}
                    Err(e) => {
                        suspension_handler.disable(e);
                    }
                }
                continue;
            }
        };

        let batch_size = match device_id.external_state() {
            DeviceSpecificInfo::Loopback(_) => {
                unimplemented!("tx task is not supported for loopback devices")
            }
            DeviceSpecificInfo::Ethernet(EthernetInfo { netdevice, .. })
            | DeviceSpecificInfo::PureIp(PureIpDeviceInfo { netdevice, .. }) => {
                // Attempt to preallocate buffers to handle the queue.
                let queue_len = netstack3_core::for_any_device_id!(
                    DeviceId,
                    DeviceProvider,
                    D,
                    &device_id,
                    id => ctx.api().transmit_queue::<D>().count(id)
                );

                match queue_len {
                    Some(queue_len) => {
                        if queue_len != 0 {
                            let collected = task_state
                                .collect_buffers(
                                    &netdevice.handler,
                                    BatchSize::new_saturating(queue_len),
                                )
                                .await?;
                            BatchSize::new_saturating(collected)
                        } else {
                            // We got woken up to do tx work but core says we
                            // have zero buffers in the queue, go back to
                            // waiting.
                            continue;
                        }
                    }
                    None => {
                        // Queueing is not configured, go back to waiting.
                        continue;
                    }
                }
            }
        };

        let r = match &device_id {
            DeviceId::Loopback(_) => {
                unimplemented!("tx task is not supported for loopback devices")
            }
            DeviceId::Ethernet(id) => ctx
                .api()
                .transmit_queue::<EthernetLinkDevice>()
                .transmit_queued_frames(id, batch_size, &mut task_state),
            DeviceId::PureIp(id) => ctx
                .api()
                .transmit_queue::<PureIpDevice>()
                .transmit_queued_frames(id, batch_size, &mut task_state),
        }
        .unwrap_or_else(|err| {
            match err {
                // Core is already keeping track of counters for this, nothing
                // to be done in bindings.
                DeviceSendFrameError::NoBuffers => (),
            }
            // If we observe an error when sending, we're not sure if the queue
            // was drained or not, so assume we need to look at it again.
            WorkQueueReport::Pending
        });

        let TxTaskState { tx_buffers } = &mut task_state;
        // If for some reason we have buffers left here, return them so other
        // interfaces on the same device can use the buffers.
        tx_buffers.clear();

        match r {
            WorkQueueReport::AllDone => (),
            WorkQueueReport::Pending => {
                // Yield the task to the executor once.
                yield_fut = Some(async_utils::futures::YieldToExecutorOnce::new()).into();
            }
        }
    }
}

/// Static information common to all devices.
#[derive(Derivative, Debug)]
pub(crate) struct StaticCommonInfo {
    #[derivative(Debug = "ignore")]
    pub(crate) authorization_token: zx::Event,
}

impl Default for StaticCommonInfo {
    fn default() -> StaticCommonInfo {
        StaticCommonInfo { authorization_token: zx::Event::create() }
    }
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

/// Loopback device information.
#[derive(Derivative)]
#[derivative(Debug)]
pub(crate) struct LoopbackInfo {
    pub(crate) static_common_info: StaticCommonInfo,
    pub(crate) dynamic_common_info: CoreRwLock<DynamicCommonInfo>,
    #[derivative(Debug = "ignore")]
    pub(crate) rx_notifier: NeedsDataNotifier,
}

impl LoopbackInfo {
    pub(crate) fn with_dynamic_info<O, F: FnOnce(&DynamicCommonInfo) -> O>(&self, cb: F) -> O {
        cb(self.dynamic_common_info.read().deref())
    }

    pub(crate) fn with_dynamic_info_mut<O, F: FnOnce(&mut DynamicCommonInfo) -> O>(
        &self,
        cb: F,
    ) -> O {
        cb(self.dynamic_common_info.write().deref_mut())
    }
}

impl DeviceClassMatcher<fidl_fuchsia_net_interfaces::PortClass> for LoopbackInfo {
    fn device_class_matches(&self, port_class: &fidl_fuchsia_net_interfaces::PortClass) -> bool {
        match port_class {
            fidl_fuchsia_net_interfaces::PortClass::Loopback(
                fidl_fuchsia_net_interfaces::Empty {},
            ) => true,
            fidl_fuchsia_net_interfaces::PortClass::Blackhole(
                fidl_fuchsia_net_interfaces::Empty {},
            )
            | fidl_fuchsia_net_interfaces::PortClass::Device(_) => false,
            fidl_fuchsia_net_interfaces::PortClass::__SourceBreaking { unknown_ordinal } => {
                panic!("unknown device class ordinal {unknown_ordinal:?}")
            }
        }
    }
}

/// Dynamic information common to all Netdevice backed devices.
#[derive(Debug)]
pub(crate) struct DynamicNetdeviceInfo {
    pub(crate) phy_up: bool,
    pub(crate) common_info: DynamicCommonInfo,
}

/// Static information common to all Netdevice backed devices.
#[derive(Debug)]
pub(crate) struct StaticNetdeviceInfo {
    pub(crate) handler: netdevice_worker::PortHandler,
    pub(crate) tx_notifier: NeedsDataNotifier,
}

impl StaticNetdeviceInfo {
    fn device_class_matches(&self, port_class: &fidl_fuchsia_net_interfaces::PortClass) -> bool {
        match port_class {
            fidl_fuchsia_net_interfaces::PortClass::Loopback(
                fidl_fuchsia_net_interfaces::Empty {},
            )
            | fidl_fuchsia_net_interfaces::PortClass::Blackhole(
                fidl_fuchsia_net_interfaces::Empty {},
            ) => false,
            fidl_fuchsia_net_interfaces::PortClass::Device(port_class) => {
                *port_class == self.handler.port_class()
            }
            fidl_fuchsia_net_interfaces::PortClass::__SourceBreaking { unknown_ordinal } => {
                panic!("unknown device class ordinal {unknown_ordinal:?}")
            }
        }
    }
}

/// Dynamic information for Ethernet devices
#[derive(Derivative)]
#[derivative(Debug)]
pub(crate) struct DynamicEthernetInfo {
    pub(crate) netdevice: DynamicNetdeviceInfo,
    #[derivative(Debug = "ignore")]
    pub(crate) neighbor_event_sink: futures::channel::mpsc::UnboundedSender<neighbor_worker::Event>,
}

/// Ethernet device information.
#[derive(Debug)]
pub(crate) struct EthernetInfo {
    pub(crate) dynamic_info: CoreRwLock<DynamicEthernetInfo>,
    pub(crate) common_info: StaticCommonInfo,
    pub(crate) netdevice: StaticNetdeviceInfo,
    pub(crate) mac: UnicastAddr<Mac>,
    // We must keep the mac proxy alive to maintain our multicast filtering mode
    // selection set.
    pub(crate) _mac_proxy: fhardware_network::MacAddressingProxy,
}

impl EthernetInfo {
    pub(crate) fn with_dynamic_info<O, F: FnOnce(&DynamicEthernetInfo) -> O>(&self, cb: F) -> O {
        let dynamic = self.dynamic_info.read();
        cb(dynamic.deref())
    }

    pub(crate) fn with_dynamic_info_mut<O, F: FnOnce(&mut DynamicEthernetInfo) -> O>(
        &self,
        cb: F,
    ) -> O {
        let mut dynamic = self.dynamic_info.write();
        cb(dynamic.deref_mut())
    }
}

impl DeviceClassMatcher<fidl_fuchsia_net_interfaces::PortClass> for EthernetInfo {
    fn device_class_matches(&self, device_class: &fidl_fuchsia_net_interfaces::PortClass) -> bool {
        self.netdevice.device_class_matches(device_class)
    }
}

/// Pure IP device information.
#[derive(Debug)]
pub(crate) struct PureIpDeviceInfo {
    pub(crate) common_info: StaticCommonInfo,
    pub(crate) netdevice: StaticNetdeviceInfo,
    pub(crate) dynamic_info: CoreRwLock<DynamicNetdeviceInfo>,
}

impl PureIpDeviceInfo {
    pub(crate) fn with_dynamic_info<O, F: FnOnce(&DynamicNetdeviceInfo) -> O>(&self, cb: F) -> O {
        let dynamic = self.dynamic_info.read();
        cb(dynamic.deref())
    }

    pub(crate) fn with_dynamic_info_mut<O, F: FnOnce(&mut DynamicNetdeviceInfo) -> O>(
        &self,
        cb: F,
    ) -> O {
        let mut dynamic = self.dynamic_info.write();
        cb(dynamic.deref_mut())
    }
}

impl DeviceClassMatcher<fidl_fuchsia_net_interfaces::PortClass> for PureIpDeviceInfo {
    fn device_class_matches(&self, device_class: &fidl_fuchsia_net_interfaces::PortClass) -> bool {
        self.netdevice.device_class_matches(device_class)
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

impl DeviceIdAndNameMatcher for DeviceIdAndName {
    fn id_matches(&self, id: &NonZeroU64) -> bool {
        self.id == *id
    }

    fn name_matches(&self, name: &str) -> bool {
        self.name == name
    }
}
