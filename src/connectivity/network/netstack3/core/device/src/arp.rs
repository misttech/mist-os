// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The Address Resolution Protocol (ARP).

use alloc::fmt::Debug;
use core::time::Duration;

use log::{debug, trace, warn};
use net_types::ip::{Ipv4, Ipv4Addr};
use net_types::{SpecifiedAddr, UnicastAddr, Witness as _};
use netstack3_base::{
    CoreTimerContext, Counter, CounterContext, DeviceIdContext, EventContext, FrameDestination,
    InstantBindingsTypes, LinkDevice, SendFrameContext, SendFrameError, TimerContext,
    TracingContext, WeakDeviceIdentifier,
};
use netstack3_ip::nud::{
    self, ConfirmationFlags, DynamicNeighborUpdateSource, LinkResolutionContext, NudBindingsTypes,
    NudConfigContext, NudContext, NudHandler, NudSenderContext, NudState, NudTimerId,
    NudUserConfig,
};
use packet::{BufferMut, InnerPacketBuilder, Serializer};
use packet_formats::arp::{ArpOp, ArpPacket, ArpPacketBuilder, HType};
use packet_formats::utils::NonZeroDuration;
use ref_cast::RefCast;

/// A link device whose addressing scheme is supported by ARP.
///
/// `ArpDevice` is implemented for all `L: LinkDevice where L::Address: HType`.
pub trait ArpDevice: LinkDevice<Address: HType> {}

impl<L: LinkDevice<Address: HType>> ArpDevice for L {}

/// The identifier for timer events in the ARP layer.
pub(crate) type ArpTimerId<L, D> = NudTimerId<Ipv4, L, D>;

/// The metadata associated with an ARP frame.
#[cfg_attr(test, derive(Debug, PartialEq, Clone))]
pub struct ArpFrameMetadata<D: ArpDevice, DeviceId> {
    /// The ID of the ARP device.
    pub(super) device_id: DeviceId,
    /// The destination hardware address.
    pub(super) dst_addr: D::Address,
}

/// Counters for the ARP layer.
#[derive(Default)]
pub struct ArpCounters {
    /// Count of ARP packets received from the link layer.
    pub rx_packets: Counter,
    /// Count of received ARP packets that were dropped due to being unparsable.
    pub rx_malformed_packets: Counter,
    /// Count of ARP request packets received.
    pub rx_requests: Counter,
    /// Count of ARP response packets received.
    pub rx_responses: Counter,
    /// Count of non-gratuitous ARP packets received and dropped because the
    /// destination address is non-local.
    pub rx_dropped_non_local_target: Counter,
    /// Count of ARP request packets sent.
    pub tx_requests: Counter,
    /// Count of ARP request packets not sent because the source address was
    /// unknown or unassigned.
    pub tx_requests_dropped_no_local_addr: Counter,
    /// Count of ARP response packets sent.
    pub tx_responses: Counter,
}

/// An execution context for the ARP protocol that allows sending IP packets to
/// specific neighbors.
pub trait ArpSenderContext<D: ArpDevice, BC: ArpBindingsContext<D, Self::DeviceId>>:
    ArpConfigContext + DeviceIdContext<D>
{
    /// Send an IP packet to the neighbor with address `dst_link_address`.
    fn send_ip_packet_to_neighbor_link_addr<S>(
        &mut self,
        bindings_ctx: &mut BC,
        dst_link_address: D::Address,
        body: S,
    ) -> Result<(), SendFrameError<S>>
    where
        S: Serializer,
        S::Buffer: BufferMut;
}

// NOTE(joshlf): The `ArpDevice` parameter may seem unnecessary. We only ever
// use the associated `HType` type, so why not just take that directly? By the
// same token, why have it as a parameter on `ArpState`, `ArpTimerId`, and
// `ArpFrameMetadata`? The answer is that, if we did, there would be no way to
// distinguish between different link device protocols that all happened to use
// the same hardware addressing scheme.
//
// Consider that the way that we implement context traits is via blanket impls.
// Even though each module's code _feels_ isolated from the rest of the system,
// in reality, all context impls end up on the same context type. In particular,
// all impls are of the form `impl<C: SomeContextTrait> SomeOtherContextTrait
// for C`. The `C` is the same throughout the whole stack.
//
// Thus, for two different link device protocols with the same `HType` and
// `PType`, if we used an `HType` parameter rather than an `ArpDevice`
// parameter, the `ArpContext` impls would conflict (in fact, the
// `StateContext`, `TimerContext`, and `FrameContext` impls would all conflict
// for similar reasons).

/// The execution context for the ARP protocol provided by bindings.
pub trait ArpBindingsContext<D: ArpDevice, DeviceId>:
    TimerContext
    + TracingContext
    + LinkResolutionContext<D>
    + EventContext<nud::Event<D::Address, DeviceId, Ipv4, <Self as InstantBindingsTypes>::Instant>>
{
}

impl<
        DeviceId,
        D: ArpDevice,
        BC: TimerContext
            + TracingContext
            + LinkResolutionContext<D>
            + EventContext<
                nud::Event<D::Address, DeviceId, Ipv4, <Self as InstantBindingsTypes>::Instant>,
            >,
    > ArpBindingsContext<D, DeviceId> for BC
{
}

