// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use alloc::{collections::HashMap, vec::Vec};
use core::{
    convert::Infallible as Never,
    fmt::{Debug, Display},
    marker::PhantomData,
};

use derivative::Derivative;
use lock_order::{lock::UnlockedAccess, Locked};
use net_types::{
    ethernet::Mac,
    ip::{
        AddrSubnetEither, Ip, IpAddr, IpAddress, IpInvariant, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr, Mtu,
    },
    BroadcastAddr, MulticastAddr, SpecifiedAddr, UnicastAddr, Witness as _,
};
use packet::{Buf, BufferMut};
use smallvec::SmallVec;
use tracing::{debug, trace};

use crate::{
    context::{CounterContext, InstantBindingsTypes, InstantContext},
    counters::Counter,
    device::{
        arp::ArpCounters,
        ethernet::{
            self, EthernetDeviceStateBuilder, EthernetLinkDevice, EthernetTimerId,
            MaxEthernetFrameSize,
        },
        id::{
            BaseDeviceId, BasePrimaryDeviceId, DeviceId, EthernetDeviceId, EthernetPrimaryDeviceId,
            StrongId, WeakId,
        },
        integration,
        loopback::{
            self, LoopbackDevice, LoopbackDeviceId, LoopbackDeviceState, LoopbackPrimaryDeviceId,
        },
        queue::{
            rx::ReceiveQueueApi,
            tx::{TransmitQueueApi, TransmitQueueConfiguration},
        },
        socket::{self, HeldSockets},
        state::{BaseDeviceState, DeviceStateSpec, IpLinkDeviceStateInner},
    },
    error::{
        self, ExistsError, NeighborRemovalError, NotSupportedError, SetIpAddressPropertiesError,
        StaticNeighborInsertionError,
    },
    ip::{
        device::{
            nud::{LinkResolutionContext, NeighborStateInspect, NudHandler},
            state::{
                AddrSubnetAndManualConfigEither, AssignedAddress as _, IpDeviceFlags,
                Ipv4DeviceConfigurationAndFlags, Ipv6DeviceConfigurationAndFlags, Lifetime,
            },
            DelIpv6Addr, DualStackDeviceHandler, IpDeviceIpExt, IpDeviceStateContext,
            Ipv4DeviceConfigurationUpdate, Ipv6DeviceConfigurationUpdate,
            PendingIpv4DeviceConfigurationUpdate, PendingIpv6DeviceConfigurationUpdate,
        },
        forwarding::IpForwardingDeviceContext,
        types::RawMetric,
    },
    sync::{PrimaryRc, RwLock},
    trace_duration,
    work_queue::WorkQueueReport,
    BindingsContext, Instant, SyncCtx,
};

/// A device.
///
/// `Device` is used to identify a particular device implementation. It
/// is only intended to exist at the type level, never instantiated at runtime.
pub trait Device: 'static {}

/// Marker type for a generic device.
pub(crate) enum AnyDevice {}

impl Device for AnyDevice {}

/// An execution context which provides device ID types type for various
/// netstack internals to share.
pub(crate) trait DeviceIdContext<D: Device> {
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
    pub(super) device: D,
    pub(super) frame_dst: FrameDestination,
    _marker: PhantomData<I>,
}

impl<D, I: Ip> RecvIpFrameMeta<D, I> {
    pub(super) fn new(device: D, frame_dst: FrameDestination) -> RecvIpFrameMeta<D, I> {
        RecvIpFrameMeta { device, frame_dst, _marker: PhantomData }
    }
}

/// Iterator over devices.
///
/// Implements `Iterator<Item=DeviceId<C>>` by pulling from provided loopback
/// and ethernet device ID iterators. This struct only exists as a named type
/// so it can be an associated type on impls of the [`IpDeviceContext`] trait.
pub(crate) struct DevicesIter<'s, BC: BindingsContext> {
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

impl<I: IpDeviceIpExt, BC: BindingsContext, L> IpForwardingDeviceContext<I>
    for Locked<&SyncCtx<BC>, L>
where
    Self: IpDeviceStateContext<I, BC, DeviceId = DeviceId<BC>>,
{
    fn get_routing_metric(&mut self, device_id: &Self::DeviceId) -> RawMetric {
        match device_id {
            DeviceId::Ethernet(id) => self::ethernet::get_routing_metric(self, id),
            DeviceId::Loopback(id) => self::loopback::get_routing_metric(self, id),
        }
    }

    fn is_ip_device_enabled(&mut self, device_id: &Self::DeviceId) -> bool {
        IpDeviceStateContext::<I, _>::with_ip_device_flags(
            self,
            device_id,
            |IpDeviceFlags { ip_enabled }| *ip_enabled,
        )
    }
}

/// Gets the routing metric for the device.
pub fn get_routing_metric<BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    device_id: &DeviceId<BC>,
) -> RawMetric {
    let mut sync_ctx = Locked::new(core_ctx);
    match device_id {
        DeviceId::Ethernet(id) => ethernet::get_routing_metric(&mut sync_ctx, id),
        DeviceId::Loopback(id) => loopback::get_routing_metric(&mut sync_ctx, id),
    }
}

/// Visitor for NUD state.
pub trait NeighborVisitor<BC: BindingsContext, T: Instant> {
    /// Performs a user-defined operation over an iterator of neighbor state
    /// describing the neighbors associated with a given `device`.
    ///
    /// This function will be called N times, where N is the number of devices
    /// in the stack.
    fn visit_neighbors<LinkAddress: Debug>(
        &self,
        device: DeviceId<BC>,
        neighbors: impl Iterator<Item = NeighborStateInspect<LinkAddress, T>>,
    );
}

/// Creates a snapshot of the devices in the stack at the time of invocation.
///
/// Devices are copied into the return value.
///
/// The argument `filter_map` defines a filtering function, so that unneeded
/// devices are not copied and returned in the snapshot.
pub(crate) fn snapshot_device_ids<T, BC: BindingsContext, F: FnMut(DeviceId<BC>) -> Option<T>>(
    core_ctx: &SyncCtx<BC>,
    filter_map: F,
) -> impl IntoIterator<Item = T> {
    let mut sync_ctx = Locked::new(core_ctx);
    let devices = sync_ctx.read_lock::<crate::lock_ordering::DeviceLayerState>();
    let Devices { ethernet, loopback } = &*devices;
    DevicesIter { ethernet: ethernet.values(), loopback: loopback.iter() }
        .filter_map(filter_map)
        .collect::<SmallVec<[T; 32]>>()
}

