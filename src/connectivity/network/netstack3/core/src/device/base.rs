// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use alloc::{collections::HashMap, vec::Vec};
use core::{
    fmt::{Debug, Display},
    num::NonZeroU64,
};

use derivative::Derivative;
use lock_order::lock::{OrderedLockAccess, OrderedLockRef};
use net_types::{
    ethernet::Mac,
    ip::{Ip, IpVersion, Ipv4, Ipv6},
};
use packet::Buf;

use crate::{
    context::{
        HandleableTimer, InstantContext, ReferenceNotifiers, TimerBindingsTypes, TimerHandler,
    },
    counters::Counter,
    device::{
        arp::ArpCounters,
        ethernet::{EthernetLinkDevice, EthernetTimerId},
        id::{
            BaseDeviceId, BasePrimaryDeviceId, DeviceId, EthernetDeviceId, EthernetPrimaryDeviceId,
            EthernetWeakDeviceId,
        },
        loopback::{LoopbackDeviceId, LoopbackPrimaryDeviceId},
        pure_ip::{PureIpDeviceId, PureIpPrimaryDeviceId},
        queue::{rx::ReceiveQueueBindingsContext, tx::TransmitQueueBindingsContext},
        socket::{self, HeldSockets},
        state::DeviceStateSpec,
    },
    filter::FilterBindingsTypes,
    inspect::Inspectable,
    ip::nud::{LinkResolutionContext, NudCounters},
    sync::RwLock,
    Inspector,
};

pub(crate) use netstack3_base::{
    AnyDevice, Device, DeviceIdAnyCompatContext, DeviceIdContext, EitherDeviceId, FrameDestination,
    RecvIpFrameMeta,
};

/// Iterator over devices.
///
/// Implements `Iterator<Item=DeviceId<C>>` by pulling from provided loopback
/// and ethernet device ID iterators. This struct only exists as a named type
/// so it can be an associated type on impls of the [`IpDeviceContext`] trait.
pub struct DevicesIter<'s, BT: DeviceLayerTypes> {
    pub(super) ethernet:
        alloc::collections::hash_map::Values<'s, EthernetDeviceId<BT>, EthernetPrimaryDeviceId<BT>>,
    pub(super) pure_ip:
        alloc::collections::hash_map::Values<'s, PureIpDeviceId<BT>, PureIpPrimaryDeviceId<BT>>,
    pub(super) loopback: core::option::Iter<'s, LoopbackPrimaryDeviceId<BT>>,
}

impl<'s, BT: DeviceLayerTypes> Iterator for DevicesIter<'s, BT> {
    type Item = DeviceId<BT>;

    fn next(&mut self) -> Option<Self::Item> {
        let Self { ethernet, pure_ip, loopback } = self;
        ethernet
            .map(|primary| primary.clone_strong().into())
            .chain(pure_ip.map(|primary| primary.clone_strong().into()))
            .chain(loopback.map(|primary| primary.clone_strong().into()))
            .next()
    }
}

pub enum Ipv6DeviceLinkLayerAddr {
    Mac(Mac),
    // Add other link-layer address types as needed.
}

impl AsRef<[u8]> for Ipv6DeviceLinkLayerAddr {
    fn as_ref(&self) -> &[u8] {
        match self {
            Ipv6DeviceLinkLayerAddr::Mac(a) => a.as_ref(),
        }
    }
}

/// The identifier for timer events in the device layer.
#[derive(Derivative)]
#[derivative(
    Clone(bound = ""),
    Eq(bound = ""),
    PartialEq(bound = ""),
    Hash(bound = ""),
    Debug(bound = "")
)]
pub(crate) struct DeviceLayerTimerId<BT: DeviceLayerTypes>(DeviceLayerTimerIdInner<BT>);

#[derive(Derivative)]
#[derivative(
    Clone(bound = ""),
    Eq(bound = ""),
    PartialEq(bound = ""),
    Hash(bound = ""),
    Debug(bound = "")
)]
enum DeviceLayerTimerIdInner<BT: DeviceLayerTypes> {
    /// A timer event for an Ethernet device.
    Ethernet(EthernetTimerId<EthernetWeakDeviceId<BT>>),
}