/// An execution context for the ARP protocol.
pub trait ArpContext<D: ArpDevice, BC: ArpBindingsContext<D, Self::DeviceId>>:
    DeviceIdContext<D>
    + SendFrameContext<BC, ArpFrameMetadata<D, Self::DeviceId>>
    + CounterContext<ArpCounters>
{
    /// The inner configuration context.
    type ConfigCtx<'a>: ArpConfigContext;
    /// The inner sender context.
    type ArpSenderCtx<'a>: ArpSenderContext<D, BC, DeviceId = Self::DeviceId>;

    /// Calls the function with a mutable reference to ARP state and the
    /// core sender context.
    fn with_arp_state_mut_and_sender_ctx<
        O,
        F: FnOnce(&mut ArpState<D, BC>, &mut Self::ArpSenderCtx<'_>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O;

    /// Get a protocol address of this interface.
    ///
    /// If `device_id` does not have any addresses associated with it, return
    /// `None`.
    ///
    /// NOTE: If the interface has multiple addresses, an arbitrary one will be
    /// returned.
    fn get_protocol_addr(&mut self, device_id: &Self::DeviceId) -> Option<Ipv4Addr>;

    /// Check if `addr` is assigned to this interface.
    ///
    /// If `device_id` does not have any addresses, return `false`.
    fn addr_on_interface(&mut self, device_id: &Self::DeviceId, addr: Ipv4Addr) -> bool;

    /// Get the hardware address of this interface.
    fn get_hardware_addr(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
    ) -> UnicastAddr<D::Address>;

    /// Calls the function with a mutable reference to ARP state and the ARP
    /// configuration context.
    fn with_arp_state_mut<O, F: FnOnce(&mut ArpState<D, BC>, &mut Self::ConfigCtx<'_>) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O;

    /// Calls the function with an immutable reference to ARP state.
    fn with_arp_state<O, F: FnOnce(&ArpState<D, BC>) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O;
}

/// An execution context for the ARP protocol that allows accessing
/// configuration parameters.
pub trait ArpConfigContext {
    /// The retransmit timeout for ARP frames.
    ///
    /// Provided implementation always return the default RFC period.
    fn retransmit_timeout(&mut self) -> NonZeroDuration {
        NonZeroDuration::new(DEFAULT_ARP_REQUEST_PERIOD).unwrap()
    }

    /// Calls the callback with an immutable reference to NUD configurations.
    fn with_nud_user_config<O, F: FnOnce(&NudUserConfig) -> O>(&mut self, cb: F) -> O;
}

/// Provides a [`NudContext`] IPv4 implementation for a core context that
/// implements [`ArpContext`].
#[derive(RefCast)]
#[repr(transparent)]
pub struct ArpNudCtx<CC>(CC);

impl<D, CC> DeviceIdContext<D> for ArpNudCtx<CC>
where
    D: ArpDevice,
    CC: DeviceIdContext<D>,
{
    type DeviceId = CC::DeviceId;
    type WeakDeviceId = CC::WeakDeviceId;
}

impl<D, CC, BC> NudContext<Ipv4, D, BC> for ArpNudCtx<CC>
where
    D: ArpDevice,
    BC: ArpBindingsContext<D, CC::DeviceId>,
    CC: ArpContext<D, BC>,
{
    type ConfigCtx<'a> = ArpNudCtx<CC::ConfigCtx<'a>>;
    type SenderCtx<'a> = ArpNudCtx<CC::ArpSenderCtx<'a>>;

    fn with_nud_state_mut_and_sender_ctx<
        O,
        F: FnOnce(&mut NudState<Ipv4, D, BC>, &mut Self::SenderCtx<'_>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        let Self(core_ctx) = self;
        core_ctx.with_arp_state_mut_and_sender_ctx(device_id, |ArpState { nud }, core_ctx| {
            cb(nud, ArpNudCtx::ref_cast_mut(core_ctx))
        })
    }

    fn with_nud_state_mut<
        O,
        F: FnOnce(&mut NudState<Ipv4, D, BC>, &mut Self::ConfigCtx<'_>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        let Self(core_ctx) = self;
        core_ctx.with_arp_state_mut(device_id, |ArpState { nud }, core_ctx| {
            cb(nud, ArpNudCtx::ref_cast_mut(core_ctx))
        })
    }

    fn with_nud_state<O, F: FnOnce(&NudState<Ipv4, D, BC>) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        let Self(core_ctx) = self;
        core_ctx.with_arp_state(device_id, |ArpState { nud }| cb(nud))
    }

    fn send_neighbor_solicitation(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        lookup_addr: SpecifiedAddr<<Ipv4 as net_types::ip::Ip>::Addr>,
        remote_link_addr: Option<<D as LinkDevice>::Address>,
    ) {
        let Self(core_ctx) = self;
        send_arp_request(core_ctx, bindings_ctx, device_id, lookup_addr.get(), remote_link_addr)
    }
}

impl<CC: ArpConfigContext> NudConfigContext<Ipv4> for ArpNudCtx<CC> {
    fn retransmit_timeout(&mut self) -> NonZeroDuration {
        let Self(core_ctx) = self;
        core_ctx.retransmit_timeout()
    }

    fn with_nud_user_config<O, F: FnOnce(&NudUserConfig) -> O>(&mut self, cb: F) -> O {
        let Self(core_ctx) = self;
        core_ctx.with_nud_user_config(cb)
    }
}

impl<D: ArpDevice, BC: ArpBindingsContext<D, CC::DeviceId>, CC: ArpSenderContext<D, BC>>
    NudSenderContext<Ipv4, D, BC> for ArpNudCtx<CC>
{
    fn send_ip_packet_to_neighbor_link_addr<S>(
        &mut self,
        bindings_ctx: &mut BC,
        dst_mac: D::Address,
        body: S,
    ) -> Result<(), SendFrameError<S>>
    where
        S: Serializer,
        S::Buffer: BufferMut,
    {
        let Self(core_ctx) = self;
        core_ctx.send_ip_packet_to_neighbor_link_addr(bindings_ctx, dst_mac, body)
    }
}

pub(crate) trait ArpPacketHandler<D: ArpDevice, BC>: DeviceIdContext<D> {
    fn handle_packet<B: BufferMut + Debug>(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: Self::DeviceId,
        frame_dst: FrameDestination,
        buffer: B,
    );
}

impl<
        D: ArpDevice,
        BC: ArpBindingsContext<D, CC::DeviceId>,
        CC: ArpContext<D, BC>
            + SendFrameContext<BC, ArpFrameMetadata<D, Self::DeviceId>>
            + NudHandler<Ipv4, D, BC>
            + CounterContext<ArpCounters>,
    > ArpPacketHandler<D, BC> for CC
{
    /// Handles an inbound ARP packet.
    fn handle_packet<B: BufferMut + Debug>(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: Self::DeviceId,
        frame_dst: FrameDestination,
        buffer: B,
    ) {
        handle_packet(self, bindings_ctx, device_id, frame_dst, buffer)
    }
}

fn handle_packet<
    D: ArpDevice,
    BC: ArpBindingsContext<D, CC::DeviceId>,
    B: BufferMut + Debug,
    CC: ArpContext<D, BC>
        + SendFrameContext<BC, ArpFrameMetadata<D, CC::DeviceId>>
        + NudHandler<Ipv4, D, BC>
        + CounterContext<ArpCounters>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: CC::DeviceId,
    frame_dst: FrameDestination,
    mut buffer: B,
) {
    core_ctx.increment(|counters| &counters.rx_packets);
    // TODO(wesleyac) Add support for probe.
    let packet = match buffer.parse::<ArpPacket<_, D::Address, Ipv4Addr>>() {
        Ok(packet) => packet,
        Err(err) => {
            // If parse failed, it's because either the packet was malformed, or
            // it was for an unexpected hardware or network protocol. In either
            // case, we just drop the packet and move on. RFC 826's "Packet
            // Reception" section says of packet processing algorithm, "Negative
            // conditionals indicate an end of processing and a discarding of
            // the packet."
            debug!("discarding malformed ARP packet: {}", err);
            core_ctx.increment(|counters| &counters.rx_malformed_packets);
            return;
        }
    };

    enum ValidArpOp {
        Request,
        Response,
    }

    let op = match packet.operation() {
        ArpOp::Request => {
            core_ctx.increment(|counters| &counters.rx_requests);
            ValidArpOp::Request
        }
        ArpOp::Response => {
            core_ctx.increment(|counters| &counters.rx_responses);
            ValidArpOp::Response
        }
        ArpOp::Other(o) => {
            core_ctx.increment(|counters| &counters.rx_malformed_packets);
            debug!("dropping arp packet with op = {:?}", o);
            return;
        }
    };

    enum PacketKind {
        Gratuitous,
        AddressedToMe,
    }

    // The following logic is equivalent to the "Packet Reception" section of
    // RFC 826.
    //
    // We statically know that the hardware type and protocol type are correct,
    // so we do not need to have additional code to check that. The remainder of
    // the algorithm is:
    //
    // Merge_flag := false
    // If the pair <protocol type, sender protocol address> is
    //     already in my translation table, update the sender
    //     hardware address field of the entry with the new
    //     information in the packet and set Merge_flag to true.
    // ?Am I the target protocol address?
    // Yes:
    //   If Merge_flag is false, add the triplet <protocol type,
    //       sender protocol address, sender hardware address> to
    //       the translation table.
    //   ?Is the opcode ares_op$REQUEST?  (NOW look at the opcode!!)
    //   Yes:
    //     Swap hardware and protocol fields, putting the local
    //         hardware and protocol addresses in the sender fields.
    //     Set the ar$op field to ares_op$REPLY
    //     Send the packet to the (new) target hardware address on
    //         the same hardware on which the request was received.
    //
    // This can be summed up as follows:
    //
    // +----------+---------------+---------------+-----------------------------+
    // | opcode   | Am I the TPA? | SPA in table? | action                      |
    // +----------+---------------+---------------+-----------------------------+
    // | REQUEST  | yes           | yes           | Update table, Send response |
    // | REQUEST  | yes           | no            | Update table, Send response |
    // | REQUEST  | no            | yes           | Update table                |
    // | REQUEST  | no            | no            | NOP                         |
    // | RESPONSE | yes           | yes           | Update table                |
    // | RESPONSE | yes           | no            | Update table                |
    // | RESPONSE | no            | yes           | Update table                |
    // | RESPONSE | no            | no            | NOP                         |
    // +----------+---------------+---------------+-----------------------------+

    let sender_addr = packet.sender_protocol_address();
    let target_addr = packet.target_protocol_address();

    let garp = sender_addr == target_addr;
    let targets_interface = core_ctx.addr_on_interface(&device_id, target_addr);
    let (source, kind) = match (garp, targets_interface) {
        (true, false) => {
            // Treat all GARP messages as neighbor probes as GARPs are not
            // responses for previously sent requests, even if the packet
            // operation is a response OP code.
            //
            // Per RFC 5944 section 4.6,
            //
            //   A Gratuitous ARP [45] is an ARP packet sent by a node in order
            //   to spontaneously cause other nodes to update an entry in their
            //   ARP cache. A gratuitous ARP MAY use either an ARP Request or an
            //   ARP Reply packet. In either case, the ARP Sender Protocol
            //   Address and ARP Target Protocol Address are both set to the IP
            //   address of the cache entry to be updated, and the ARP Sender
            //   Hardware Address is set to the link-layer address to which this
            //   cache entry should be updated. When using an ARP Reply packet,
            //   the Target Hardware Address is also set to the link-layer
            //   address to which this cache entry should be updated (this field
            //   is not used in an ARP Request packet).
            //
            //   In either case, for a gratuitous ARP, the ARP packet MUST be
            //   transmitted as a local broadcast packet on the local link. As
            //   specified in [16], any node receiving any ARP packet (Request
            //   or Reply) MUST update its local ARP cache with the Sender
            //   Protocol and Hardware Addresses in the ARP packet, if the
            //   receiving node has an entry for that IP address already in its
            //   ARP cache. This requirement in the ARP protocol applies even
            //   for ARP Request packets, and for ARP Reply packets that do not
            //   match any ARP Request transmitted by the receiving node [16].
            (DynamicNeighborUpdateSource::Probe, PacketKind::Gratuitous)
        }
        (false, true) => {
            // Consider ARP replies as solicited if they were unicast directly to us, and
            // unsolicited otherwise.
            let solicited = match frame_dst {
                FrameDestination::Individual { local } => local,
                FrameDestination::Broadcast | FrameDestination::Multicast => false,
            };
            let source = match op {
                ValidArpOp::Request => DynamicNeighborUpdateSource::Probe,
                ValidArpOp::Response => {
                    DynamicNeighborUpdateSource::Confirmation(ConfirmationFlags {
                        solicited_flag: solicited,
                        // ARP does not have the concept of an override flag in a neighbor
                        // confirmation; if the link address that's received does not match the one
                        // in the neighbor cache, the entry should always go to STALE.
                        override_flag: false,
                    })
                }
            };
            (source, PacketKind::AddressedToMe)
        }
        (false, false) => {
            core_ctx.increment(|counters| &counters.rx_dropped_non_local_target);
            trace!(
                "non-gratuitous ARP packet not targetting us; sender = {}, target={}",
                sender_addr,
                target_addr
            );
            return;
        }
        (true, true) => {
            warn!(
                "got gratuitous ARP packet with our address {target_addr} on device {device_id:?}, \
                dropping...",
            );
            return;
        }
    };

    let sender_hw_addr = packet.sender_hardware_address();
    if let Some(addr) = SpecifiedAddr::new(sender_addr) {
        NudHandler::<Ipv4, D, _>::handle_neighbor_update(
            core_ctx,
            bindings_ctx,
            &device_id,
            addr,
            sender_hw_addr,
            source,
        )
    };

    match kind {
        PacketKind::Gratuitous => return,
        PacketKind::AddressedToMe => match source {
            DynamicNeighborUpdateSource::Probe => {
                let self_hw_addr = core_ctx.get_hardware_addr(bindings_ctx, &device_id);

                core_ctx.increment(|counters| &counters.tx_responses);
                debug!("sending ARP response for {target_addr} to {sender_addr}");

                SendFrameContext::send_frame(
                    core_ctx,
                    bindings_ctx,
                    ArpFrameMetadata { device_id, dst_addr: sender_hw_addr },
                    ArpPacketBuilder::new(
                        ArpOp::Response,
                        self_hw_addr.get(),
                        target_addr,
                        sender_hw_addr,
                        sender_addr,
                    )
                    .into_serializer_with(buffer),
                )
                .unwrap_or_else(|serializer| {
                    warn!(
                        "failed to send ARP response for {target_addr} to {sender_addr}: \
                        {serializer:?}"
                    )
                });
            }
            DynamicNeighborUpdateSource::Confirmation(_flags) => {}
        },
    }
}

