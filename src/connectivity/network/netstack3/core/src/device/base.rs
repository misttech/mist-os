// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use alloc::{collections::HashMap, vec::Vec};
use core::{
    fmt::{Debug, Display},
    marker::PhantomData,
    num::NonZeroU64,
};

use derivative::Derivative;
use lock_order::{lock::UnlockedAccess, wrap::prelude::*};
use net_types::{
    ethernet::Mac,
    ip::{Ip, IpVersion, Ipv4, Ipv6},
    BroadcastAddr, MulticastAddr,
};
use packet::Buf;

use crate::{
    context::{CounterContext, InstantContext, TimerHandler},
    counters::Counter,
    device::{
        arp::ArpCounters,
        ethernet::{EthernetLinkDevice, EthernetTimerId},
        id::{
            BaseDeviceId, BasePrimaryDeviceId, DeviceId, EthernetDeviceId, EthernetPrimaryDeviceId,
            StrongId, WeakId,
        },
        loopback::{LoopbackDeviceId, LoopbackPrimaryDeviceId},
        pure_ip::{PureIpDeviceId, PureIpPrimaryDeviceId},
        socket::{self, HeldSockets},
        state::{DeviceStateSpec, IpLinkDeviceStateInner},
    },
    filter::FilterBindingsTypes,
    inspect::Inspectable,
    ip::{
        device::{
            nud::{LinkResolutionContext, NudCounters},
            state::IpDeviceFlags,
            IpDeviceIpExt, IpDeviceStateContext,
        },
        forwarding::IpForwardingDeviceContext,
        types::RawMetric,
    },
    sync::RwLock,
    BindingsContext, CoreCtx, Inspector, StackState,
};

/// A device.
///
/// `Device` is used to identify a particular device implementation. It
/// is only intended to exist at the type level, never instantiated at runtime.
pub trait Device: 'static {}

/// Marker type for a generic device.
pub enum AnyDevice {}

impl Device for AnyDevice {}

/// An execution context which provides device ID types type for various
/// netstack internals to share.
pub trait DeviceIdContext<D: Device> {
    /// The type of device IDs.
    type DeviceId: StrongId<Weak = Self::WeakDeviceId> + 'static;

    /// The type of weakly referenced device IDs.
    type WeakDeviceId: WeakId<Strong = Self::DeviceId> + 'static;

    /// Returns a weak ID for the strong ID.
    fn downgrade_device_id(&self, device_id: &Self::DeviceId) -> Self::WeakDeviceId;

    /// Attempts to upgrade the weak device ID to a strong ID.
    ///
    /// Returns `None` if the device has been removed.
    fn upgrade_weak_device_id(&self, weak_device_id: &Self::WeakDeviceId)
        -> Option<Self::DeviceId>;
}

pub(super) struct RecvIpFrameMeta<D, I: Ip> {
    /// The device on which the IP frame was received.
    pub(super) device: D,
    /// The link-layer destination address from the link-layer frame, if any.
    /// `None` if the IP frame originated above the link-layer (e.g. pure IP
    /// devices).
    // NB: In the future, this field may also be `None` to represent link-layer
    // protocols without destination addresses (i.e. PPP), but at the moment no
    // such protocols are supported.
    pub(super) frame_dst: Option<FrameDestination>,
    pub(super) _marker: PhantomData<I>,
}

impl<D, I: Ip> RecvIpFrameMeta<D, I> {
    pub(super) fn new(device: D, frame_dst: Option<FrameDestination>) -> RecvIpFrameMeta<D, I> {
        RecvIpFrameMeta { device, frame_dst, _marker: PhantomData }
    }
}

/// Iterator over devices.
///
/// Implements `Iterator<Item=DeviceId<C>>` by pulling from provided loopback
/// and ethernet device ID iterators. This struct only exists as a named type
/// so it can be an associated type on impls of the [`IpDeviceContext`] trait.
pub struct DevicesIter<'s, BC: BindingsContext> {
    pub(super) ethernet:
        alloc::collections::hash_map::Values<'s, EthernetDeviceId<BC>, EthernetPrimaryDeviceId<BC>>,
    pub(super) loopback: core::option::Iter<'s, LoopbackPrimaryDeviceId<BC>>,
}