impl<BT: DeviceLayerTypes> From<EthernetTimerId<EthernetWeakDeviceId<BT>>>
    for DeviceLayerTimerId<BT>
{
    fn from(id: EthernetTimerId<EthernetWeakDeviceId<BT>>) -> DeviceLayerTimerId<BT> {
        DeviceLayerTimerId(DeviceLayerTimerIdInner::Ethernet(id))
    }
}

impl<CC, BT> HandleableTimer<CC, BT> for DeviceLayerTimerId<BT>
where
    BT: DeviceLayerTypes,
    CC: TimerHandler<BT, EthernetTimerId<EthernetWeakDeviceId<BT>>>,
{
    fn handle(self, core_ctx: &mut CC, bindings_ctx: &mut BT) {
        let Self(id) = self;
        match id {
            DeviceLayerTimerIdInner::Ethernet(id) => core_ctx.handle_timer(bindings_ctx, id),
        }
    }
}

#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub struct Devices<BT: DeviceLayerTypes> {
    pub(super) ethernet: HashMap<EthernetDeviceId<BT>, EthernetPrimaryDeviceId<BT>>,
    pub(super) pure_ip: HashMap<PureIpDeviceId<BT>, PureIpPrimaryDeviceId<BT>>,
    pub(super) loopback: Option<LoopbackPrimaryDeviceId<BT>>,
}

/// The state associated with the device layer.
pub struct DeviceLayerState<BT: DeviceLayerTypes> {
    pub(super) devices: RwLock<Devices<BT>>,
    pub(super) origin: OriginTracker,
    pub(super) shared_sockets: HeldSockets<BT>,
    pub(super) counters: DeviceCounters,
    pub(super) ethernet_counters: EthernetDeviceCounters,
    pub(super) pure_ip_counters: PureIpDeviceCounters,
    pub(super) nud_v4_counters: NudCounters<Ipv4>,
    pub(super) nud_v6_counters: NudCounters<Ipv6>,
    pub(super) arp_counters: ArpCounters,
}

impl<BT: DeviceLayerTypes> DeviceLayerState<BT> {
    pub(crate) fn counters(&self) -> &DeviceCounters {
        &self.counters
    }

    pub(crate) fn ethernet_counters(&self) -> &EthernetDeviceCounters {
        &self.ethernet_counters
    }

    pub(crate) fn pure_ip_counters(&self) -> &PureIpDeviceCounters {
        &self.pure_ip_counters
    }

    pub(crate) fn nud_counters<I: Ip>(&self) -> &NudCounters<I> {
        I::map_ip((), |()| &self.nud_v4_counters, |()| &self.nud_v6_counters)
    }

    pub(crate) fn arp_counters(&self) -> &ArpCounters {
        &self.arp_counters
    }
}

impl<BT: DeviceLayerTypes> OrderedLockAccess<Devices<BT>> for DeviceLayerState<BT> {
    type Lock = RwLock<Devices<BT>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.devices)
    }
}

/// Counters for ethernet devices.
#[derive(Default)]
pub struct EthernetDeviceCounters {
    /// Count of incoming frames dropped because the destination address was for
    /// another device.
    pub recv_ethernet_other_dest: Counter,
    /// Count of incoming frames dropped due to an unsupported ethertype.
    pub recv_unsupported_ethertype: Counter,
    /// Count of incoming frames dropped due to an empty ethertype.
    pub recv_no_ethertype: Counter,
}

impl Inspectable for EthernetDeviceCounters {
    fn record<I: Inspector>(&self, inspector: &mut I) {
        inspector.record_child("Ethernet", |inspector| {
            crate::counters::inspect_ethernet_device_counters(inspector, self)
        })
    }
}

/// Counters for pure IP devices.
#[derive(Default)]
pub struct PureIpDeviceCounters {}

impl Inspectable for PureIpDeviceCounters {
    fn record<I: Inspector>(&self, _inspector: &mut I) {}
}