// Use the same default retransmit timeout that is defined for NDP in
// [RFC 4861 section 10], to align behavior between IPv4 and IPv6 and simplify
// testing.
//
// TODO(https://fxbug.dev/42075782): allow this default to be overridden.
//
// [RFC 4861 section 10]: https://tools.ietf.org/html/rfc4861#section-10
const DEFAULT_ARP_REQUEST_PERIOD: Duration = nud::RETRANS_TIMER_DEFAULT.get();

fn send_arp_request<
    D: ArpDevice,
    BC: ArpBindingsContext<D, CC::DeviceId>,
    CC: ArpContext<D, BC> + CounterContext<ArpCounters>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    lookup_addr: Ipv4Addr,
    remote_link_addr: Option<D::Address>,
) {
    if let Some(sender_protocol_addr) = core_ctx.get_protocol_addr(device_id) {
        let self_hw_addr = core_ctx.get_hardware_addr(bindings_ctx, device_id);
        let dst_addr = remote_link_addr.unwrap_or(D::Address::BROADCAST);
        core_ctx.increment(|counters| &counters.tx_requests);
        debug!("sending ARP request for {lookup_addr} to {dst_addr:?}");
        SendFrameContext::send_frame(
            core_ctx,
            bindings_ctx,
            ArpFrameMetadata { device_id: device_id.clone(), dst_addr },
            ArpPacketBuilder::new(
                ArpOp::Request,
                self_hw_addr.get(),
                sender_protocol_addr,
                // This is meaningless, since RFC 826 does not specify the
                // behaviour. However, `dst_addr` is sensible, as this is the
                // actual address we are sending the packet to.
                dst_addr,
                lookup_addr,
            )
            .into_serializer(),
        )
        .unwrap_or_else(|serializer| {
            warn!("failed to send ARP request for {lookup_addr} to {dst_addr:?}: {serializer:?}")
        });
    } else {
        // RFC 826 does not specify what to do if we don't have a local address,
        // but there is no reasonable way to send an ARP request without one (as
        // the receiver will cache our local address on receiving the packet.
        // So, if this is the case, we do not send an ARP request.
        // TODO(wesleyac): Should we cache these, and send packets once we have
        // an address?
        core_ctx.increment(|counters| &counters.tx_requests_dropped_no_local_addr);
        debug!("Not sending ARP request, since we don't know our local protocol address");
    }
}