/// Provides access to NUD state via a `visitor`.
pub fn inspect_neighbors<BC, V>(core_ctx: &SyncCtx<BC>, visitor: &V)
where
    BC: BindingsContext,
    V: NeighborVisitor<BC, <BC as InstantBindingsTypes>::Instant>,
{
    let device_ids = snapshot_device_ids(core_ctx, |device| match device {
        DeviceId::Ethernet(d) => Some(d),
        // Loopback devices do not have neighbors.
        DeviceId::Loopback(_) => None,
    });
    let mut sync_ctx = Locked::new(core_ctx);
    for device in device_ids {
        let id = device.clone();
        integration::with_ethernet_state(&mut sync_ctx, &id, |mut device_state| {
            let (arp, mut device_state) =
                device_state.lock_and::<crate::lock_ordering::EthernetIpv4Arp>();
            let nud = device_state.lock::<crate::lock_ordering::EthernetIpv6Nud>();
            visitor.visit_neighbors(
                DeviceId::from(device),
                arp.nud.state_iter().chain(nud.state_iter()),
            );
        })
    }
}

/// Visitor for Device state.
pub trait DevicesVisitor<BC: BindingsContext> {
    /// Performs a user-defined operation over an iterator of device state.
    fn visit_devices(&self, devices: impl Iterator<Item = InspectDeviceState<BC>>);
}

/// The state of a Device, for exporting to Inspect.
pub struct InspectDeviceState<BC: BindingsContext> {
    /// A strong ID identifying a Device.
    pub device_id: DeviceId<BC>,

    /// The IP addresses assigned to a Device by core.
    pub addresses: SmallVec<[IpAddr; 32]>,
}

/// Provides access to Device state via a `visitor`.
pub fn inspect_devices<BC: BindingsContext, V: DevicesVisitor<BC>>(
    core_ctx: &SyncCtx<BC>,
    visitor: &V,
) {
    let devices = snapshot_device_ids(core_ctx, Some).into_iter().map(|device| {
        let device_id = device.clone();
        let ip = match &device {
            DeviceId::Ethernet(d) => &d.device_state().ip,
            DeviceId::Loopback(d) => &d.device_state().ip,
        };
        let ipv4 =
            lock_order::lock::RwLockFor::<crate::lock_ordering::IpDeviceAddresses<Ipv4>>::read_lock(
                ip,
            );
        let ipv4_addresses = ipv4.iter().map(|a| IpAddr::from(a.addr().into_addr()));
        let ipv6 =
            lock_order::lock::RwLockFor::<crate::lock_ordering::IpDeviceAddresses<Ipv6>>::read_lock(
                ip,
            );
        let ipv6_addresses = ipv6.iter().map(|a| IpAddr::from(a.addr().into_addr()));
        InspectDeviceState { device_id, addresses: ipv4_addresses.chain(ipv6_addresses).collect() }
    });
    visitor.visit_devices(devices)
}

pub(crate) enum Ipv6DeviceLinkLayerAddr {
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
    C: BindingsContext,
    DeviceLayerTimerId<C>,
    EthernetTimerId<EthernetDeviceId<C>>,
    DeviceLayerTimerId(DeviceLayerTimerIdInner::Ethernet(id)),
    id
);

/// Handle a timer event firing in the device layer.
pub(crate) fn handle_timer<BC: BindingsContext>(
    core_ctx: &mut Locked<&SyncCtx<BC>, crate::lock_ordering::Unlocked>,
    bindings_ctx: &mut BC,
    DeviceLayerTimerId(id): DeviceLayerTimerId<BC>,
) {
    match id {
        DeviceLayerTimerIdInner::Ethernet(id) => ethernet::handle_timer(core_ctx, bindings_ctx, id),
    }
}

// TODO(joshlf): Does the IP layer ever need to distinguish between broadcast
// and multicast frames?

/// The type of address used as the source address in a device-layer frame:
/// unicast or broadcast.
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
    /// Is this `FrameDestination::Multicast`?
    pub(crate) fn is_multicast(self) -> bool {
        self == FrameDestination::Multicast
    }

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
pub(crate) struct Devices<BT: DeviceLayerTypes> {
    pub(super) ethernet: HashMap<EthernetDeviceId<BT>, EthernetPrimaryDeviceId<BT>>,
    pub(super) loopback: Option<LoopbackPrimaryDeviceId<BT>>,
}

/// The state associated with the device layer.
pub(crate) struct DeviceLayerState<BT: DeviceLayerTypes> {
    pub(super) devices: RwLock<Devices<BT>>,
    pub(super) origin: OriginTracker,
    pub(super) shared_sockets: HeldSockets<BT>,
    pub(super) counters: DeviceCounters,
    pub(super) arp_counters: ArpCounters,
}

impl<BT: DeviceLayerTypes> DeviceLayerState<BT> {
    pub(crate) fn counters(&self) -> &DeviceCounters {
        &self.counters
    }

    pub(crate) fn arp_counters(&self) -> &ArpCounters {
        &self.arp_counters
    }
}

/// Device layer counters.
#[derive(Default)]
pub struct DeviceCounters {
    /// Device layer counters for Ethernet devices.
    pub ethernet: EthernetDeviceCounters,
    /// Device layer counters for Loopback devices.
    pub loopback: LoopbackDeviceCounters,
}

/// Device layer counters for Ethernet devices.
#[derive(Default)]
pub struct EthernetDeviceCounters {
    /// Common device layer counters.
    pub common: CommonDeviceCounters,
    /// Count of ip packets sent inside an ethernet frame.
    pub(crate) send_ip_frame: Counter,
    /// Count of ethernet frames that failed to send because there was no Tx queue.
    pub send_no_queue: Counter,
    /// Count of incoming ethernet frames dropped because the destination address was for another device.
    pub recv_other_dest: Counter,
    /// Count of incoming ethernet frames deliverd to the ARP layer.
    pub recv_arp_delivered: Counter,
}

/// Device layer counters for Loopback devices.
#[derive(Default)]
pub struct LoopbackDeviceCounters {
    /// Common device layer counters.
    pub common: CommonDeviceCounters,
    /// Count of incoming loopback frames dropped due to an empty ethertype.
    pub recv_no_ethertype: Counter,
}

/// Device layer counters that are common to both Ethernet and Loopback devices.
#[derive(Default)]
pub struct CommonDeviceCounters {
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
    /// Count of incoming frames deliverd to the IP layer.
    pub recv_ip_delivered: Counter,
    /// Count of incoming frames dropped due to an unsupported ethertype.
    pub recv_unsupported_ethertype: Counter,
}

impl<BC: BindingsContext> UnlockedAccess<crate::lock_ordering::DeviceCounters> for SyncCtx<BC> {
    type Data = DeviceCounters;
    type Guard<'l> = &'l DeviceCounters where Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        self.state.device_counters()
    }
}