/// Device layer counters.
#[derive(Default)]
pub struct DeviceCounters {
    /// Count of outgoing frames which enter the device layer (but may or may
    /// not have been dropped prior to reaching the wire).
    pub send_total_frames: Counter,
    /// Count of frames sent.
    pub send_frame: Counter,
    /// Count of frames that failed to send because of a full Tx queue.
    pub send_queue_full: Counter,
    /// Count of frames that failed to send because of a serialization error.
    pub send_serialize_error: Counter,
    /// Count of frames received.
    pub recv_frame: Counter,
    /// Count of incoming frames dropped due to a parsing error.
    pub recv_parse_error: Counter,
    /// Count of incoming frames containing an IPv4 packet delivered.
    pub recv_ipv4_delivered: Counter,
    /// Count of incoming frames containing an IPv6 packet delivered.
    pub recv_ipv6_delivered: Counter,
    /// Count of sent frames containing an IPv4 packet.
    pub send_ipv4_frame: Counter,
    /// Count of sent frames containing an IPv6 packet.
    pub send_ipv6_frame: Counter,
    /// Count of frames that failed to send because there was no Tx queue.
    pub send_dropped_no_queue: Counter,
}

impl Inspectable for DeviceCounters {
    fn record<I: Inspector>(&self, inspector: &mut I) {
        crate::counters::inspect_device_counters(inspector, self)
    }
}
/// Light-weight tracker for recording the source of some instance.
///
/// This should be held as a field in a parent type that is cloned into each
/// child instance. Then, the origin of a child instance can be verified by
/// asserting equality against the parent's field.
///
/// This is only enabled in debug builds; in non-debug builds, all
/// `OriginTracker` instances are identical so all operations are no-ops.
// TODO(https://fxbug.dev/320078167): Move this and OriginTrackerContext out of
// the device module and apply to more places.
#[derive(Clone, Debug, PartialEq)]
pub struct OriginTracker(#[cfg(debug_assertions)] u64);

impl OriginTracker {
    /// Creates a new `OriginTracker` that isn't derived from any other
    /// instance.
    ///
    /// In debug builds, this creates a unique `OriginTracker` that won't be
    /// equal to any instances except those cloned from it. In non-debug builds
    /// all `OriginTracker` instances are identical.
    #[cfg_attr(not(debug_assertions), inline)]
    fn new() -> Self {
        Self(
            #[cfg(debug_assertions)]
            {
                static COUNTER: core::sync::atomic::AtomicU64 =
                    core::sync::atomic::AtomicU64::new(0);
                COUNTER.fetch_add(1, core::sync::atomic::Ordering::Relaxed)
            },
        )
    }
}

/// A trait abstracting a context containing an [`OriginTracker`].
///
/// This allows API structs to extract origin from contexts when creating
/// resources.
pub trait OriginTrackerContext {
    /// Gets the origin tracker for this context.
    fn origin_tracker(&mut self) -> OriginTracker;
}

/// A context providing facilities to store and remove primary device IDs.
///
/// This allows the device layer APIs to be written generically on `D`.
pub trait DeviceCollectionContext<D: Device + DeviceStateSpec, BT: DeviceLayerTypes>:
    DeviceIdContext<D>
{
    /// Adds `device` to the device collection.
    fn insert(&mut self, device: BasePrimaryDeviceId<D, BT>);

    /// Removes `device` from the collection, if it exists.
    fn remove(&mut self, device: &BaseDeviceId<D, BT>) -> Option<BasePrimaryDeviceId<D, BT>>;
}

/// Provides abstractions over the frame metadata received from bindings for
/// implementers of [`Device`].
///
/// This trait allows [`api::DeviceApi`] to provide a single entrypoint for
/// frames from bindings.
pub trait DeviceReceiveFrameSpec {
    /// The frame metadata for ingress frames, where `D` is a device identifier.
    type FrameMetadata<D>;
}

impl<BC: DeviceLayerTypes + socket::DeviceSocketBindingsContext<DeviceId<BC>>>
    DeviceLayerState<BC>
{
    /// Creates a new [`DeviceLayerState`] instance.
    pub(crate) fn new() -> Self {
        Self {
            devices: Default::default(),
            origin: OriginTracker::new(),
            shared_sockets: Default::default(),
            counters: Default::default(),
            ethernet_counters: EthernetDeviceCounters::default(),
            pure_ip_counters: PureIpDeviceCounters::default(),
            nud_v4_counters: Default::default(),
            nud_v6_counters: Default::default(),
            arp_counters: Default::default(),
        }
    }
}

/// Provides associated types used in the device layer.
pub trait DeviceLayerStateTypes: InstantContext + FilterBindingsTypes {
    /// The state associated with loopback devices.
    type LoopbackDeviceState: Send + Sync + DeviceClassMatcher<Self::DeviceClass>;

    /// The state associated with ethernet devices.
    type EthernetDeviceState: Send + Sync + DeviceClassMatcher<Self::DeviceClass>;

    /// The state associated with pure IP devices.
    type PureIpDeviceState: Send + Sync + DeviceClassMatcher<Self::DeviceClass>;

    /// An opaque identifier that is available from both strong and weak device
    /// references.
    type DeviceIdentifier: Send + Sync + Debug + Display + DeviceIdAndNameMatcher;
}

/// Provides matching functionality for the device class of a device installed
/// in the netstack.
pub trait DeviceClassMatcher<DeviceClass> {
    /// Returns whether the provided device class matches the class of the
    /// device.
    fn device_class_matches(&self, device_class: &DeviceClass) -> bool;
}

/// Provides matching functionality for the ID and name of a device installed in
/// the netstack.
pub trait DeviceIdAndNameMatcher {
    /// Returns whether the provided ID matches the ID of the device.
    fn id_matches(&self, id: &NonZeroU64) -> bool;

    /// Returns whether the provided name matches the name of the device.
    fn name_matches(&self, name: &str) -> bool;
}

/// Provides associated types used in the device layer.
///
/// This trait groups together state types used throughout the device layer. It
/// is blanket-implemented for all types that implement
/// [`socket::DeviceSocketTypes`] and [`DeviceLayerStateTypes`].
pub trait DeviceLayerTypes:
    DeviceLayerStateTypes
    + socket::DeviceSocketTypes
    + LinkResolutionContext<EthernetLinkDevice>
    + TimerBindingsTypes
    + ReferenceNotifiers
    + 'static
{
}
impl<
        BC: DeviceLayerStateTypes
            + socket::DeviceSocketTypes
            + LinkResolutionContext<EthernetLinkDevice>
            + TimerBindingsTypes
            + ReferenceNotifiers
            + 'static,
    > DeviceLayerTypes for BC
{
}

/// An event dispatcher for the device layer.
pub trait DeviceLayerEventDispatcher:
    DeviceLayerTypes
    + ReceiveQueueBindingsContext<LoopbackDeviceId<Self>>
    + TransmitQueueBindingsContext<EthernetDeviceId<Self>>
    + TransmitQueueBindingsContext<LoopbackDeviceId<Self>>
    + TransmitQueueBindingsContext<PureIpDeviceId<Self>>
    + Sized
{
    /// Send a frame to an Ethernet device driver.
    ///
    /// See [`DeviceSendFrameError`] for the ways this call may fail; all other
    /// errors are silently ignored and reported as success. Implementations are
    /// expected to gracefully handle non-conformant but correctable input, e.g.
    /// by padding too-small frames.
    fn send_ethernet_frame(
        &mut self,
        device: &EthernetDeviceId<Self>,
        frame: Buf<Vec<u8>>,
    ) -> Result<(), DeviceSendFrameError<Buf<Vec<u8>>>>;

    /// Send an IP packet to an IP device driver.
    ///
    /// See [`DeviceSendFrameError`] for the ways this call may fail; all other
    /// errors are silently ignored and reported as success. Implementations are
    /// expected to gracefully handle non-conformant but correctable input, e.g.
    /// by padding too-small frames.
    fn send_ip_packet(
        &mut self,
        device: &PureIpDeviceId<Self>,
        packet: Buf<Vec<u8>>,
        ip_version: IpVersion,
    ) -> Result<(), DeviceSendFrameError<Buf<Vec<u8>>>>;
}

/// An error encountered when sending a frame.
#[derive(Debug, PartialEq, Eq)]
pub enum DeviceSendFrameError<T> {
    /// The device is not ready to send frames.
    DeviceNotReady(T),
}

#[cfg(test)]
pub(crate) mod testutil {
    #[cfg(test)]
    pub(crate) use netstack3_base::testutil::{
        FakeDeviceId, FakeReferencyDeviceId, FakeStrongDeviceId, FakeWeakDeviceId,
        MultipleDevicesId,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_tracker() {
        let tracker = OriginTracker::new();
        if cfg!(debug_assertions) {
            assert_ne!(tracker, OriginTracker::new());
        } else {
            assert_eq!(tracker, OriginTracker::new());
        }
        assert_eq!(tracker.clone(), tracker);
    }
}