/// The state associated with an instance of the Address Resolution Protocol
/// (ARP).
///
/// Each device will contain an `ArpState` object for each of the network
/// protocols that it supports.
pub struct ArpState<D: ArpDevice, BT: NudBindingsTypes<D>> {
    pub(crate) nud: NudState<Ipv4, D, BT>,
}

impl<D: ArpDevice, BC: NudBindingsTypes<D> + TimerContext> ArpState<D, BC> {
    /// Creates a new `ArpState` for `device_id`.
    pub fn new<
        DeviceId: WeakDeviceIdentifier,
        CC: CoreTimerContext<ArpTimerId<D, DeviceId>, BC>,
    >(
        bindings_ctx: &mut BC,
        device_id: DeviceId,
    ) -> Self {
        ArpState { nud: NudState::new::<_, CC>(bindings_ctx, device_id) }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;
    use core::iter;

    use net_types::ethernet::Mac;
    use netstack3_base::socket::SocketIpAddr;
    use netstack3_base::testutil::{
        assert_empty, FakeBindingsCtx, FakeCoreCtx, FakeDeviceId, FakeInstant, FakeLinkDeviceId,
        FakeNetworkSpec, FakeTimerId, FakeWeakDeviceId, WithFakeFrameContext,
    };
    use netstack3_base::{CtxPair, InstantContext as _, IntoCoreTimerCtx, TimerHandler};
    use netstack3_ip::nud::testutil::{
        assert_dynamic_neighbor_state, assert_dynamic_neighbor_with_addr, assert_neighbor_unknown,
    };
    use netstack3_ip::nud::{
        DelegateNudContext, DynamicNeighborState, NudCounters, NudIcmpContext, Reachable, Stale,
        UseDelegateNudContext,
    };
    use packet::{Buf, ParseBuffer};
    use packet_formats::arp::{peek_arp_types, ArpHardwareType, ArpNetworkType};
    use packet_formats::ipv4::Ipv4FragmentType;
    use test_case::test_case;

    use super::*;
    use crate::internal::ethernet::EthernetLinkDevice;

    const TEST_LOCAL_IPV4: Ipv4Addr = Ipv4Addr::new([1, 2, 3, 4]);
    const TEST_LOCAL_IPV4_2: Ipv4Addr = Ipv4Addr::new([4, 5, 6, 7]);
    const TEST_REMOTE_IPV4: Ipv4Addr = Ipv4Addr::new([5, 6, 7, 8]);
    const TEST_ANOTHER_REMOTE_IPV4: Ipv4Addr = Ipv4Addr::new([9, 10, 11, 12]);
    const TEST_LOCAL_MAC: Mac = Mac::new([0, 1, 2, 3, 4, 5]);
    const TEST_REMOTE_MAC: Mac = Mac::new([6, 7, 8, 9, 10, 11]);
    const TEST_INVALID_MAC: Mac = Mac::new([0, 0, 0, 0, 0, 0]);

    /// A fake `ArpContext` that stores frames, address resolution events, and
    /// address resolution failure events.
    struct FakeArpCtx {
        proto_addrs: Vec<Ipv4Addr>,
        hw_addr: UnicastAddr<Mac>,
        arp_state: ArpState<EthernetLinkDevice, FakeBindingsCtxImpl>,
        inner: FakeArpInnerCtx,
        config: FakeArpConfigCtx,
        counters: ArpCounters,
        nud_counters: NudCounters<Ipv4>,
    }

    /// A fake `ArpSenderContext` that sends IP packets.
    struct FakeArpInnerCtx;

    /// A fake `ArpConfigContext`.
    struct FakeArpConfigCtx;

    impl FakeArpCtx {
        fn new(bindings_ctx: &mut FakeBindingsCtxImpl) -> FakeArpCtx {
            FakeArpCtx {
                proto_addrs: vec![TEST_LOCAL_IPV4, TEST_LOCAL_IPV4_2],
                hw_addr: UnicastAddr::new(TEST_LOCAL_MAC).unwrap(),
                arp_state: ArpState::new::<_, IntoCoreTimerCtx>(
                    bindings_ctx,
                    FakeWeakDeviceId(FakeLinkDeviceId),
                ),
                inner: FakeArpInnerCtx,
                config: FakeArpConfigCtx,
                counters: Default::default(),
                nud_counters: Default::default(),
            }
        }
    }

    type FakeBindingsCtxImpl = FakeBindingsCtx<
        ArpTimerId<EthernetLinkDevice, FakeWeakDeviceId<FakeLinkDeviceId>>,
        nud::Event<Mac, FakeLinkDeviceId, Ipv4, FakeInstant>,
        (),
        (),
    >;

    type FakeCoreCtxImpl = FakeCoreCtx<
        FakeArpCtx,
        ArpFrameMetadata<EthernetLinkDevice, FakeLinkDeviceId>,
        FakeDeviceId,
    >;

    fn new_context() -> CtxPair<FakeCoreCtxImpl, FakeBindingsCtxImpl> {
        CtxPair::with_default_bindings_ctx(|bindings_ctx| {
            FakeCoreCtxImpl::with_state(FakeArpCtx::new(bindings_ctx))
        })
    }

    enum ArpNetworkSpec {}
    impl FakeNetworkSpec for ArpNetworkSpec {
        type Context = CtxPair<FakeCoreCtxImpl, FakeBindingsCtxImpl>;
        type TimerId = ArpTimerId<EthernetLinkDevice, FakeWeakDeviceId<FakeLinkDeviceId>>;
        type SendMeta = ArpFrameMetadata<EthernetLinkDevice, FakeLinkDeviceId>;
        type RecvMeta = ArpFrameMetadata<EthernetLinkDevice, FakeLinkDeviceId>;

        fn handle_frame(
            ctx: &mut Self::Context,
            ArpFrameMetadata { device_id, .. }: Self::RecvMeta,
            data: Buf<Vec<u8>>,
        ) {
            let CtxPair { core_ctx, bindings_ctx } = ctx;
            handle_packet(
                core_ctx,
                bindings_ctx,
                device_id,
                FrameDestination::Individual { local: true },
                data,
            )
        }
        fn handle_timer(ctx: &mut Self::Context, dispatch: Self::TimerId, timer: FakeTimerId) {
            let CtxPair { core_ctx, bindings_ctx } = ctx;
            TimerHandler::handle_timer(core_ctx, bindings_ctx, dispatch, timer)
        }

        fn process_queues(_ctx: &mut Self::Context) -> bool {
            false
        }

        fn fake_frames(ctx: &mut Self::Context) -> &mut impl WithFakeFrameContext<Self::SendMeta> {
            &mut ctx.core_ctx
        }
    }

    impl DeviceIdContext<EthernetLinkDevice> for FakeCoreCtxImpl {
        type DeviceId = FakeLinkDeviceId;
        type WeakDeviceId = FakeWeakDeviceId<FakeLinkDeviceId>;
    }

    impl DeviceIdContext<EthernetLinkDevice> for FakeArpInnerCtx {
        type DeviceId = FakeLinkDeviceId;
        type WeakDeviceId = FakeWeakDeviceId<FakeLinkDeviceId>;
    }

    impl ArpContext<EthernetLinkDevice, FakeBindingsCtxImpl> for FakeCoreCtxImpl {
        type ConfigCtx<'a> = FakeArpConfigCtx;

        type ArpSenderCtx<'a> = FakeArpInnerCtx;

        fn with_arp_state_mut_and_sender_ctx<
            O,
            F: FnOnce(
                &mut ArpState<EthernetLinkDevice, FakeBindingsCtxImpl>,
                &mut Self::ArpSenderCtx<'_>,
            ) -> O,
        >(
            &mut self,
            FakeLinkDeviceId: &FakeLinkDeviceId,
            cb: F,
        ) -> O {
            let FakeArpCtx { arp_state, inner, .. } = &mut self.state;
            cb(arp_state, inner)
        }

        fn with_arp_state<O, F: FnOnce(&ArpState<EthernetLinkDevice, FakeBindingsCtxImpl>) -> O>(
            &mut self,
            FakeLinkDeviceId: &FakeLinkDeviceId,
            cb: F,
        ) -> O {
            cb(&self.state.arp_state)
        }

        fn addr_on_interface(&mut self, _device_id: &FakeLinkDeviceId, addr: Ipv4Addr) -> bool {
            self.state.proto_addrs.iter().any(|&a| a == addr)
        }

        fn get_protocol_addr(&mut self, _device_id: &FakeLinkDeviceId) -> Option<Ipv4Addr> {
            self.state.proto_addrs.first().copied()
        }

        fn get_hardware_addr(
            &mut self,
            _bindings_ctx: &mut FakeBindingsCtxImpl,
            _device_id: &FakeLinkDeviceId,
        ) -> UnicastAddr<Mac> {
            self.state.hw_addr
        }

        fn with_arp_state_mut<
            O,
            F: FnOnce(
                &mut ArpState<EthernetLinkDevice, FakeBindingsCtxImpl>,
                &mut Self::ConfigCtx<'_>,
            ) -> O,
        >(
            &mut self,
            FakeLinkDeviceId: &FakeLinkDeviceId,
            cb: F,
        ) -> O {
            let FakeArpCtx { arp_state, config, .. } = &mut self.state;
            cb(arp_state, config)
        }
    }

    impl UseDelegateNudContext for FakeArpCtx {}
    impl DelegateNudContext<Ipv4> for FakeArpCtx {
        type Delegate<T> = ArpNudCtx<T>;
    }

    impl NudIcmpContext<Ipv4, EthernetLinkDevice, FakeBindingsCtxImpl> for FakeCoreCtxImpl {
        fn send_icmp_dest_unreachable(
            &mut self,
            _bindings_ctx: &mut FakeBindingsCtxImpl,
            _frame: Buf<Vec<u8>>,
            _device_id: Option<&Self::DeviceId>,
            _original_src: SocketIpAddr<Ipv4Addr>,
            _original_dst: SocketIpAddr<Ipv4Addr>,
            _metadata: (usize, Ipv4FragmentType),
        ) {
            panic!("send_icmp_dest_unreachable should not be called");
        }
    }

    impl ArpConfigContext for FakeArpConfigCtx {
        fn with_nud_user_config<O, F: FnOnce(&NudUserConfig) -> O>(&mut self, cb: F) -> O {
            cb(&NudUserConfig::default())
        }
    }
    impl ArpConfigContext for FakeArpInnerCtx {
        fn with_nud_user_config<O, F: FnOnce(&NudUserConfig) -> O>(&mut self, cb: F) -> O {
            cb(&NudUserConfig::default())
        }
    }

    impl ArpSenderContext<EthernetLinkDevice, FakeBindingsCtxImpl> for FakeArpInnerCtx {
        fn send_ip_packet_to_neighbor_link_addr<S>(
            &mut self,
            _bindings_ctx: &mut FakeBindingsCtxImpl,
            _dst_link_address: Mac,
            _body: S,
        ) -> Result<(), SendFrameError<S>> {
            Ok(())
        }
    }

    impl CounterContext<ArpCounters> for FakeArpCtx {
        fn with_counters<O, F: FnOnce(&ArpCounters) -> O>(&self, cb: F) -> O {
            cb(&self.counters)
        }
    }

    impl CounterContext<NudCounters<Ipv4>> for FakeArpCtx {
        fn with_counters<O, F: FnOnce(&NudCounters<Ipv4>) -> O>(&self, cb: F) -> O {
            cb(&self.nud_counters)
        }
    }

    fn send_arp_packet(
        core_ctx: &mut FakeCoreCtxImpl,
        bindings_ctx: &mut FakeBindingsCtxImpl,
        op: ArpOp,
        sender_ipv4: Ipv4Addr,
        target_ipv4: Ipv4Addr,
        sender_mac: Mac,
        target_mac: Mac,
        frame_dst: FrameDestination,
    ) {
        let buf = ArpPacketBuilder::new(op, sender_mac, sender_ipv4, target_mac, target_ipv4)
            .into_serializer()
            .serialize_vec_outer()
            .unwrap();
        let (hw, proto) = peek_arp_types(buf.as_ref()).unwrap();
        assert_eq!(hw, ArpHardwareType::Ethernet);
        assert_eq!(proto, ArpNetworkType::Ipv4);

        handle_packet::<_, _, _, _>(core_ctx, bindings_ctx, FakeLinkDeviceId, frame_dst, buf);
    }

    // Validate that buf is an ARP packet with the specific op, local_ipv4,
    // remote_ipv4, local_mac and remote_mac.
    fn validate_arp_packet(
        mut buf: &[u8],
        op: ArpOp,
        local_ipv4: Ipv4Addr,
        remote_ipv4: Ipv4Addr,
        local_mac: Mac,
        remote_mac: Mac,
    ) {
        let packet = buf.parse::<ArpPacket<_, Mac, Ipv4Addr>>().unwrap();
        assert_eq!(packet.sender_hardware_address(), local_mac);
        assert_eq!(packet.target_hardware_address(), remote_mac);
        assert_eq!(packet.sender_protocol_address(), local_ipv4);
        assert_eq!(packet.target_protocol_address(), remote_ipv4);
        assert_eq!(packet.operation(), op);
    }

    // Validate that we've sent `total_frames` frames in total, and that the
    // most recent one was sent to `dst` with the given ARP packet contents.
    fn validate_last_arp_packet(
        core_ctx: &FakeCoreCtxImpl,
        total_frames: usize,
        dst: Mac,
        op: ArpOp,
        local_ipv4: Ipv4Addr,
        remote_ipv4: Ipv4Addr,
        local_mac: Mac,
        remote_mac: Mac,
    ) {
        assert_eq!(core_ctx.frames().len(), total_frames);
        let (meta, frame) = &core_ctx.frames()[total_frames - 1];
        assert_eq!(meta.dst_addr, dst);
        validate_arp_packet(frame, op, local_ipv4, remote_ipv4, local_mac, remote_mac);
    }

    #[test]
    fn test_receive_gratuitous_arp_request() {
        // Test that, when we receive a gratuitous ARP request, we cache the
        // sender's address information, and we do not send a response.

        let CtxPair { mut core_ctx, mut bindings_ctx } = new_context();
        send_arp_packet(
            &mut core_ctx,
            &mut bindings_ctx,
            ArpOp::Request,
            TEST_REMOTE_IPV4,
            TEST_REMOTE_IPV4,
            TEST_REMOTE_MAC,
            TEST_INVALID_MAC,
            FrameDestination::Individual { local: false },
        );

        // We should have cached the sender's address information.
        assert_dynamic_neighbor_with_addr(
            &mut core_ctx,
            FakeLinkDeviceId,
            SpecifiedAddr::new(TEST_REMOTE_IPV4).unwrap(),
            TEST_REMOTE_MAC,
        );
        // Gratuitous ARPs should not prompt a response.
        assert_empty(core_ctx.frames().iter());
    }

    #[test]
    fn test_receive_gratuitous_arp_response() {
        // Test that, when we receive a gratuitous ARP response, we cache the
        // sender's address information, and we do not send a response.

        let CtxPair { mut core_ctx, mut bindings_ctx } = new_context();
        send_arp_packet(
            &mut core_ctx,
            &mut bindings_ctx,
            ArpOp::Response,
            TEST_REMOTE_IPV4,
            TEST_REMOTE_IPV4,
            TEST_REMOTE_MAC,
            TEST_REMOTE_MAC,
            FrameDestination::Individual { local: false },
        );

        // We should have cached the sender's address information.
        assert_dynamic_neighbor_with_addr(
            &mut core_ctx,
            FakeLinkDeviceId,
            SpecifiedAddr::new(TEST_REMOTE_IPV4).unwrap(),
            TEST_REMOTE_MAC,
        );
        // Gratuitous ARPs should not send a response.
        assert_empty(core_ctx.frames().iter());
    }

    #[test]
    fn test_receive_gratuitous_arp_response_existing_request() {
        // Test that, if we have an outstanding request retry timer and receive
        // a gratuitous ARP for the same host, we cancel the timer and notify
        // the device layer.

        let CtxPair { mut core_ctx, mut bindings_ctx } = new_context();

        // Trigger link resolution.
        assert_neighbor_unknown(
            &mut core_ctx,
            FakeLinkDeviceId,
            SpecifiedAddr::new(TEST_REMOTE_IPV4).unwrap(),
        );
        assert_eq!(
            NudHandler::send_ip_packet_to_neighbor(
                &mut core_ctx,
                &mut bindings_ctx,
                &FakeLinkDeviceId,
                SpecifiedAddr::new(TEST_REMOTE_IPV4).unwrap(),
                Buf::new([1], ..),
            ),
            Ok(())
        );

        send_arp_packet(
            &mut core_ctx,
            &mut bindings_ctx,
            ArpOp::Response,
            TEST_REMOTE_IPV4,
            TEST_REMOTE_IPV4,
            TEST_REMOTE_MAC,
            TEST_REMOTE_MAC,
            FrameDestination::Individual { local: false },
        );

        // The response should now be in our cache.
        assert_dynamic_neighbor_with_addr(
            &mut core_ctx,
            FakeLinkDeviceId,
            SpecifiedAddr::new(TEST_REMOTE_IPV4).unwrap(),
            TEST_REMOTE_MAC,
        );

        // Gratuitous ARPs should not send a response (the 1 frame is for the
        // original request).
        assert_eq!(core_ctx.frames().len(), 1);
    }

    #[test_case(TEST_LOCAL_IPV4)]
    #[test_case(TEST_LOCAL_IPV4_2)]
    fn test_handle_arp_request(local_addr: Ipv4Addr) {
        // Test that, when we receive an ARP request, we cache the sender's
        // address information and send an ARP response.

        let CtxPair { mut core_ctx, mut bindings_ctx } = new_context();

        send_arp_packet(
            &mut core_ctx,
            &mut bindings_ctx,
            ArpOp::Request,
            TEST_REMOTE_IPV4,
            local_addr,
            TEST_REMOTE_MAC,
            TEST_LOCAL_MAC,
            FrameDestination::Individual { local: true },
        );

        // Make sure we cached the sender's address information.
        assert_dynamic_neighbor_with_addr(
            &mut core_ctx,
            FakeLinkDeviceId,
            SpecifiedAddr::new(TEST_REMOTE_IPV4).unwrap(),
            TEST_REMOTE_MAC,
        );

        // We should have sent an ARP response.
        validate_last_arp_packet(
            &core_ctx,
            1,
            TEST_REMOTE_MAC,
            ArpOp::Response,
            local_addr,
            TEST_REMOTE_IPV4,
            TEST_LOCAL_MAC,
            TEST_REMOTE_MAC,
        );
    }

    struct ArpHostConfig<'a> {
        name: &'a str,
        proto_addr: Ipv4Addr,
        hw_addr: Mac,
    }