impl<BC: BindingsContext, L> CounterContext<DeviceCounters> for Locked<&SyncCtx<BC>, L> {
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
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OriginTracker(#[cfg(debug_assertions)] u64);

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
            arp_counters: Default::default(),
        }
    }

    /// Add a new ethernet device to the device layer.
    ///
    /// `add` adds a new `EthernetDeviceState` with the given MAC address and
    /// maximum frame size. The frame size is the limit on the size of the data
    /// payload and the header but not the FCS.
    pub(crate) fn add_ethernet_device<
        F: FnOnce() -> (BC::EthernetDeviceState, BC::DeviceIdentifier),
    >(
        &self,
        mac: UnicastAddr<Mac>,
        max_frame_size: MaxEthernetFrameSize,
        metric: RawMetric,
        bindings_state: F,
    ) -> EthernetDeviceId<BC> {
        let Devices { ethernet, loopback: _ } = &mut *self.devices.write();

        let (external_state, bindings_id) = bindings_state();
        let primary = EthernetPrimaryDeviceId::new(
            IpLinkDeviceStateInner::new(
                EthernetDeviceStateBuilder::new(mac, max_frame_size, metric).build(),
                self.origin.clone(),
            ),
            external_state,
            bindings_id,
        );
        let id = primary.clone_strong();

        assert!(ethernet.insert(id.clone(), primary).is_none());
        debug!("adding Ethernet device {:?} with MTU {:?}", id, max_frame_size);
        id
    }

    /// Adds a new loopback device to the device layer.
    pub(crate) fn add_loopback_device<
        F: FnOnce() -> (BC::LoopbackDeviceState, BC::DeviceIdentifier),
    >(
        &self,
        mtu: Mtu,
        metric: RawMetric,
        bindings_state: F,
    ) -> Result<LoopbackDeviceId<BC>, ExistsError> {
        let Devices { ethernet: _, loopback } = &mut *self.devices.write();

        if let Some(_) = loopback {
            return Err(ExistsError);
        }

        let (external_state, bindings_id) = bindings_state();
        let primary = LoopbackPrimaryDeviceId::new(
            IpLinkDeviceStateInner::new(LoopbackDeviceState::new(mtu, metric), self.origin.clone()),
            external_state,
            bindings_id,
        );

        let id = primary.clone_strong();

        *loopback = Some(primary);

        debug!("added loopback device");

        Ok(id)
    }
}

/// Provides associated types used in the device layer.
pub trait DeviceLayerStateTypes: InstantContext {
    /// The state associated with loopback devices.
    type LoopbackDeviceState: Send + Sync;

    /// The state associated with ethernet devices.
    type EthernetDeviceState: Send + Sync;

    /// An opaque identifier that is available from both strong and weak device
    /// references.
    type DeviceIdentifier: Send + Sync + Debug + Display;
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

    /// Send a frame to a device driver.
    ///
    /// If there was an MTU error while attempting to serialize the frame, the
    /// original buffer is returned in the `Err` variant. All other errors (for
    /// example, errors in allocating a buffer) are silently ignored and
    /// reported as success. Implementations are expected to gracefully handle
    /// non-conformant but correctable input, e.g. by padding too-small frames.
    fn send_frame(
        &mut self,
        device: &EthernetDeviceId<Self>,
        frame: Buf<Vec<u8>>,
    ) -> Result<(), DeviceSendFrameError<Buf<Vec<u8>>>>;
}

/// An error encountered when sending a frame.
#[derive(Debug, PartialEq, Eq)]
pub enum DeviceSendFrameError<T> {
    /// The device is not ready to send frames.
    DeviceNotReady(T),
}

/// Sets the TX queue configuration for a device.
pub fn set_tx_queue_configuration<BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    device: &DeviceId<BC>,
    config: TransmitQueueConfiguration,
) {
    let sync_ctx = &mut Locked::new(core_ctx);
    match device {
        DeviceId::Ethernet(id) => TransmitQueueApi::<_, _, EthernetLinkDevice>::set_configuration(
            sync_ctx,
            bindings_ctx,
            id,
            config,
        ),
        DeviceId::Loopback(id) => TransmitQueueApi::<_, _, LoopbackDevice>::set_configuration(
            sync_ctx,
            bindings_ctx,
            id,
            config,
        ),
    }
}

/// Does the work of transmitting frames for a device.
pub fn transmit_queued_tx_frames<BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    device: &DeviceId<BC>,
) -> Result<WorkQueueReport, DeviceSendFrameError<()>> {
    let sync_ctx = &mut Locked::new(core_ctx);
    match device {
        DeviceId::Ethernet(id) => {
            TransmitQueueApi::<_, _, EthernetLinkDevice>::transmit_queued_frames(
                sync_ctx,
                bindings_ctx,
                id,
            )
        }
        DeviceId::Loopback(id) => TransmitQueueApi::<_, _, LoopbackDevice>::transmit_queued_frames(
            sync_ctx,
            bindings_ctx,
            id,
        ),
    }
}

/// Handle a batch of queued RX packets for the device.
///
/// If packets remain in the RX queue after a batch of RX packets has been
/// handled, the RX task will be scheduled to run again so the next batch of
/// RX packets may be handled. See [`DeviceLayerEventDispatcher::wake_rx_task`]
/// for more details.
pub fn handle_queued_rx_packets<BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    device: &LoopbackDeviceId<BC>,
) -> WorkQueueReport {
    ReceiveQueueApi::<_, _, LoopbackDevice>::handle_queued_rx_frames(
        &mut Locked::new(core_ctx),
        bindings_ctx,
        device,
    )
}

/// The result of removing a device from core.
#[derive(Debug)]
pub enum RemoveDeviceResult<R, D> {
    /// The device was synchronously removed and no more references to it exist.
    Removed(R),
    /// The device was marked for destruction but there are still references to
    /// it in existence. The provided receiver can be polled on to observe
    /// device destruction completion.
    Deferred(D),
}

impl<R> RemoveDeviceResult<R, Never> {
    /// A helper function to unwrap a [`RemovedDeviceResult`] that can never be
    /// [`RemovedDeviceResult::Deferred`].
    pub fn into_removed(self) -> R {
        match self {
            Self::Removed(r) => r,
            Self::Deferred(never) => match never {},
        }
    }
}

/// An alias for [`RemoveDeviceResult`] that extracts the receiver type from the
/// NonSyncContext.
pub type RemoveDeviceResultWithContext<S, BT> =
    RemoveDeviceResult<S, <BT as crate::ReferenceNotifiers>::ReferenceReceiver<S>>;