impl<'s, BC: BindingsContext> Iterator for DevicesIter<'s, BC> {
    type Item = DeviceId<BC>;

    fn next(&mut self) -> Option<Self::Item> {
        let Self { ethernet, loopback } = self;
        ethernet
            .map(|primary| primary.clone_strong().into())
            .chain(loopback.map(|primary| primary.clone_strong().into()))
            .next()
    }
}

impl<I: IpDeviceIpExt, BC: BindingsContext, L> IpForwardingDeviceContext<I> for CoreCtx<'_, BC, L>
where
    Self: IpDeviceStateContext<I, BC, DeviceId = DeviceId<BC>>,
{
    fn get_routing_metric(&mut self, device_id: &Self::DeviceId) -> RawMetric {
        crate::device::integration::with_ip_device_state(self, device_id, |state| {
            *state.unlocked_access::<crate::lock_ordering::RoutingMetric>()
        })
    }

    fn is_ip_device_enabled(&mut self, device_id: &Self::DeviceId) -> bool {
        IpDeviceStateContext::<I, _>::with_ip_device_flags(
            self,
            device_id,
            |IpDeviceFlags { ip_enabled }| *ip_enabled,
        )
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
    Ethernet(EthernetTimerId<EthernetDeviceId<BT>>),
}

impl<BT: DeviceLayerTypes> From<EthernetTimerId<EthernetDeviceId<BT>>> for DeviceLayerTimerId<BT> {
    fn from(id: EthernetTimerId<EthernetDeviceId<BT>>) -> DeviceLayerTimerId<BT> {
        DeviceLayerTimerId(DeviceLayerTimerIdInner::Ethernet(id))
    }
}

impl_timer_context!(
    BT: DeviceLayerTypes,
    DeviceLayerTimerId<BT>,
    EthernetTimerId<EthernetDeviceId<BT>>,
    DeviceLayerTimerId(DeviceLayerTimerIdInner::Ethernet(id)),
    id
);

impl<CC, BT> TimerHandler<BT, DeviceLayerTimerId<BT>> for CC
where
    BT: DeviceLayerTypes,
    CC: TimerHandler<BT, EthernetTimerId<EthernetDeviceId<BT>>>,
{
    fn handle_timer(
        &mut self,
        bindings_ctx: &mut BT,
        DeviceLayerTimerId(id): DeviceLayerTimerId<BT>,
    ) {
        match id {
            DeviceLayerTimerIdInner::Ethernet(id) => self.handle_timer(bindings_ctx, id),
        }
    }
}

/// The type of address used as the destination address in a device-layer frame.
///
/// `FrameDestination` is used to implement RFC 1122 section 3.2.2 and RFC 4443
/// section 2.4.e, which govern when to avoid sending an ICMP error message for
/// ICMP and ICMPv6 respectively.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FrameDestination {
    /// A unicast address - one which is neither multicast nor broadcast.
    Individual {
        /// Whether the frame's destination address belongs to the receiver.
        local: bool,
    },
    /// A multicast address; if the addressing scheme supports overlap between
    /// multicast and broadcast, then broadcast addresses should use the
    /// `Broadcast` variant.
    Multicast,
    /// A broadcast address; if the addressing scheme supports overlap between
    /// multicast and broadcast, then broadcast addresses should use the
    /// `Broadcast` variant.
    Broadcast,
}

impl FrameDestination {
    /// Is this `FrameDestination::Broadcast`?
    pub(crate) fn is_broadcast(self) -> bool {
        self == FrameDestination::Broadcast
    }

    pub(crate) fn from_dest(destination: Mac, local_mac: Mac) -> Self {
        BroadcastAddr::new(destination)
            .map(Into::into)
            .or_else(|| MulticastAddr::new(destination).map(Into::into))
            .unwrap_or_else(|| FrameDestination::Individual { local: destination == local_mac })
    }
}

impl From<BroadcastAddr<Mac>> for FrameDestination {
    fn from(_value: BroadcastAddr<Mac>) -> Self {
        Self::Broadcast
    }
}

