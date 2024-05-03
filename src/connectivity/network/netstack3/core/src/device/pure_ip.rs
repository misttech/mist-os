// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A pure IP device, capable of directly sending/receiving IPv4 & IPv6 packets.

use alloc::vec::Vec;
use core::convert::Infallible as Never;
use net_types::ip::{Ip, IpVersion, Mtu};
use packet::{Buf, BufferMut, Serializer};
use tracing::warn;

use crate::{
    context::{CoreTimerContext, ResourceCounterContext, TimerContext},
    device::{
        self,
        queue::{
            tx::{BufVecU8Allocator, TransmitQueue, TransmitQueueHandler},
            TransmitQueueFrameError,
        },
        state::DeviceStateSpec,
        BaseDeviceId, BasePrimaryDeviceId, BaseWeakDeviceId, Device, DeviceCounters,
        DeviceIdContext, DeviceLayerTypes, DeviceReceiveFrameSpec, DeviceSendFrameError,
        PureIpDeviceCounters,
    },
    sync::RwLock,
};

mod integration;

/// A weak device ID identifying a pure IP device.
///
/// This device ID is like [`WeakDeviceId`] but specifically for pure IP
/// devices.
///
/// [`WeakDeviceId`]: crate::device::WeakDeviceId
pub type PureIpWeakDeviceId<BT> = BaseWeakDeviceId<PureIpDevice, BT>;

/// A strong device ID identifying a pure IP device.
///
/// This device ID is like [`DeviceId`] but specifically for pure IP devices.
///
/// [`DeviceId`]: crate::device::DeviceId
pub type PureIpDeviceId<BT> = BaseDeviceId<PureIpDevice, BT>;

/// The primary reference for a pure IP device.
pub(crate) type PureIpPrimaryDeviceId<BT> = BasePrimaryDeviceId<PureIpDevice, BT>;

/// A marker type identifying a pure IP device.
#[derive(Copy, Clone)]
pub enum PureIpDevice {}

/// The parameters required to create a pure IP device.
#[derive(Debug)]
pub struct PureIpDeviceCreationProperties {
    /// The MTU of the device.
    pub mtu: Mtu,
}

/// Metadata for IP packets held in the TX queue.
pub struct PureIpDeviceTxQueueFrameMetadata {
    /// The IP version of the sent packet.
    ip_version: IpVersion,
}

/// Metadata for sending IP packets from a device socket.
#[derive(Debug, PartialEq)]
pub struct PureIpHeaderParams {
    /// The IP version of the packet to send.
    pub ip_version: IpVersion,
}

/// State for a pure IP device.
pub struct PureIpDeviceState {
    /// The device's dynamic state.
    dynamic_state: RwLock<DynamicPureIpDeviceState>,
    /// The device's transmit queue.
    tx_queue: TransmitQueue<PureIpDeviceTxQueueFrameMetadata, Buf<Vec<u8>>, BufVecU8Allocator>,
    /// Counters specific to pure IP devices.
    counters: PureIpDeviceCounters,
}

/// Dynamic state for a pure IP device.
pub(crate) struct DynamicPureIpDeviceState {
    /// The MTU of the device.
    pub(crate) mtu: Mtu,
}

impl Device for PureIpDevice {}

impl DeviceStateSpec for PureIpDevice {
    type Link<BT: DeviceLayerTypes> = PureIpDeviceState;
    type External<BT: DeviceLayerTypes> = BT::PureIpDeviceState;
    type CreationProperties = PureIpDeviceCreationProperties;
    type Counters = PureIpDeviceCounters;
    const IS_LOOPBACK: bool = false;
    const DEBUG_TYPE: &'static str = "PureIP";
    type TimerId<D: device::WeakId> = Never;