fn remove_device<T: DeviceStateSpec, BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    device: BaseDeviceId<T, BC>,
    remove: impl FnOnce(
        &mut Devices<BC>,
        BaseDeviceId<T, BC>,
    ) -> (BasePrimaryDeviceId<T, BC>, BaseDeviceId<T, BC>),
) -> RemoveDeviceResultWithContext<T::External<BC>, BC>
where
    BaseDeviceId<T, BC>: Into<DeviceId<BC>>,
{
    // Start cleaning up the device by disabling IP state. This removes timers
    // for the device that would otherwise hold references to defunct device
    // state.
    let debug_references = {
        let mut sync_ctx = Locked::new(core_ctx);

        let device = device.clone().into();

        crate::ip::device::clear_ipv4_device_state(&mut sync_ctx, bindings_ctx, &device);
        crate::ip::device::clear_ipv6_device_state(&mut sync_ctx, bindings_ctx, &device);
        device.downgrade().debug_references()
    };

    tracing::debug!("removing {device:?}");
    let (primary, strong) = {
        let mut devices = core_ctx.state.device.devices.write();
        remove(&mut *devices, device)
    };
    assert_eq!(strong, primary);
    core::mem::drop(strong);
    match PrimaryRc::unwrap_or_notify_with(primary.into_inner(), || {
        let (notifier, receiver) =
            BC::new_reference_notifier::<T::External<BC>, _>(debug_references);
        let notifier = crate::sync::MapRcNotifier::new(notifier, |state: BaseDeviceState<_, _>| {
            state.external_state
        });
        (notifier, receiver)
    }) {
        Ok(s) => RemoveDeviceResult::Removed(s.external_state),
        Err(receiver) => RemoveDeviceResult::Deferred(receiver),
    }
}

/// Removes an Ethernet device from the device layer.
///
/// # Panics
///
/// Panics if the caller holds strong device IDs for `device`.
pub fn remove_ethernet_device<BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    device: EthernetDeviceId<BC>,
) -> RemoveDeviceResultWithContext<BC::EthernetDeviceState, BC> {
    remove_device(core_ctx, bindings_ctx, device, |devices, id| {
        let removed = devices
            .ethernet
            .remove(&id)
            .unwrap_or_else(|| panic!("no such Ethernet device: {id:?}"));

        (removed, id)
    })
}

/// Removes the Loopback device from the device layer.
///
/// # Panics
///
/// Panics if the caller holds strong device IDs for `device`.
pub fn remove_loopback_device<BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    device: LoopbackDeviceId<BC>,
) -> RemoveDeviceResultWithContext<BC::LoopbackDeviceState, BC> {
    remove_device(core_ctx, bindings_ctx, device, |devices, id| {
        let removed = devices.loopback.take().expect("loopback device not installed");
        (removed, id)
    })
}

/// Adds a new Ethernet device to the stack.
pub fn add_ethernet_device_with_state<
    BC: BindingsContext,
    F: FnOnce() -> (BC::EthernetDeviceState, BC::DeviceIdentifier),
>(
    core_ctx: &SyncCtx<BC>,
    mac: UnicastAddr<Mac>,
    max_frame_size: MaxEthernetFrameSize,
    metric: RawMetric,
    bindings_state: F,
) -> EthernetDeviceId<BC> {
    core_ctx.state.device.add_ethernet_device(mac, max_frame_size, metric, bindings_state)
}

/// Adds a new Ethernet device to the stack.
#[cfg(any(test, feature = "testutils"))]
pub(crate) fn add_ethernet_device<BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    mac: UnicastAddr<Mac>,
    max_frame_size: MaxEthernetFrameSize,
    metric: RawMetric,
) -> EthernetDeviceId<BC>
where
    BC::EthernetDeviceState: Default,
    BC::DeviceIdentifier: Default,
{
    add_ethernet_device_with_state(core_ctx, mac, max_frame_size, metric, Default::default)
}

/// Adds a new loopback device to the stack.
///
/// Adds a new loopback device to the stack. Only one loopback device may be
/// installed at any point in time, so if there is one already, an error is
/// returned.
pub fn add_loopback_device_with_state<
    BC: BindingsContext,
    F: FnOnce() -> (BC::LoopbackDeviceState, BC::DeviceIdentifier),
>(
    core_ctx: &SyncCtx<BC>,
    mtu: Mtu,
    metric: RawMetric,
    bindings_state: F,
) -> Result<LoopbackDeviceId<BC>, crate::error::ExistsError> {
    core_ctx.state.device.add_loopback_device(mtu, metric, bindings_state)
}

/// Adds a new loopback device to the stack.
///
/// Adds a new loopback device to the stack. Only one loopback device may be
/// installed at any point in time, so if there is one already, an error is
/// returned.
#[cfg(test)]
pub(crate) fn add_loopback_device<BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    mtu: Mtu,
    metric: RawMetric,
) -> Result<LoopbackDeviceId<BC>, crate::error::ExistsError>
where
    BC::LoopbackDeviceState: Default,
    BC::DeviceIdentifier: Default,
{
    add_loopback_device_with_state(core_ctx, mtu, metric, Default::default)
}

/// Receive a device layer frame from the network.
pub fn receive_frame<B: BufferMut, BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    device: &EthernetDeviceId<BC>,
    buffer: B,
) {
    trace_duration!(bindings_ctx, "device::receive_frame");
    core_ctx.state.device_counters().ethernet.common.recv_frame.increment();
    self::ethernet::receive_frame(&mut Locked::new(core_ctx), bindings_ctx, device, buffer)
}

/// Set the promiscuous mode flag on `device`.
// TODO(rheacock): remove `allow(dead_code)` when this is used.
#[allow(dead_code)]
pub(crate) fn set_promiscuous_mode<BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    device: &DeviceId<BC>,
    enabled: bool,
) -> Result<(), NotSupportedError> {
    match device {
        DeviceId::Ethernet(id) => Ok(self::ethernet::set_promiscuous_mode(
            &mut Locked::new(core_ctx),
            bindings_ctx,
            id,
            enabled,
        )),
        DeviceId::Loopback(LoopbackDeviceId { .. }) => Err(NotSupportedError),
    }
}

/// Get all IPv4 and IPv6 address/subnet pairs configured on a device
pub fn get_all_ip_addr_subnets<BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    device: &DeviceId<BC>,
) -> Vec<AddrSubnetEither> {
    DualStackDeviceHandler::get_all_ip_addr_subnets(&mut Locked::new(core_ctx), device)
}

/// Adds an IP address and associated subnet to this device.
///
/// For IPv6, this function also joins the solicited-node multicast group and
/// begins performing Duplicate Address Detection (DAD).
pub fn add_ip_addr_subnet<BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    device: &DeviceId<BC>,
    addr_sub_and_config: impl Into<AddrSubnetAndManualConfigEither<BC::Instant>>,
) -> Result<(), ExistsError> {
    let addr_sub_and_config = addr_sub_and_config.into();
    trace!(
        "add_ip_addr_subnet: adding addr_sub_and_config {:?} to device {:?}",
        addr_sub_and_config,
        device
    );
    let mut sync_ctx = Locked::new(core_ctx);

    match addr_sub_and_config {
        AddrSubnetAndManualConfigEither::V4(addr_sub, config) => {
            crate::ip::device::add_ipv4_addr_subnet(
                &mut sync_ctx,
                bindings_ctx,
                device,
                addr_sub,
                config,
            )
        }
        AddrSubnetAndManualConfigEither::V6(addr_sub, config) => {
            crate::ip::device::add_ipv6_addr_subnet(
                &mut sync_ctx,
                bindings_ctx,
                device,
                addr_sub,
                config,
            )
        }
    }
}