    #[test_case(ArpHostConfig {
                    name: "remote",
                    proto_addr: TEST_REMOTE_IPV4,
                    hw_addr: TEST_REMOTE_MAC
                },
                vec![]
    )]
    #[test_case(ArpHostConfig {
                    name: "requested_remote",
                    proto_addr: TEST_REMOTE_IPV4,
                    hw_addr: TEST_REMOTE_MAC
                },
                vec![
                    ArpHostConfig {
                        name: "non_requested_remote",
                        proto_addr: TEST_ANOTHER_REMOTE_IPV4,
                        hw_addr: TEST_REMOTE_MAC
                    }
                ]
    )]
    fn test_address_resolution(
        requested_remote_cfg: ArpHostConfig<'_>,
        other_remote_cfgs: Vec<ArpHostConfig<'_>>,
    ) {
        // Test a basic ARP resolution scenario.
        // We expect the following steps:
        // 1. When a lookup is performed and results in a cache miss, we send an
        //    ARP request and set a request retry timer.
        // 2. When the requested remote receives the request, it populates its cache with
        //    the local's information, and sends an ARP reply.
        // 3. Any non-requested remotes will neither populate their caches nor send ARP replies.
        // 4. When the reply is received, the timer is canceled, the table is
        //    updated, a new entry expiration timer is installed, and the device
        //    layer is notified of the resolution.

        const LOCAL_HOST_CFG: ArpHostConfig<'_> =
            ArpHostConfig { name: "local", proto_addr: TEST_LOCAL_IPV4, hw_addr: TEST_LOCAL_MAC };
        let host_iter = other_remote_cfgs
            .iter()
            .chain(iter::once(&requested_remote_cfg))
            .chain(iter::once(&LOCAL_HOST_CFG));

        let mut network = ArpNetworkSpec::new_network(
            {
                host_iter.clone().map(|cfg| {
                    let ArpHostConfig { name, proto_addr, hw_addr } = cfg;
                    let mut ctx = new_context();
                    let CtxPair { core_ctx, bindings_ctx: _ } = &mut ctx;
                    core_ctx.state.hw_addr = UnicastAddr::new(*hw_addr).unwrap();
                    core_ctx.state.proto_addrs = vec![*proto_addr];
                    (*name, ctx)
                })
            },
            |ctx: &str, meta: ArpFrameMetadata<_, _>| {
                host_iter
                    .clone()
                    .filter_map(|cfg| {
                        let ArpHostConfig { name, proto_addr: _, hw_addr: _ } = cfg;
                        if !ctx.eq(*name) {
                            Some((*name, meta.clone(), None))
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
            },
        );

        let ArpHostConfig {
            name: local_name,
            proto_addr: local_proto_addr,
            hw_addr: local_hw_addr,
        } = LOCAL_HOST_CFG;

        let ArpHostConfig {
            name: requested_remote_name,
            proto_addr: requested_remote_proto_addr,
            hw_addr: requested_remote_hw_addr,
        } = requested_remote_cfg;

        // Trigger link resolution.
        network.with_context(local_name, |CtxPair { core_ctx, bindings_ctx }| {
            assert_neighbor_unknown(
                core_ctx,
                FakeLinkDeviceId,
                SpecifiedAddr::new(requested_remote_proto_addr).unwrap(),
            );
            assert_eq!(
                NudHandler::send_ip_packet_to_neighbor(
                    core_ctx,
                    bindings_ctx,
                    &FakeLinkDeviceId,
                    SpecifiedAddr::new(requested_remote_proto_addr).unwrap(),
                    Buf::new([1], ..),
                ),
                Ok(())
            );

            // We should have sent an ARP request.
            validate_last_arp_packet(
                core_ctx,
                1,
                Mac::BROADCAST,
                ArpOp::Request,
                local_proto_addr,
                requested_remote_proto_addr,
                local_hw_addr,
                Mac::BROADCAST,
            );
        });
        // Step once to deliver the ARP request to the remotes.
        let res = network.step();
        assert_eq!(res.timers_fired, 0);

        // Our faked broadcast network should deliver frames to every host other
        // than the sender itself. These should include all non-participating remotes
        // and either the local or the participating remote, depending on who is
        // sending the packet.
        let expected_frames_sent_bcast = other_remote_cfgs.len() + 1;
        assert_eq!(res.frames_sent, expected_frames_sent_bcast);

        // The requested remote should have populated its ARP cache with the local's
        // information.
        network.with_context(requested_remote_name, |CtxPair { core_ctx, bindings_ctx: _ }| {
            assert_dynamic_neighbor_with_addr(
                core_ctx,
                FakeLinkDeviceId,
                SpecifiedAddr::new(local_proto_addr).unwrap(),
                LOCAL_HOST_CFG.hw_addr,
            );

            // The requested remote should have sent an ARP response.
            validate_last_arp_packet(
                core_ctx,
                1,
                local_hw_addr,
                ArpOp::Response,
                requested_remote_proto_addr,
                local_proto_addr,
                requested_remote_hw_addr,
                local_hw_addr,
            );
        });

        // Step once to deliver the ARP response to the local.
        let res = network.step();
        assert_eq!(res.timers_fired, 0);
        assert_eq!(res.frames_sent, expected_frames_sent_bcast);

        // The local should have populated its cache with the remote's
        // information.
        network.with_context(local_name, |CtxPair { core_ctx, bindings_ctx: _ }| {
            assert_dynamic_neighbor_with_addr(
                core_ctx,
                FakeLinkDeviceId,
                SpecifiedAddr::new(requested_remote_proto_addr).unwrap(),
                requested_remote_hw_addr,
            );
        });

        other_remote_cfgs.iter().for_each(
            |ArpHostConfig { name: unrequested_remote_name, proto_addr: _, hw_addr: _ }| {
                // The non-requested_remote should not have populated its ARP cache.
                network.with_context(
                    *unrequested_remote_name,
                    |CtxPair { core_ctx, bindings_ctx: _ }| {
                        // The non-requested_remote should not have sent an ARP response.
                        assert_empty(core_ctx.frames().iter());

                        assert_neighbor_unknown(
                            core_ctx,
                            FakeLinkDeviceId,
                            SpecifiedAddr::new(local_proto_addr).unwrap(),
                        );
                    },
                )
            },
        );
    }

    #[test_case(FrameDestination::Individual { local: true }, true; "unicast to us is solicited")]
    #[test_case(
        FrameDestination::Individual { local: false },
        false;
        "unicast to other addr is unsolicited"
    )]
    #[test_case(FrameDestination::Multicast, false; "multicast reply is unsolicited")]
    #[test_case(FrameDestination::Broadcast, false; "broadcast reply is unsolicited")]
    fn only_unicast_reply_treated_as_solicited(
        frame_dst: FrameDestination,
        expect_solicited: bool,
    ) {
        let CtxPair { mut core_ctx, mut bindings_ctx } = new_context();

        // Trigger link resolution.
        assert_neighbor_unknown(
            &mut core_ctx,
            FakeLinkDeviceId,
            SpecifiedAddr::new(TEST_REMOTE_IPV4).unwrap(),
        );
        assert_eq!(
            NudHandler::send_ip_packet_to_neighbor(
                &mut core_ctx,
                &mut bindings_ctx,
                &FakeLinkDeviceId,
                SpecifiedAddr::new(TEST_REMOTE_IPV4).unwrap(),
                Buf::new([1], ..),
            ),
            Ok(())
        );

        // Now send a confirmation with the specified frame destination.
        send_arp_packet(
            &mut core_ctx,
            &mut bindings_ctx,
            ArpOp::Response,
            TEST_REMOTE_IPV4,
            TEST_LOCAL_IPV4,
            TEST_REMOTE_MAC,
            TEST_LOCAL_MAC,
            frame_dst,
        );

        // If the confirmation was interpreted as solicited, the entry should be
        // marked as REACHABLE; otherwise, it should have transitioned to STALE.
        let expected_state = if expect_solicited {
            DynamicNeighborState::Reachable(Reachable {
                link_address: TEST_REMOTE_MAC,
                last_confirmed_at: bindings_ctx.now(),
            })
        } else {
            DynamicNeighborState::Stale(Stale { link_address: TEST_REMOTE_MAC })
        };
        assert_dynamic_neighbor_state(
            &mut core_ctx,
            FakeLinkDeviceId,
            SpecifiedAddr::new(TEST_REMOTE_IPV4).unwrap(),
            expected_state,
        );
    }
}