    fn new_link_state<
        CC: CoreTimerContext<Self::TimerId<CC::WeakDeviceId>, BC> + DeviceIdContext<Self>,
        BC: DeviceLayerTypes + TimerContext,
    >(
        _bindings_ctx: &mut BC,
        _self_id: CC::WeakDeviceId,
        PureIpDeviceCreationProperties { mtu }: Self::CreationProperties,
    ) -> Self::Link<BC> {
        PureIpDeviceState {
            dynamic_state: RwLock::new(DynamicPureIpDeviceState { mtu }),
            tx_queue: Default::default(),
            counters: PureIpDeviceCounters::default(),
        }
    }
}

/// Metadata for IP packets received on a pure IP device.
pub struct PureIpDeviceReceiveFrameMetadata<D> {
    /// The device a packet was received on.
    pub device_id: D,
    /// The IP version of the received packet.
    pub ip_version: IpVersion,
}

impl DeviceReceiveFrameSpec for PureIpDevice {
    type FrameMetadata<D> = PureIpDeviceReceiveFrameMetadata<D>;
}

/// Provides access to a pure IP device's state.
pub(crate) trait PureIpDeviceStateContext: DeviceIdContext<PureIpDevice> {
    /// Calls the function with an immutable reference to the pure IP device's
    /// dynamic state.
    fn with_pure_ip_state<O, F: FnOnce(&DynamicPureIpDeviceState) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O;

    /// Calls the function with a mutable reference to the pure IP device's
    /// dynamic state.
    fn with_pure_ip_state_mut<O, F: FnOnce(&mut DynamicPureIpDeviceState) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O;
}

/// Enqueues the given IP packet on the TX queue for the given [`PureIpDevice`].
pub(super) fn send_ip_frame<CC, BC, I, S>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    packet: S,
) -> Result<(), S>
where
    CC: TransmitQueueHandler<PureIpDevice, BC, Meta = PureIpDeviceTxQueueFrameMetadata>
        + ResourceCounterContext<CC::DeviceId, DeviceCounters>,
    I: Ip,
    S: Serializer,
    S::Buffer: BufferMut,
{
    core_ctx.increment(device_id, |counters| &counters.send_total_frames);
    match I::VERSION {
        IpVersion::V4 => core_ctx.increment(device_id, |counters| &counters.send_ipv4_frame),
        IpVersion::V6 => core_ctx.increment(device_id, |counters| &counters.send_ipv6_frame),
    }

    let result = TransmitQueueHandler::<PureIpDevice, _>::queue_tx_frame(
        core_ctx,
        bindings_ctx,
        device_id,
        PureIpDeviceTxQueueFrameMetadata { ip_version: I::VERSION },
        packet,
    );
    match result {
        Ok(()) => {
            core_ctx.increment(device_id, |counters| &counters.send_frame);
            Ok(())
        }
        Err(TransmitQueueFrameError::NoQueue(DeviceSendFrameError::DeviceNotReady(()))) => {
            core_ctx.increment(device_id, |counters| &counters.send_dropped_no_queue);
            warn!("device {device_id:?} not ready to send frame.");
            Ok(())
        }
        Err(TransmitQueueFrameError::QueueFull(s)) => {
            core_ctx.increment(device_id, |counters| &counters.send_queue_full);
            Err(s)
        }
        Err(TransmitQueueFrameError::SerializeError(s)) => {
            core_ctx.increment(device_id, |counters| &counters.send_serialize_error);
            Err(s)
        }
    }
}

/// Gets the MTU of the given [`PureIpDevice`].
pub(super) fn get_mtu<CC: PureIpDeviceStateContext>(
    core_ctx: &mut CC,
    device_id: &CC::DeviceId,
) -> Mtu {
    core_ctx.with_pure_ip_state(device_id, |DynamicPureIpDeviceState { mtu }| *mtu)
}

/// Updates the MTU of the given [`PureIpDevice`].
pub(super) fn set_mtu<CC: PureIpDeviceStateContext>(
    core_ctx: &mut CC,
    device_id: &CC::DeviceId,
    new_mtu: Mtu,
) {
    core_ctx.with_pure_ip_state_mut(device_id, |DynamicPureIpDeviceState { mtu }| *mtu = new_mtu)
}