/// Sets properties on an IP address.
pub fn set_ip_addr_properties<BC: BindingsContext, A: IpAddress>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    device: &DeviceId<BC>,
    address: SpecifiedAddr<A>,
    next_valid_until: Lifetime<BC::Instant>,
) -> Result<(), SetIpAddressPropertiesError> {
    trace!(
        "set_ip_addr_properties: setting valid_until={:?} for addr={}",
        next_valid_until,
        address
    );
    let mut sync_ctx = Locked::new(core_ctx);

    match address.into() {
        IpAddr::V4(address) => crate::ip::device::set_ipv4_addr_properties(
            &mut sync_ctx,
            bindings_ctx,
            device,
            address,
            next_valid_until,
        ),
        IpAddr::V6(address) => crate::ip::device::set_ipv6_addr_properties(
            &mut sync_ctx,
            bindings_ctx,
            device,
            address,
            next_valid_until,
        ),
    }
}

/// Delete an IP address on a device.
pub fn del_ip_addr<BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    device: &DeviceId<BC>,
    addr: impl Into<SpecifiedAddr<IpAddr>>,
) -> Result<(), error::NotFoundError> {
    let mut sync_ctx = Locked::new(core_ctx);
    let addr = addr.into();
    trace!("del_ip_addr: removing addr {:?} from device {:?}", addr, device);
    match addr.into() {
        IpAddr::V4(addr) => {
            crate::ip::device::del_ipv4_addr(&mut sync_ctx, bindings_ctx, &device, &addr)
        }
        IpAddr::V6(addr) => crate::ip::device::del_ipv6_addr_with_reason(
            &mut sync_ctx,
            bindings_ctx,
            &device,
            DelIpv6Addr::SpecifiedAddr(addr),
            crate::ip::device::state::DelIpv6AddrReason::ManualAction,
        ),
    }
}

// TODO(https://fxbug.dev/134098): Use NeighborAddr to witness these properties.
fn validate_ipv4_neighbor_addr(addr: Ipv4Addr) -> Option<SpecifiedAddr<Ipv4Addr>> {
    (!Ipv4::LOOPBACK_SUBNET.contains(&addr)
        && !Ipv4::MULTICAST_SUBNET.contains(&addr)
        && addr != Ipv4::LIMITED_BROADCAST_ADDRESS.get())
    .then_some(())
    .and_then(|()| SpecifiedAddr::new(addr))
}

// TODO(https://fxbug.dev/134098): Use NeighborAddr to witness these properties.
fn validate_ipv6_neighbor_addr(addr: Ipv6Addr) -> Option<UnicastAddr<Ipv6Addr>> {
    (addr != Ipv6::LOOPBACK_ADDRESS.get() && addr.to_ipv4_mapped().is_none())
        .then_some(())
        .and_then(|()| UnicastAddr::new(addr))
}

/// Inserts a static neighbor entry for a neighbor.
pub fn insert_static_neighbor_entry<I: Ip, BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    device: &DeviceId<BC>,
    addr: I::Addr,
    mac: Mac,
) -> Result<(), StaticNeighborInsertionError> {
    let mac = UnicastAddr::new(mac).ok_or(StaticNeighborInsertionError::MacAddressNotUnicast)?;
    let IpInvariant(result) = I::map_ip(
        (IpInvariant((core_ctx, bindings_ctx, device, mac)), addr),
        |(IpInvariant((sync_ctx, ctx, device, mac)), addr)| {
            IpInvariant(
                validate_ipv4_neighbor_addr(addr)
                    .ok_or(StaticNeighborInsertionError::IpAddressInvalid)
                    .and_then(|addr| {
                        insert_static_arp_table_entry(sync_ctx, ctx, device, addr, mac)
                            .map_err(StaticNeighborInsertionError::NotSupported)
                    }),
            )
        },
        |(IpInvariant((sync_ctx, ctx, device, mac)), addr)| {
            IpInvariant(
                validate_ipv6_neighbor_addr(addr)
                    .ok_or(StaticNeighborInsertionError::IpAddressInvalid)
                    .and_then(|addr| {
                        insert_static_ndp_table_entry(sync_ctx, ctx, device, addr, mac)
                            .map_err(StaticNeighborInsertionError::NotSupported)
                    }),
            )
        },
    );
    result
}

/// Insert a static entry into this device's ARP table.
///
/// This will cause any conflicting dynamic entry to be removed, and
/// any future conflicting gratuitous ARPs to be ignored.
pub(crate) fn insert_static_arp_table_entry<BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    device: &DeviceId<BC>,
    // TODO(https://fxbug.dev/134098): Use NeighborAddr when available.
    addr: SpecifiedAddr<Ipv4Addr>,
    mac: UnicastAddr<Mac>,
) -> Result<(), NotSupportedError> {
    match device {
        DeviceId::Ethernet(id) => Ok(self::ethernet::insert_static_arp_table_entry(
            &mut Locked::new(core_ctx),
            bindings_ctx,
            id,
            addr,
            mac,
        )),
        DeviceId::Loopback(LoopbackDeviceId { .. }) => Err(NotSupportedError),
    }
}

/// Insert a static entry into this device's NDP table.
///
/// This will cause any conflicting dynamic entry to be removed, and NDP
/// messages about `addr` to be ignored.
pub(crate) fn insert_static_ndp_table_entry<BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    device: &DeviceId<BC>,
    // TODO(https://fxbug.dev/134098): Use NeighborAddr when available.
    addr: UnicastAddr<Ipv6Addr>,
    mac: UnicastAddr<Mac>,
) -> Result<(), NotSupportedError> {
    match device {
        DeviceId::Ethernet(id) => Ok(self::ethernet::insert_static_ndp_table_entry(
            &mut Locked::new(core_ctx),
            bindings_ctx,
            id,
            addr,
            mac,
        )),
        DeviceId::Loopback(LoopbackDeviceId { .. }) => Err(NotSupportedError),
    }
}