impl From<MulticastAddr<Mac>> for FrameDestination {
    fn from(_value: MulticastAddr<Mac>) -> Self {
        Self::Multicast
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
pub(crate) struct DeviceLayerState<BT: DeviceLayerTypes> {
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

impl<BC: BindingsContext> UnlockedAccess<crate::lock_ordering::DeviceCounters> for StackState<BC> {
    type Data = DeviceCounters;
    type Guard<'l> = &'l DeviceCounters where Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        self.device_counters()
    }
}

impl<T, BC: BindingsContext> UnlockedAccess<crate::lock_ordering::DeviceCounters>
    for IpLinkDeviceStateInner<T, BC>
{
    type Data = DeviceCounters;
    type Guard<'l> = &'l DeviceCounters where Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        &self.counters
    }
}

impl<BC: BindingsContext, L> CounterContext<DeviceCounters> for CoreCtx<'_, BC, L> {
    fn with_counters<O, F: FnOnce(&DeviceCounters) -> O>(&self, cb: F) -> O {
        cb(self.unlocked_access::<crate::lock_ordering::DeviceCounters>())
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
    + 'static
{
}
impl<
        BC: DeviceLayerStateTypes
            + socket::DeviceSocketTypes
            + LinkResolutionContext<EthernetLinkDevice>
            + 'static,
    > DeviceLayerTypes for BC
{
}

/// An event dispatcher for the device layer.
///
/// See the `EventDispatcher` trait in the crate root for more details.
pub trait DeviceLayerEventDispatcher: DeviceLayerTypes + Sized {
    /// Signals to the dispatcher that RX frames are available and ready to be
    /// handled by [`handle_queued_rx_packets`].
    ///
    /// Implementations must make sure that [`handle_queued_rx_packets`] is
    /// scheduled to be called as soon as possible so that enqueued RX frames
    /// are promptly handled.
    fn wake_rx_task(&mut self, device: &LoopbackDeviceId<Self>);

    /// Signals to the dispatcher that TX frames are available and ready to be
    /// sent by [`transmit_queued_tx_frames`].
    ///
    /// Implementations must make sure that [`transmit_queued_tx_frames`] is
    /// scheduled to be called as soon as possible so that enqueued TX frames
    /// are promptly sent.
    fn wake_tx_task(&mut self, device: &DeviceId<Self>);

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

#[cfg(any(test, feature = "testutils"))]
pub(crate) mod testutil {
    use super::*;

    use crate::ip::device::config::{
        IpDeviceConfigurationUpdate, Ipv4DeviceConfigurationUpdate, Ipv6DeviceConfigurationUpdate,
    };
    #[cfg(test)]
    use crate::testutil::Ctx;

    #[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
    pub struct FakeWeakDeviceId<D>(pub(crate) D);

    impl<D: PartialEq> PartialEq<D> for FakeWeakDeviceId<D> {
        fn eq(&self, other: &D) -> bool {
            let Self(this) = self;
            this == other
        }
    }

    impl<D: StrongId<Weak = Self>> WeakId for FakeWeakDeviceId<D> {
        type Strong = D;
    }

    impl<D: crate::device::Id> crate::device::Id for FakeWeakDeviceId<D> {
        fn is_loopback(&self) -> bool {
            let Self(inner) = self;
            inner.is_loopback()
        }
    }

    /// A fake device ID for use in testing.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
    pub(crate) struct FakeDeviceId;

    impl StrongId for FakeDeviceId {
        type Weak = FakeWeakDeviceId<Self>;
    }

    impl crate::device::Id for FakeDeviceId {
        fn is_loopback(&self) -> bool {
            false
        }
    }

    #[cfg(test)]
    pub trait FakeStrongDeviceId: StrongId<Weak = FakeWeakDeviceId<Self>> + 'static + Ord {}

    #[cfg(test)]
    impl<D: StrongId<Weak = FakeWeakDeviceId<Self>> + 'static + Ord> FakeStrongDeviceId for D {}

    pub fn enable_device<BC: BindingsContext>(
        ctx: &mut crate::testutil::Ctx<BC>,
        device: &DeviceId<BC>,
    ) {
        let ip_config =
            IpDeviceConfigurationUpdate { ip_enabled: Some(true), ..Default::default() };
        let _: Ipv4DeviceConfigurationUpdate = ctx
            .core_api()
            .device_ip::<Ipv4>()
            .update_configuration(
                device,
                Ipv4DeviceConfigurationUpdate { ip_config, ..Default::default() },
            )
            .unwrap();
        let _: Ipv6DeviceConfigurationUpdate = ctx
            .core_api()
            .device_ip::<Ipv6>()
            .update_configuration(
                device,
                Ipv6DeviceConfigurationUpdate { ip_config, ..Default::default() },
            )
            .unwrap();
    }

    /// Enables or disables IP packet routing on `device`.
    #[cfg(test)]
    #[netstack3_macros::context_ip_bounds(I, BC, crate)]
    pub(crate) fn set_forwarding_enabled<BC: BindingsContext, I: crate::IpExt>(
        ctx: &mut Ctx<BC>,
        device: &DeviceId<BC>,
        enabled: bool,
    ) {
        let _config = ctx
            .core_api()
            .device_ip::<I>()
            .update_configuration(
                device,
                IpDeviceConfigurationUpdate {
                    forwarding_enabled: Some(enabled),
                    ..Default::default()
                }
                .into(),
            )
            .unwrap();
    }

    /// Returns whether IP packet routing is enabled on `device`.
    #[cfg(test)]
    #[netstack3_macros::context_ip_bounds(I, BC, crate)]
    pub(crate) fn is_forwarding_enabled<BC: BindingsContext, I: crate::IpExt>(
        ctx: &mut Ctx<BC>,
        device: &DeviceId<BC>,
    ) -> bool {
        let configuration = ctx.core_api().device_ip::<I>().get_configuration(device);
        let crate::ip::device::state::IpDeviceConfiguration { forwarding_enabled, .. } =
            configuration.as_ref();
        *forwarding_enabled
    }

    /// A device ID type that supports identifying more than one distinct
    /// device.
    #[cfg(test)]
    #[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Ord, PartialOrd)]
    pub(crate) enum MultipleDevicesId {
        A,
        B,
        C,
    }

    #[cfg(test)]
    impl MultipleDevicesId {
        pub(crate) fn all() -> [Self; 3] {
            [Self::A, Self::B, Self::C]
        }
    }

    #[cfg(test)]
    impl crate::device::Id for MultipleDevicesId {
        fn is_loopback(&self) -> bool {
            false
        }
    }

    #[cfg(test)]
    impl StrongId for MultipleDevicesId {
        type Weak = FakeWeakDeviceId<Self>;
    }
}

#[cfg(test)]
mod tests {
    use core::{
        num::{NonZeroU16, NonZeroU8},
        time::Duration,
    };

    use const_unwrap::const_unwrap_option;
    use net_declare::net_mac;
    use net_types::{
        ip::{AddrSubnet, Mtu},
        SpecifiedAddr, UnicastAddr, Witness as _,
    };
    use test_case::test_case;

    use super::*;
    use crate::{
        context::testutil::FakeInstant,
        device::{
            ethernet::{EthernetCreationProperties, MaxEthernetFrameSize},
            loopback::{LoopbackCreationProperties, LoopbackDevice},
            queue::tx::TransmitQueueConfiguration,
            DeviceProvider,
        },
        error, for_any_device_id,
        ip::device::{
            api::AddIpAddrSubnetError,
            config::{
                IpDeviceConfigurationUpdate, Ipv4DeviceConfigurationUpdate,
                Ipv6DeviceConfigurationUpdate,
            },
            slaac::SlaacConfiguration,
            state::{Ipv4AddrConfig, Ipv6AddrManualConfig, Lifetime},
        },
        testutil::{TestIpExt, DEFAULT_INTERFACE_METRIC, IPV6_MIN_IMPLIED_MAX_FRAME_SIZE},
        work_queue::WorkQueueReport,
    };

    #[test]
    fn test_origin_tracker() {
        let tracker = OriginTracker::new();
        if cfg!(debug_assertions) {
            assert_ne!(tracker, OriginTracker::new());
        } else {
            assert_eq!(tracker, OriginTracker::new());
        }
        assert_eq!(tracker.clone(), tracker);
    }

    #[test]
    fn frame_destination_from_dest() {
        const LOCAL_ADDR: Mac = net_mac!("88:88:88:88:88:88");

        assert_eq!(
            FrameDestination::from_dest(
                UnicastAddr::new(net_mac!("00:11:22:33:44:55")).unwrap().get(),
                LOCAL_ADDR
            ),
            FrameDestination::Individual { local: false }
        );
        assert_eq!(
            FrameDestination::from_dest(LOCAL_ADDR, LOCAL_ADDR),
            FrameDestination::Individual { local: true }
        );
        assert_eq!(
            FrameDestination::from_dest(Mac::BROADCAST, LOCAL_ADDR),
            FrameDestination::Broadcast,
        );
        assert_eq!(
            FrameDestination::from_dest(
                MulticastAddr::new(net_mac!("11:11:11:11:11:11")).unwrap().get(),
                LOCAL_ADDR
            ),
            FrameDestination::Multicast
        );
    }

    #[test]
    fn test_no_default_routes() {
        let mut ctx = crate::testutil::FakeCtx::default();
        let _loopback_device: LoopbackDeviceId<_> =
            ctx.core_api().device::<LoopbackDevice>().add_device_with_default_state(
                LoopbackCreationProperties { mtu: Mtu::new(55) },
                DEFAULT_INTERFACE_METRIC,
            );

        assert_eq!(ctx.core_api().routes_any().get_all_routes(), []);
        let _ethernet_device: EthernetDeviceId<_> =
            ctx.core_api().device::<EthernetLinkDevice>().add_device_with_default_state(
                EthernetCreationProperties {
                    mac: UnicastAddr::new(net_mac!("aa:bb:cc:dd:ee:ff")).expect("MAC is unicast"),
                    max_frame_size: MaxEthernetFrameSize::MIN,
                },
                DEFAULT_INTERFACE_METRIC,
            );
        assert_eq!(ctx.core_api().routes_any().get_all_routes(), []);
    }

    #[test]
    fn remove_ethernet_device_disables_timers() {
        let mut ctx = crate::testutil::FakeCtx::default();

        let ethernet_device =
            ctx.core_api().device::<EthernetLinkDevice>().add_device_with_default_state(
                EthernetCreationProperties {
                    mac: UnicastAddr::new(net_mac!("aa:bb:cc:dd:ee:ff")).expect("MAC is unicast"),
                    max_frame_size: MaxEthernetFrameSize::from_mtu(Mtu::new(1500)).unwrap(),
                },
                DEFAULT_INTERFACE_METRIC,
            );

        {
            let device = ethernet_device.clone().into();
            // Enable the device, turning on a bunch of features that install
            // timers.
            let ip_config = IpDeviceConfigurationUpdate {
                ip_enabled: Some(true),
                gmp_enabled: Some(true),
                ..Default::default()
            };
            let _: Ipv4DeviceConfigurationUpdate = ctx
                .core_api()
                .device_ip::<Ipv4>()
                .update_configuration(&device, ip_config.into())
                .unwrap();
            let _: Ipv6DeviceConfigurationUpdate = ctx
                .core_api()
                .device_ip::<Ipv6>()
                .update_configuration(
                    &device,
                    Ipv6DeviceConfigurationUpdate {
                        max_router_solicitations: Some(Some(const_unwrap_option(NonZeroU8::new(
                            2,
                        )))),
                        slaac_config: Some(SlaacConfiguration {
                            enable_stable_addresses: true,
                            ..Default::default()
                        }),
                        ip_config,
                        ..Default::default()
                    },
                )
                .unwrap();
        }

        ctx.core_api().device().remove_device(ethernet_device).into_removed();
        assert_eq!(ctx.bindings_ctx.timer_ctx().timers(), &[]);
    }

    fn add_ethernet(
        ctx: &mut crate::testutil::FakeCtx,
    ) -> DeviceId<crate::testutil::FakeBindingsCtx> {
        ctx.core_api()
            .device::<EthernetLinkDevice>()
            .add_device_with_default_state(
                EthernetCreationProperties {
                    mac: Ipv6::FAKE_CONFIG.local_mac,
                    max_frame_size: IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
                },
                DEFAULT_INTERFACE_METRIC,
            )
            .into()
    }

    fn add_loopback(
        ctx: &mut crate::testutil::FakeCtx,
    ) -> DeviceId<crate::testutil::FakeBindingsCtx> {
        let device = ctx
            .core_api()
            .device::<LoopbackDevice>()
            .add_device_with_default_state(
                LoopbackCreationProperties { mtu: Ipv6::MINIMUM_LINK_MTU },
                DEFAULT_INTERFACE_METRIC,
            )
            .into();
        ctx.core_api()
            .device_ip::<Ipv6>()
            .add_ip_addr_subnet(
                &device,
                AddrSubnet::from_witness(Ipv6::LOOPBACK_ADDRESS, Ipv6::LOOPBACK_SUBNET.prefix())
                    .unwrap(),
            )
            .unwrap();
        device
    }

    fn check_transmitted_ethernet(
        bindings_ctx: &mut crate::testutil::FakeBindingsCtx,
        _device_id: &DeviceId<crate::testutil::FakeBindingsCtx>,
        count: usize,
    ) {
        assert_eq!(bindings_ctx.take_ethernet_frames().len(), count);
    }

    fn check_transmitted_loopback(
        bindings_ctx: &mut crate::testutil::FakeBindingsCtx,
        device_id: &DeviceId<crate::testutil::FakeBindingsCtx>,
        count: usize,
    ) {
        // Loopback frames leave the stack; outgoing frames land in
        // its RX queue.
        let rx_available = core::mem::take(&mut bindings_ctx.state_mut().rx_available);
        if count == 0 {
            assert_eq!(rx_available, <[LoopbackDeviceId::<_>; 0]>::default());
        } else {
            assert_eq!(
                rx_available.into_iter().map(DeviceId::Loopback).collect::<Vec<_>>(),
                [device_id.clone()]
            );
        }
    }

    #[test_case(add_ethernet, check_transmitted_ethernet, true; "ethernet with queue")]
    #[test_case(add_ethernet, check_transmitted_ethernet, false; "ethernet without queue")]
    #[test_case(add_loopback, check_transmitted_loopback, true; "loopback with queue")]
    #[test_case(add_loopback, check_transmitted_loopback, false; "loopback without queue")]
    fn tx_queue(
        add_device: fn(&mut crate::testutil::FakeCtx) -> DeviceId<crate::testutil::FakeBindingsCtx>,
        check_transmitted: fn(
            &mut crate::testutil::FakeBindingsCtx,
            &DeviceId<crate::testutil::FakeBindingsCtx>,
            usize,
        ),
        with_tx_queue: bool,
    ) {
        let mut ctx = crate::testutil::FakeCtx::default();
        let device = add_device(&mut ctx);

        if with_tx_queue {
            for_any_device_id!(DeviceId, DeviceProvider, D, &device, device => {
                    ctx.core_api().transmit_queue::<D>()
                        .set_configuration(device, TransmitQueueConfiguration::Fifo)
            })
        }

        let _: Ipv6DeviceConfigurationUpdate = ctx
            .core_api()
            .device_ip::<Ipv6>()
            .update_configuration(
                &device,
                Ipv6DeviceConfigurationUpdate {
                    // Enable DAD so that the auto-generated address triggers a DAD
                    // message immediately on interface enable.
                    dad_transmits: Some(Some(const_unwrap_option(NonZeroU16::new(1)))),
                    // Enable stable addresses so the link-local address is auto-
                    // generated.
                    slaac_config: Some(SlaacConfiguration {
                        enable_stable_addresses: true,
                        ..Default::default()
                    }),
                    ip_config: IpDeviceConfigurationUpdate {
                        ip_enabled: Some(true),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .unwrap();

        if with_tx_queue {
            check_transmitted(&mut ctx.bindings_ctx, &device, 0);
            assert_eq!(
                core::mem::take(&mut ctx.bindings_ctx.state_mut().tx_available),
                [device.clone()]
            );
            let result = for_any_device_id!(
                DeviceId, DeviceProvider, D, &device, device => {
                    ctx.core_api().transmit_queue::<D>().transmit_queued_frames(device)
                }
            );
            assert_eq!(result, Ok(WorkQueueReport::AllDone));
        }

        check_transmitted(&mut ctx.bindings_ctx, &device, 1);
        assert_eq!(ctx.bindings_ctx.state_mut().tx_available, <[DeviceId::<_>; 0]>::default());
        for_any_device_id!(
            DeviceId,
            device,
            device => ctx.core_api().device().remove_device(device).into_removed()
        )
    }

    #[netstack3_macros::context_ip_bounds(I, crate::testutil::FakeBindingsCtx, crate)]
    fn test_add_remove_ip_addresses<I: Ip + TestIpExt + crate::IpExt>(
        addr_config: Option<I::ManualAddressConfig<FakeInstant>>,
    ) {
        let config = I::FAKE_CONFIG;
        let mut ctx = crate::testutil::FakeCtx::default();
        let device = ctx
            .core_api()
            .device::<EthernetLinkDevice>()
            .add_device_with_default_state(
                EthernetCreationProperties {
                    mac: config.local_mac,
                    max_frame_size: IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
                },
                DEFAULT_INTERFACE_METRIC,
            )
            .into();

        crate::device::testutil::enable_device(&mut ctx, &device);

        let ip = I::get_other_ip_address(1).get();
        let prefix = config.subnet.prefix();
        let addr_subnet = AddrSubnet::new(ip, prefix).unwrap();

        let check_contains_addr = |ctx: &mut crate::testutil::FakeCtx| {
            ctx.core_api()
                .device_ip::<I>()
                .get_assigned_ip_addr_subnets(&device)
                .contains(&addr_subnet)
        };

        // IP doesn't exist initially.
        assert_eq!(check_contains_addr(&mut ctx), false);

        // Add IP (OK).
        ctx.core_api()
            .device_ip::<I>()
            .add_ip_addr_subnet_with_config(&device, addr_subnet, addr_config.unwrap_or_default())
            .unwrap();
        assert_eq!(check_contains_addr(&mut ctx), true);

        // Add IP again (already exists).
        assert_eq!(
            ctx.core_api().device_ip::<I>().add_ip_addr_subnet(&device, addr_subnet),
            Err(AddIpAddrSubnetError::Exists),
        );
        assert_eq!(check_contains_addr(&mut ctx), true);

        // Add IP with different subnet (already exists).
        let wrong_addr_subnet = AddrSubnet::new(ip, prefix - 1).unwrap();
        assert_eq!(
            ctx.core_api().device_ip::<I>().add_ip_addr_subnet(&device, wrong_addr_subnet),
            Err(AddIpAddrSubnetError::Exists),
        );
        assert_eq!(check_contains_addr(&mut ctx), true);

        let ip = SpecifiedAddr::new(ip).unwrap();
        // Del IP (ok).
        let () = ctx.core_api().device_ip::<I>().del_ip_addr(&device, ip).unwrap();
        assert_eq!(check_contains_addr(&mut ctx), false);

        // Del IP again (not found).
        assert_eq!(
            ctx.core_api().device_ip::<I>().del_ip_addr(&device, ip),
            Err(error::NotFoundError),
        );

        assert_eq!(check_contains_addr(&mut ctx), false);
    }

    #[test_case(None; "with no AddressConfig specified")]
    #[test_case(Some(Ipv4AddrConfig {
        valid_until: Lifetime::Finite(FakeInstant::from(Duration::from_secs(1)))
    }); "with AddressConfig specified")]
    fn test_add_remove_ipv4_addresses(addr_config: Option<Ipv4AddrConfig<FakeInstant>>) {
        test_add_remove_ip_addresses::<Ipv4>(addr_config);
    }

    #[test_case(None; "with no AddressConfig specified")]
    #[test_case(Some(Ipv6AddrManualConfig {
        valid_until: Lifetime::Finite(FakeInstant::from(Duration::from_secs(1)))
    }); "with AddressConfig specified")]
    fn test_add_remove_ipv6_addresses(addr_config: Option<Ipv6AddrManualConfig<FakeInstant>>) {
        test_add_remove_ip_addresses::<Ipv6>(addr_config);
    }
}