/// Remove a static or dynamic neighbor table entry.
pub fn remove_neighbor_table_entry<I: Ip, BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    device: &DeviceId<BC>,
    addr: I::Addr,
) -> Result<(), NeighborRemovalError> {
    let device = match device {
        DeviceId::Ethernet(device) => device,
        DeviceId::Loopback(LoopbackDeviceId { .. }) => return Err(NotSupportedError.into()),
    };
    let IpInvariant(result) = I::map_ip(
        (IpInvariant((core_ctx, bindings_ctx, device)), addr),
        |(IpInvariant((sync_ctx, ctx, device)), addr)| {
            IpInvariant(
                validate_ipv4_neighbor_addr(addr)
                    .ok_or(NeighborRemovalError::IpAddressInvalid)
                    .and_then(|addr| {
                        NudHandler::<Ipv4, EthernetLinkDevice, _>::delete_neighbor(
                            &mut Locked::new(sync_ctx),
                            ctx,
                            device,
                            addr,
                        )
                        .map_err(Into::into)
                    }),
            )
        },
        |(IpInvariant((sync_ctx, ctx, device)), addr)| {
            IpInvariant(
                validate_ipv6_neighbor_addr(addr)
                    .map(UnicastAddr::into_specified)
                    .ok_or(NeighborRemovalError::IpAddressInvalid)
                    .and_then(|addr| {
                        NudHandler::<Ipv6, EthernetLinkDevice, _>::delete_neighbor(
                            &mut Locked::new(sync_ctx),
                            ctx,
                            device,
                            addr,
                        )
                        .map_err(Into::into)
                    }),
            )
        },
    );
    result
}

/// Flush neighbor table entries.
pub fn flush_neighbor_table<I: Ip, BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    device: &DeviceId<BC>,
) -> Result<(), NotSupportedError> {
    let device = match device {
        DeviceId::Ethernet(device) => device,
        DeviceId::Loopback(LoopbackDeviceId { .. }) => return Err(NotSupportedError),
    };
    let IpInvariant(()) = I::map_ip(
        IpInvariant((core_ctx, bindings_ctx)),
        |IpInvariant((sync_ctx, ctx))| {
            NudHandler::<Ipv4, EthernetLinkDevice, _>::flush(
                &mut Locked::new(sync_ctx),
                ctx,
                device,
            );
            IpInvariant(())
        },
        |IpInvariant((sync_ctx, ctx))| {
            NudHandler::<Ipv6, EthernetLinkDevice, _>::flush(
                &mut Locked::new(sync_ctx),
                ctx,
                device,
            );
            IpInvariant(())
        },
    );
    Ok(())
}

/// Gets the IPv4 configuration and flags for a `device`.
pub fn get_ipv4_configuration_and_flags<BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    device: &DeviceId<BC>,
) -> Ipv4DeviceConfigurationAndFlags {
    crate::ip::device::get_ipv4_configuration_and_flags(&mut Locked::new(core_ctx), device)
}

/// Gets the IPv6 configuration and flags for a `device`.
pub fn get_ipv6_configuration_and_flags<BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    device: &DeviceId<BC>,
) -> Ipv6DeviceConfigurationAndFlags {
    crate::ip::device::get_ipv6_configuration_and_flags(&mut Locked::new(core_ctx), device)
}

/// Updates the IPv4 configuration for a device.
///
/// Each field in [`Ipv4DeviceConfigurationUpdate`] represents an optionally
/// updateable configuration. If the field has a `Some(_)` value, then an
/// attempt will be made to update that configuration on the device. A `None`
/// value indicates that an update for the configuration is not requested.
///
/// Note that some fields have the type `Option<Option<T>>`. In this case, as
/// long as the outer `Option` is `Some`, then an attempt will be made to update
/// the configuration.
///
/// This function returns a [`PendingIpv4DeviceConfigurationUpdate`] which is validated
/// and its `apply` method can be called to apply the configuration.
pub fn new_ipv4_configuration_update<'a, BC: BindingsContext>(
    device: &'a DeviceId<BC>,
    config: Ipv4DeviceConfigurationUpdate,
) -> Result<
    PendingIpv4DeviceConfigurationUpdate<'a, DeviceId<BC>>,
    crate::ip::UpdateIpConfigurationError,
> {
    PendingIpv4DeviceConfigurationUpdate::new(device, config)
}

/// Updates the IPv6 configuration for a device.
///
/// Each field in [`Ipv6DeviceConfigurationUpdate`] represents an optionally
/// updateable configuration. If the field has a `Some(_)` value, then an
/// attempt will be made to update that configuration on the device. A `None`
/// value indicates that an update for the configuration is not requested.
///
/// Note that some fields have the type `Option<Option<T>>`. In this case,
/// as long as the outer `Option` is `Some`, then an attempt will be made to
/// update the configuration.
///
/// This function returns a [`PendingIpv6DeviceConfigurationUpdate`] which is validated
/// and its `apply` method can be called to apply the configuration.
pub fn new_ipv6_configuration_update<'a, BC: BindingsContext>(
    device: &'a DeviceId<BC>,
    config: Ipv6DeviceConfigurationUpdate,
) -> Result<
    PendingIpv6DeviceConfigurationUpdate<'a, DeviceId<BC>>,
    crate::ip::UpdateIpConfigurationError,
> {
    PendingIpv6DeviceConfigurationUpdate::new(device, config)
}

#[cfg(any(test, feature = "testutils"))]
pub(crate) mod testutil {
    use super::*;

    #[cfg(test)]
    use net_types::ip::IpVersion;

    use crate::ip::device::IpDeviceConfigurationUpdate;
    #[cfg(test)]
    use crate::testutil::Ctx;

    #[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
    pub(crate) struct FakeWeakDeviceId<D>(pub(crate) D);

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

    pub(crate) trait FakeStrongDeviceId:
        StrongId<Weak = FakeWeakDeviceId<Self>> + 'static + Ord
    {
    }

    impl<D: StrongId<Weak = FakeWeakDeviceId<Self>> + 'static + Ord> FakeStrongDeviceId for D {}

    /// Calls [`receive_frame`], with a [`Ctx`].
    #[cfg(test)]
    pub(crate) fn receive_frame<B: BufferMut, BC: BindingsContext>(
        Ctx { sync_ctx, non_sync_ctx }: &mut Ctx<BC>,
        device: EthernetDeviceId<BC>,
        buffer: B,
    ) {
        crate::device::receive_frame(sync_ctx, non_sync_ctx, &device, buffer)
    }

    pub fn enable_device<BC: BindingsContext>(
        core_ctx: &SyncCtx<BC>,
        bindings_ctx: &mut BC,
        device: &DeviceId<BC>,
    ) {
        let ip_config =
            Some(IpDeviceConfigurationUpdate { ip_enabled: Some(true), ..Default::default() });
        let _: Ipv4DeviceConfigurationUpdate = crate::device::testutil::update_ipv4_configuration(
            core_ctx,
            bindings_ctx,
            device,
            Ipv4DeviceConfigurationUpdate { ip_config, ..Default::default() },
        )
        .unwrap();
        let _: Ipv6DeviceConfigurationUpdate = crate::device::testutil::update_ipv6_configuration(
            core_ctx,
            bindings_ctx,
            device,
            Ipv6DeviceConfigurationUpdate { ip_config, ..Default::default() },
        )
        .unwrap();
    }

    /// Enables or disables IP packet routing on `device`.
    #[cfg(test)]
    pub(crate) fn set_forwarding_enabled<BC: BindingsContext, I: Ip>(
        core_ctx: &SyncCtx<BC>,
        bindings_ctx: &mut BC,
        device: &DeviceId<BC>,
        enabled: bool,
    ) -> Result<(), NotSupportedError> {
        let ip_config = Some(IpDeviceConfigurationUpdate {
            forwarding_enabled: Some(enabled),
            ..Default::default()
        });
        match I::VERSION {
            IpVersion::V4 => {
                let _: Ipv4DeviceConfigurationUpdate =
                    crate::device::testutil::update_ipv4_configuration(
                        core_ctx,
                        bindings_ctx,
                        device,
                        Ipv4DeviceConfigurationUpdate { ip_config, ..Default::default() },
                    )
                    .unwrap();
            }
            IpVersion::V6 => {
                let _: Ipv6DeviceConfigurationUpdate =
                    crate::device::testutil::update_ipv6_configuration(
                        core_ctx,
                        bindings_ctx,
                        device,
                        Ipv6DeviceConfigurationUpdate { ip_config, ..Default::default() },
                    )
                    .unwrap();
            }
        }

        Ok(())
    }

    /// Returns whether IP packet routing is enabled on `device`.
    #[cfg(test)]
    pub(crate) fn is_forwarding_enabled<BC: BindingsContext, I: Ip>(
        core_ctx: &SyncCtx<BC>,
        device: &DeviceId<BC>,
    ) -> bool {
        let mut sync_ctx = Locked::new(core_ctx);
        match I::VERSION {
            IpVersion::V4 => {
                crate::ip::device::is_ip_forwarding_enabled::<Ipv4, _, _>(&mut sync_ctx, device)
            }
            IpVersion::V6 => {
                crate::ip::device::is_ip_forwarding_enabled::<Ipv6, _, _>(&mut sync_ctx, device)
            }
        }
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

    /// A shortcut to update IPv4 configuration in a single call.
    pub fn update_ipv4_configuration<BC: crate::BindingsContext>(
        core_ctx: &SyncCtx<BC>,
        bindings_ctx: &mut BC,
        device: &DeviceId<BC>,
        config: Ipv4DeviceConfigurationUpdate,
    ) -> Result<Ipv4DeviceConfigurationUpdate, crate::ip::UpdateIpConfigurationError> {
        let pending = crate::device::new_ipv4_configuration_update(device, config)?;
        Ok(pending.apply_inner(&mut lock_order::Locked::new(core_ctx), bindings_ctx))
    }

    /// A shortcut to update IPv6 configuration in a single call.
    pub fn update_ipv6_configuration<BC: crate::BindingsContext>(
        core_ctx: &SyncCtx<BC>,
        bindings_ctx: &mut BC,
        device: &DeviceId<BC>,
        config: Ipv6DeviceConfigurationUpdate,
    ) -> Result<Ipv6DeviceConfigurationUpdate, crate::ip::UpdateIpConfigurationError> {
        let pending = crate::device::new_ipv6_configuration_update(device, config)?;
        Ok(pending.apply_inner(&mut lock_order::Locked::new(core_ctx), bindings_ctx))
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;
    use core::{num::NonZeroU8, time::Duration};

    use const_unwrap::const_unwrap_option;
    use net_declare::net_mac;
    use net_types::ip::{AddrSubnet, AddrSubnetEither};
    use test_case::test_case;

    use super::*;
    use crate::{
        context::testutil::FakeInstant,
        error,
        ip::device::{
            slaac::SlaacConfiguration,
            state::{Ipv4AddrConfig, Ipv6AddrManualConfig, Lifetime},
            IpDeviceConfigurationUpdate,
        },
        testutil::{Ctx, TestIpExt, DEFAULT_INTERFACE_METRIC, IPV6_MIN_IMPLIED_MAX_FRAME_SIZE},
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
        let Ctx { sync_ctx, non_sync_ctx: _ } = crate::testutil::FakeCtx::default();

        let _loopback_device: LoopbackDeviceId<_> =
            crate::device::add_loopback_device(&sync_ctx, Mtu::new(55), DEFAULT_INTERFACE_METRIC)
                .expect("error adding loopback device");

        assert_eq!(crate::ip::get_all_routes(&sync_ctx), []);
        let _ethernet_device: EthernetDeviceId<_> = crate::device::add_ethernet_device(
            &sync_ctx,
            UnicastAddr::new(net_mac!("aa:bb:cc:dd:ee:ff")).expect("MAC is unicast"),
            MaxEthernetFrameSize::MIN,
            DEFAULT_INTERFACE_METRIC,
        );
        assert_eq!(crate::ip::get_all_routes(&sync_ctx), []);
    }

    #[test]
    fn remove_ethernet_device_disables_timers() {
        let Ctx { sync_ctx, mut non_sync_ctx } = crate::testutil::FakeCtx::default();

        let ethernet_device = crate::device::add_ethernet_device(
            &sync_ctx,
            UnicastAddr::new(net_mac!("aa:bb:cc:dd:ee:ff")).expect("MAC is unicast"),
            MaxEthernetFrameSize::from_mtu(Mtu::new(1500)).unwrap(),
            DEFAULT_INTERFACE_METRIC,
        );

        {
            let device = ethernet_device.clone().into();
            // Enable the device, turning on a bunch of features that install
            // timers.
            let ip_config = Some(IpDeviceConfigurationUpdate {
                ip_enabled: Some(true),
                gmp_enabled: Some(true),
                ..Default::default()
            });
            let _: Ipv4DeviceConfigurationUpdate =
                crate::device::testutil::update_ipv4_configuration(
                    &sync_ctx,
                    &mut non_sync_ctx,
                    &device,
                    Ipv4DeviceConfigurationUpdate { ip_config, ..Default::default() },
                )
                .unwrap();
            let _: Ipv6DeviceConfigurationUpdate =
                crate::device::testutil::update_ipv6_configuration(
                    &sync_ctx,
                    &mut non_sync_ctx,
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

        crate::device::remove_ethernet_device(&sync_ctx, &mut non_sync_ctx, ethernet_device)
            .into_removed();
        assert_eq!(non_sync_ctx.timer_ctx().timers(), &[]);
    }

    fn add_ethernet(
        core_ctx: &mut &crate::testutil::FakeSyncCtx,
        _bindings_ctx: &mut crate::testutil::FakeNonSyncCtx,
    ) -> DeviceId<crate::testutil::FakeNonSyncCtx> {
        crate::device::add_ethernet_device(
            core_ctx,
            Ipv6::FAKE_CONFIG.local_mac,
            IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
            DEFAULT_INTERFACE_METRIC,
        )
        .into()
    }

    fn add_loopback(
        core_ctx: &mut &crate::testutil::FakeSyncCtx,
        bindings_ctx: &mut crate::testutil::FakeNonSyncCtx,
    ) -> DeviceId<crate::testutil::FakeNonSyncCtx> {
        let device = crate::device::add_loopback_device(
            core_ctx,
            Ipv6::MINIMUM_LINK_MTU,
            DEFAULT_INTERFACE_METRIC,
        )
        .unwrap()
        .into();
        crate::device::add_ip_addr_subnet(
            core_ctx,
            bindings_ctx,
            &device,
            AddrSubnet::from_witness(Ipv6::LOOPBACK_ADDRESS, Ipv6::LOOPBACK_SUBNET.prefix())
                .unwrap(),
        )
        .unwrap();
        device
    }

    fn check_transmitted_ethernet(
        bindings_ctx: &mut crate::testutil::FakeNonSyncCtx,
        _device_id: &DeviceId<crate::testutil::FakeNonSyncCtx>,
        count: usize,
    ) {
        assert_eq!(bindings_ctx.frames_sent().len(), count);
    }

    fn check_transmitted_loopback(
        bindings_ctx: &mut crate::testutil::FakeNonSyncCtx,
        device_id: &DeviceId<crate::testutil::FakeNonSyncCtx>,
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
        add_device: fn(
            &mut &crate::testutil::FakeSyncCtx,
            &mut crate::testutil::FakeNonSyncCtx,
        ) -> DeviceId<crate::testutil::FakeNonSyncCtx>,
        check_transmitted: fn(
            &mut crate::testutil::FakeNonSyncCtx,
            &DeviceId<crate::testutil::FakeNonSyncCtx>,
            usize,
        ),
        with_tx_queue: bool,
    ) {
        let Ctx { sync_ctx, mut non_sync_ctx } = crate::testutil::FakeCtx::default();
        let mut sync_ctx = &sync_ctx;
        let device = add_device(&mut sync_ctx, &mut non_sync_ctx);

        if with_tx_queue {
            crate::device::set_tx_queue_configuration(
                &sync_ctx,
                &mut non_sync_ctx,
                &device,
                TransmitQueueConfiguration::Fifo,
            );
        }

        let _: Ipv6DeviceConfigurationUpdate = crate::device::testutil::update_ipv6_configuration(
            &sync_ctx,
            &mut non_sync_ctx,
            &device,
            Ipv6DeviceConfigurationUpdate {
                // Enable DAD so that the auto-generated address triggers a DAD
                // message immediately on interface enable.
                dad_transmits: Some(Some(const_unwrap_option(NonZeroU8::new(1)))),
                // Enable stable addresses so the link-local address is auto-
                // generated.
                slaac_config: Some(SlaacConfiguration {
                    enable_stable_addresses: true,
                    ..Default::default()
                }),
                ip_config: Some(IpDeviceConfigurationUpdate {
                    ip_enabled: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .unwrap();

        if with_tx_queue {
            check_transmitted(&mut non_sync_ctx, &device, 0);
            assert_eq!(
                core::mem::take(&mut non_sync_ctx.state_mut().tx_available),
                [device.clone()]
            );
            assert_eq!(
                crate::device::transmit_queued_tx_frames(&sync_ctx, &mut non_sync_ctx, &device),
                Ok(WorkQueueReport::AllDone)
            );
        }

        check_transmitted(&mut non_sync_ctx, &device, 1);
        assert_eq!(non_sync_ctx.state_mut().tx_available, <[DeviceId::<_>; 0]>::default());
    }

    fn test_add_remove_ip_addresses<I: Ip + TestIpExt>(
        addr_config: Option<I::ManualAddressConfig<FakeInstant>>,
    ) {
        let config = I::FAKE_CONFIG;
        let Ctx { sync_ctx, mut non_sync_ctx } = crate::testutil::FakeCtx::default();
        let device = crate::device::add_ethernet_device(
            &sync_ctx,
            config.local_mac,
            IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
            DEFAULT_INTERFACE_METRIC,
        )
        .into();
        crate::device::testutil::enable_device(&sync_ctx, &mut non_sync_ctx, &device);

        let ip = I::get_other_ip_address(1).get();
        let prefix = config.subnet.prefix();
        let addr_subnet = AddrSubnetEither::new(ip.into(), prefix).unwrap();

        // IP doesn't exist initially.
        assert_eq!(
            get_all_ip_addr_subnets(&sync_ctx, &device).into_iter().find(|&a| a == addr_subnet),
            None
        );

        // Add IP (OK).
        if let Some(addr_config) = addr_config {
            add_ip_addr_subnet(
                &sync_ctx,
                &mut non_sync_ctx,
                &device,
                AddrSubnetAndManualConfigEither::new::<I>(
                    AddrSubnet::new(ip, prefix).unwrap(),
                    addr_config,
                ),
            )
            .unwrap();
        } else {
            let () =
                add_ip_addr_subnet(&sync_ctx, &mut non_sync_ctx, &device, addr_subnet).unwrap();
        }
        assert_eq!(
            get_all_ip_addr_subnets(&sync_ctx, &device).into_iter().find(|&a| a == addr_subnet),
            Some(addr_subnet)
        );

        // Add IP again (already exists).
        assert_eq!(
            add_ip_addr_subnet(&sync_ctx, &mut non_sync_ctx, &device, addr_subnet).unwrap_err(),
            error::ExistsError
        );
        assert_eq!(
            get_all_ip_addr_subnets(&sync_ctx, &device).into_iter().find(|&a| a == addr_subnet),
            Some(addr_subnet)
        );

        // Add IP with different subnet (already exists).
        let wrong_addr_subnet = AddrSubnetEither::new(ip.into(), prefix - 1).unwrap();
        assert_eq!(
            add_ip_addr_subnet(&sync_ctx, &mut non_sync_ctx, &device, wrong_addr_subnet)
                .unwrap_err(),
            error::ExistsError
        );
        assert_eq!(
            get_all_ip_addr_subnets(&sync_ctx, &device).into_iter().find(|&a| a == addr_subnet),
            Some(addr_subnet)
        );

        let ip: SpecifiedAddr<IpAddr> = SpecifiedAddr::new(ip.into()).unwrap();
        // Del IP (ok).
        let () = del_ip_addr(&sync_ctx, &mut non_sync_ctx, &device, ip).unwrap();
        assert_eq!(
            get_all_ip_addr_subnets(&sync_ctx, &device).into_iter().find(|&a| a == addr_subnet),
            None
        );

        // Del IP again (not found).
        assert_eq!(
            del_ip_addr(&sync_ctx, &mut non_sync_ctx, &device, ip).unwrap_err(),
            error::NotFoundError
        );
        assert_eq!(
            get_all_ip_addr_subnets(&sync_ctx, &device).into_iter().find(|&a| a == addr_subnet),
            None
        );
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
