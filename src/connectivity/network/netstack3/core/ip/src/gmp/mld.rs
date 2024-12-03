// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Multicast Listener Discovery (MLD).
//!
//! MLD is derived from version 2 of IPv4's Internet Group Management Protocol,
//! IGMPv2. One important difference to note is that MLD uses ICMPv6 (IP
//! Protocol 58) message types, rather than IGMP (IP Protocol 2) message types.

use core::convert::Infallible as Never;
use core::time::Duration;

use log::{debug, error};
use net_types::ip::{Ip, Ipv6, Ipv6Addr, Ipv6ReservedScope, Ipv6Scope, Ipv6SourceAddr};
use net_types::{LinkLocalUnicastAddr, MulticastAddr, ScopeableAddress, SpecifiedAddr, Witness};
use netstack3_base::{AnyDevice, DeviceIdContext, HandleableTimer, WeakDeviceIdentifier};
use netstack3_filter as filter;
use packet::serialize::Serializer;
use packet::InnerPacketBuilder;
use packet_formats::icmp::mld::{
    IcmpMldv1MessageType, MldPacket, Mldv1MessageBuilder, MulticastListenerDone,
    MulticastListenerReport,
};
use packet_formats::icmp::{IcmpPacketBuilder, IcmpUnusedCode};
use packet_formats::ip::Ipv6Proto;
use packet_formats::ipv6::ext_hdrs::{
    ExtensionHeaderOptionAction, HopByHopOption, HopByHopOptionData,
};
use packet_formats::ipv6::{Ipv6PacketBuilder, Ipv6PacketBuilderWithHbhOptions};
use packet_formats::utils::NonZeroDuration;
use thiserror::Error;
use zerocopy::SplitByteSlice;

use crate::internal::base::{IpLayerHandler, IpPacketDestination};
use crate::internal::gmp::{
    self, GmpBindingsContext, GmpBindingsTypes, GmpContext, GmpContextInner, GmpGroupState,
    GmpStateContext, GmpStateRef, GmpTimerId, GmpTypeLayout, IpExt, MulticastGroupSet,
    NotAMemberErr,
};

/// The bindings types for MLD.
pub trait MldBindingsTypes: GmpBindingsTypes {}
impl<BT> MldBindingsTypes for BT where BT: GmpBindingsTypes {}

/// The bindings execution context for MLD.
pub(crate) trait MldBindingsContext: GmpBindingsContext {}
impl<BC> MldBindingsContext for BC where BC: GmpBindingsContext {}

/// Provides immutable access to MLD state.
pub trait MldStateContext<BT: MldBindingsTypes>:
    DeviceIdContext<AnyDevice> + MldContextMarker
{
    /// Calls the function with an immutable reference to the device's MLD
    /// state.
    fn with_mld_state<O, F: FnOnce(&MulticastGroupSet<Ipv6Addr, GmpGroupState<BT>>) -> O>(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> O;
}

/// The execution context capable of sending frames for MLD.
pub trait MldSendContext<BT: MldBindingsTypes>:
    DeviceIdContext<AnyDevice> + IpLayerHandler<Ipv6, BT> + MldContextMarker
{
    /// Gets the IPv6 link local address on `device`.
    fn get_ipv6_link_local_addr(
        &mut self,
        device: &Self::DeviceId,
    ) -> Option<LinkLocalUnicastAddr<Ipv6Addr>>;
}

/// A marker context for MLD traits to allow for GMP test fakes.
pub trait MldContextMarker {}

/// The execution context for the Multicast Listener Discovery (MLD) protocol.
pub trait MldContext<BT: MldBindingsTypes>: DeviceIdContext<AnyDevice> + MldContextMarker {
    /// The inner context given to `with_mld_state_mut`.
    type SendContext<'a>: MldSendContext<BT, DeviceId = Self::DeviceId> + 'a;

    /// Calls the function with a mutable reference to the device's MLD state
    /// and whether or not MLD is enabled for the `device`.
    fn with_mld_state_mut<
        O,
        F: FnOnce(Self::SendContext<'_>, GmpStateRef<'_, Ipv6, Self, BT>) -> O,
    >(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> O;
}

/// A handler for incoming MLD packets.
///
/// A blanket implementation is provided for all `C: MldContext`.
pub trait MldPacketHandler<BC, DeviceId> {
    /// Receive an MLD packet.
    fn receive_mld_packet<B: SplitByteSlice>(
        &mut self,
        bindings_ctx: &mut BC,
        device: &DeviceId,
        src_ip: Ipv6SourceAddr,
        dst_ip: SpecifiedAddr<Ipv6Addr>,
        packet: MldPacket<B>,
    );
}

impl<BC: MldBindingsContext, CC: MldContext<BC>> MldPacketHandler<BC, CC::DeviceId> for CC {
    fn receive_mld_packet<B: SplitByteSlice>(
        &mut self,
        bindings_ctx: &mut BC,
        device: &CC::DeviceId,
        _src_ip: Ipv6SourceAddr,
        _dst_ip: SpecifiedAddr<Ipv6Addr>,
        packet: MldPacket<B>,
    ) {
        if let Err(e) = match packet {
            MldPacket::MulticastListenerQuery(msg) => {
                let body = msg.body();
                let addr = body.group_addr;
                SpecifiedAddr::new(addr)
                    .map_or(Some(gmp::v1::QueryTarget::Unspecified), |addr| {
                        MulticastAddr::new(addr.get()).map(gmp::v1::QueryTarget::Specified)
                    })
                    .map_or(Err(MldError::NotAMember { addr }), |group_addr| {
                        gmp::v1::handle_query_message(
                            self,
                            bindings_ctx,
                            device,
                            group_addr,
                            body.max_response_delay(),
                        )
                        .map_err(Into::into)
                    })
            }
            MldPacket::MulticastListenerQueryV2(_msg) => {
                debug!("TODO(https://fxbug.dev/42071006): Support MLDv2");
                return;
            }
            MldPacket::MulticastListenerReport(msg) => {
                let addr = msg.body().group_addr;
                MulticastAddr::new(msg.body().group_addr).map_or(
                    Err(MldError::NotAMember { addr }),
                    |group_addr| {
                        gmp::v1::handle_report_message(self, bindings_ctx, device, group_addr)
                            .map_err(Into::into)
                    },
                )
            }
            MldPacket::MulticastListenerDone(_) => {
                debug!("Hosts are not interested in Done messages");
                return;
            }
            MldPacket::MulticastListenerReportV2(_) => {
                debug!("TODO(https://fxbug.dev/42071006): Support MLDv2");
                return;
            }
        } {
            debug!("Error occurred when handling MLD message: {}", e);
        }
    }
}

impl IpExt for Ipv6 {
    fn should_perform_gmp(group_addr: MulticastAddr<Ipv6Addr>) -> bool {
        // Per [RFC 3810 Section 6]:
        //
        // > No MLD messages are ever sent regarding neither the link-scope
        // > all-nodes multicast address, nor any multicast address of scope 0
        // > (reserved) or 1 (node-local).
        //
        // We abide by this requirement by not executing [`Actions`] on these
        // addresses. Executing [`Actions`] only produces externally-visible side
        // effects, and is not required to maintain the correctness of the MLD state
        // machines.
        //
        // [RFC 3810 Section 6]: https://tools.ietf.org/html/rfc3810#section-6
        group_addr != Ipv6::ALL_NODES_LINK_LOCAL_MULTICAST_ADDRESS
            && ![Ipv6Scope::Reserved(Ipv6ReservedScope::Scope0), Ipv6Scope::InterfaceLocal]
                .contains(&group_addr.scope())
    }
}

impl<BT: MldBindingsTypes, CC: DeviceIdContext<AnyDevice> + MldContextMarker>
    GmpTypeLayout<Ipv6, BT> for CC
{
    type Actions = Never;
    type Config = MldConfig;
}

impl<BT: MldBindingsTypes, CC: MldStateContext<BT>> GmpStateContext<Ipv6, BT> for CC {
    fn with_gmp_state<O, F: FnOnce(&MulticastGroupSet<Ipv6Addr, GmpGroupState<BT>>) -> O>(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> O {
        self.with_mld_state(device, cb)
    }
}

impl<BC: MldBindingsContext, CC: MldContext<BC>> GmpContext<Ipv6, BC> for CC {
    type Inner<'a> = CC::SendContext<'a>;

    fn with_gmp_state_mut_and_ctx<
        O,
        F: FnOnce(Self::Inner<'_>, GmpStateRef<'_, Ipv6, Self, BC>) -> O,
    >(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> O {
        self.with_mld_state_mut(device, cb)
    }
}

impl<BC: MldBindingsContext, CC: MldSendContext<BC>> GmpContextInner<Ipv6, BC> for CC {
    fn send_message_v1(
        &mut self,
        bindings_ctx: &mut BC,
        device: &Self::DeviceId,
        group_addr: MulticastAddr<Ipv6Addr>,
        msg_type: gmp::v1::GmpMessageType,
    ) {
        let result = match msg_type {
            gmp::v1::GmpMessageType::Report => send_mld_packet::<_, _, _>(
                self,
                bindings_ctx,
                device,
                group_addr,
                MulticastListenerReport,
                group_addr,
                (),
            ),
            gmp::v1::GmpMessageType::Leave => send_mld_packet::<_, _, _>(
                self,
                bindings_ctx,
                device,
                Ipv6::ALL_ROUTERS_LINK_LOCAL_MULTICAST_ADDRESS,
                MulticastListenerDone,
                group_addr,
                (),
            ),
        };

        match result {
            Ok(()) => {}
            Err(err) => error!(
                "error sending MLD message ({msg_type:?}) on device {device:?} for group \
                {group_addr}: {err}",
            ),
        }
    }

    fn run_actions(&mut self, _bindings_ctx: &mut BC, _device: &CC::DeviceId, actions: Never) {
        match actions {}
    }

    fn handle_mode_change(
        &mut self,
        _bindings_ctx: &mut BC,
        _device: &Self::DeviceId,
        _new_mode: gmp::GmpMode,
    ) {
    }
}

#[derive(Debug, Error)]
pub(crate) enum MldError {
    /// The host is trying to operate on an group address of which the host is
    /// not a member.
    #[error("the host has not already been a member of the address: {}", addr)]
    NotAMember { addr: Ipv6Addr },
    /// Failed to send an IGMP packet.
    #[error("failed to send out an IGMP packet to address: {}", addr)]
    SendFailure { addr: Ipv6Addr },
}

impl From<NotAMemberErr<Ipv6>> for MldError {
    fn from(NotAMemberErr(addr): NotAMemberErr<Ipv6>) -> Self {
        Self::NotAMember { addr }
    }
}

pub(crate) type MldResult<T> = Result<T, MldError>;

#[derive(Debug)]
pub struct MldConfig {
    unsolicited_report_interval: Duration,
    send_leave_anyway: bool,
}

/// The default value for `unsolicited_report_interval` [RFC 2710 Section 7.10]
///
/// [RFC 2710 Section 7.10]: https://tools.ietf.org/html/rfc2710#section-7.10
pub const MLD_DEFAULT_UNSOLICITED_REPORT_INTERVAL: Duration = Duration::from_secs(10);

impl Default for MldConfig {
    fn default() -> Self {
        MldConfig {
            unsolicited_report_interval: MLD_DEFAULT_UNSOLICITED_REPORT_INTERVAL,
            send_leave_anyway: false,
        }
    }
}

impl gmp::v1::ProtocolConfig for MldConfig {
    fn unsolicited_report_interval(&self) -> Duration {
        self.unsolicited_report_interval
    }

    fn send_leave_anyway(&self) -> bool {
        self.send_leave_anyway
    }

    fn get_max_resp_time(&self, resp_time: Duration) -> Option<NonZeroDuration> {
        NonZeroDuration::new(resp_time)
    }

    type QuerySpecificActions = Never;
    fn do_query_received_specific(&self, _max_resp_time: Duration) -> Option<Never> {
        None
    }
}

impl gmp::v2::ProtocolConfig for MldConfig {
    fn query_response_interval(&self) -> NonZeroDuration {
        gmp::v2::DEFAULT_QUERY_RESPONSE_INTERVAL
    }
}

/// An MLD timer to delay the sending of a report.
#[derive(PartialEq, Eq, Clone, Copy, Debug, Hash)]
pub struct MldTimerId<D: WeakDeviceIdentifier>(GmpTimerId<Ipv6, D>);

impl<D: WeakDeviceIdentifier> MldTimerId<D> {
    pub(crate) fn device_id(&self) -> &D {
        let Self(this) = self;
        this.device_id()
    }

    /// Creates a new [`MldTimerId`] for a GMP delayed report on `device`.
    #[cfg(any(test, feature = "testutils"))]
    pub fn new_delayed_report(device: D) -> Self {
        Self(GmpTimerId { device, _marker: Default::default() })
    }
}

impl<D: WeakDeviceIdentifier> From<GmpTimerId<Ipv6, D>> for MldTimerId<D> {
    fn from(id: GmpTimerId<Ipv6, D>) -> MldTimerId<D> {
        MldTimerId(id)
    }
}

impl<BC: MldBindingsContext, CC: MldContext<BC>> HandleableTimer<CC, BC>
    for MldTimerId<CC::WeakDeviceId>
{
    fn handle(self, core_ctx: &mut CC, bindings_ctx: &mut BC, _: BC::UniqueTimerId) {
        let Self(id) = self;
        gmp::handle_timer(core_ctx, bindings_ctx, id);
    }
}

/// Send an MLD packet.
///
/// The MLD packet being sent should have its `hop_limit` to be 1 and a
/// `RouterAlert` option in its Hop-by-Hop Options extensions header.
fn send_mld_packet<
    BC: MldBindingsContext,
    CC: MldSendContext<BC>,
    M: IcmpMldv1MessageType + filter::IcmpMessage<Ipv6>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    dst_ip: MulticastAddr<Ipv6Addr>,
    msg: M,
    group_addr: M::GroupAddr,
    max_resp_delay: M::MaxRespDelay,
) -> MldResult<()> {
    // According to https://tools.ietf.org/html/rfc3590#section-4, if a valid
    // link-local address is not available for the device (e.g., one has not
    // been configured), the message is sent with the unspecified address (::)
    // as the IPv6 source address.
    //
    // TODO(https://fxbug.dev/42180878): Handle an IPv6 link-local address being
    // assigned when reports were sent with the unspecified source address.
    let src_ip =
        core_ctx.get_ipv6_link_local_addr(device).map_or(Ipv6::UNSPECIFIED_ADDRESS, |x| x.get());

    let body = Mldv1MessageBuilder::<M>::new_with_max_resp_delay(group_addr, max_resp_delay)
        .into_serializer()
        .encapsulate(IcmpPacketBuilder::new(src_ip, dst_ip.get(), IcmpUnusedCode, msg))
        .encapsulate(
            Ipv6PacketBuilderWithHbhOptions::new(
                Ipv6PacketBuilder::new(src_ip, dst_ip.get(), 1, Ipv6Proto::Icmpv6),
                &[HopByHopOption {
                    action: ExtensionHeaderOptionAction::SkipAndContinue,
                    mutable: false,
                    data: HopByHopOptionData::RouterAlert { data: 0 },
                }],
            )
            .unwrap(),
        );

    let destination = IpPacketDestination::Multicast(dst_ip);
    IpLayerHandler::send_ip_frame(core_ctx, bindings_ctx, &device, destination, body)
        .map_err(|_| MldError::SendFailure { addr: group_addr.into() })
}

#[cfg(test)]
mod tests {

    use core::cell::RefCell;

    use alloc::rc::Rc;
    use assert_matches::assert_matches;
    use net_types::ethernet::Mac;
    use net_types::ip::{Ip as _, IpVersionMarker};
    use netstack3_base::testutil::{
        assert_empty, new_rng, run_with_many_seeds, FakeDeviceId, FakeInstant, FakeTimerCtxExt,
        FakeWeakDeviceId,
    };
    use netstack3_base::{CtxPair, InstantContext as _, IntoCoreTimerCtx, SendFrameContext};
    use packet::{BufferMut, ParseBuffer};
    use packet_formats::icmp::mld::MulticastListenerQuery;
    use packet_formats::icmp::{IcmpParseArgs, Icmpv6MessageType, Icmpv6Packet};

    use super::*;
    use crate::internal::base::{IpPacketDestination, IpSendFrameError, SendIpPacketMeta};
    use crate::internal::fragmentation::FragmentableIpSerializer;
    use crate::internal::gmp::{GmpHandler as _, GmpState, GroupJoinResult, GroupLeaveResult};

    /// Metadata for sending an MLD packet in an IP packet.
    #[derive(Debug, PartialEq)]
    pub(crate) struct MldFrameMetadata<D> {
        pub(crate) device: D,
        pub(crate) dst_ip: MulticastAddr<Ipv6Addr>,
    }

    impl<D> MldFrameMetadata<D> {
        fn new(device: D, dst_ip: MulticastAddr<Ipv6Addr>) -> MldFrameMetadata<D> {
            MldFrameMetadata { device, dst_ip }
        }
    }

    /// A fake [`MldContext`] that stores the [`MldInterface`] and an optional
    /// IPv6 link-local address that may be returned in calls to
    /// [`MldContext::get_ipv6_link_local_addr`].
    struct FakeMldCtx {
        shared: Rc<RefCell<Shared>>,
        mld_enabled: bool,
        ipv6_link_local: Option<LinkLocalUnicastAddr<Ipv6Addr>>,
    }

    impl FakeMldCtx {
        fn gmp_state(&mut self) -> &mut GmpState<Ipv6, FakeBindingsCtxImpl> {
            &mut Rc::get_mut(&mut self.shared).unwrap().get_mut().gmp_state
        }

        fn groups(
            &mut self,
        ) -> &mut MulticastGroupSet<Ipv6Addr, GmpGroupState<FakeBindingsCtxImpl>> {
            &mut Rc::get_mut(&mut self.shared).unwrap().get_mut().groups
        }
    }

    /// The parts of `FakeMldCtx` that are behind a RefCell, mocking a lock.
    struct Shared {
        groups: MulticastGroupSet<Ipv6Addr, GmpGroupState<FakeBindingsCtxImpl>>,
        gmp_state: GmpState<Ipv6, FakeBindingsCtxImpl>,
        config: MldConfig,
    }

    fn new_context() -> FakeCtxImpl {
        FakeCtxImpl::with_default_bindings_ctx(|bindings_ctx| {
            FakeCoreCtxImpl::with_state(FakeMldCtx {
                shared: Rc::new(RefCell::new(Shared {
                    groups: MulticastGroupSet::default(),
                    gmp_state: GmpState::new::<_, IntoCoreTimerCtx>(
                        bindings_ctx,
                        FakeWeakDeviceId(FakeDeviceId),
                    ),
                    config: Default::default(),
                })),
                mld_enabled: true,
                ipv6_link_local: None,
            })
        })
    }

    type FakeCtxImpl = CtxPair<FakeCoreCtxImpl, FakeBindingsCtxImpl>;
    type FakeCoreCtxImpl = netstack3_base::testutil::FakeCoreCtx<
        FakeMldCtx,
        MldFrameMetadata<FakeDeviceId>,
        FakeDeviceId,
    >;
    type FakeBindingsCtxImpl = netstack3_base::testutil::FakeBindingsCtx<
        MldTimerId<FakeWeakDeviceId<FakeDeviceId>>,
        (),
        (),
        (),
    >;

    impl MldContextMarker for FakeCoreCtxImpl {}
    impl MldContextMarker for &'_ mut FakeCoreCtxImpl {}

    impl MldStateContext<FakeBindingsCtxImpl> for FakeCoreCtxImpl {
        fn with_mld_state<
            O,
            F: FnOnce(&MulticastGroupSet<Ipv6Addr, GmpGroupState<FakeBindingsCtxImpl>>) -> O,
        >(
            &mut self,
            &FakeDeviceId: &FakeDeviceId,
            cb: F,
        ) -> O {
            cb(&self.state.shared.borrow().groups)
        }
    }

    impl MldContext<FakeBindingsCtxImpl> for FakeCoreCtxImpl {
        type SendContext<'a> = &'a mut Self;
        fn with_mld_state_mut<
            O,
            F: FnOnce(Self::SendContext<'_>, GmpStateRef<'_, Ipv6, Self, FakeBindingsCtxImpl>) -> O,
        >(
            &mut self,
            &FakeDeviceId: &FakeDeviceId,
            cb: F,
        ) -> O {
            let FakeMldCtx { mld_enabled, shared, .. } = &mut self.state;
            let enabled = *mld_enabled;
            let shared = Rc::clone(shared);
            let mut shared = shared.borrow_mut();
            let Shared { gmp_state, groups, config } = &mut *shared;
            cb(self, GmpStateRef { enabled, groups, gmp: gmp_state, config })
        }
    }

    impl MldSendContext<FakeBindingsCtxImpl> for &mut FakeCoreCtxImpl {
        fn get_ipv6_link_local_addr(
            &mut self,
            _device: &FakeDeviceId,
        ) -> Option<LinkLocalUnicastAddr<Ipv6Addr>> {
            self.state.ipv6_link_local
        }
    }

    impl IpLayerHandler<Ipv6, FakeBindingsCtxImpl> for &mut FakeCoreCtxImpl {
        fn send_ip_packet_from_device<S>(
            &mut self,
            _bindings_ctx: &mut FakeBindingsCtxImpl,
            _meta: SendIpPacketMeta<
                Ipv6,
                &Self::DeviceId,
                Option<SpecifiedAddr<<Ipv6 as Ip>::Addr>>,
            >,
            _body: S,
        ) -> Result<(), IpSendFrameError<S>>
        where
            S: Serializer,
            S::Buffer: BufferMut,
        {
            unimplemented!();
        }

        fn send_ip_frame<S>(
            &mut self,
            bindings_ctx: &mut FakeBindingsCtxImpl,
            device: &Self::DeviceId,
            destination: IpPacketDestination<Ipv6, &Self::DeviceId>,
            body: S,
        ) -> Result<(), IpSendFrameError<S>>
        where
            S: FragmentableIpSerializer<Ipv6, Buffer: BufferMut> + netstack3_filter::IpPacket<Ipv6>,
        {
            let addr = match destination {
                IpPacketDestination::Multicast(addr) => addr,
                _ => panic!("destination is not multicast: {:?}", destination),
            };
            (*self)
                .send_frame(bindings_ctx, MldFrameMetadata::new(device.clone(), addr), body)
                .map_err(|e| e.err_into())
        }
    }

    #[test]
    fn test_mld_immediate_report() {
        run_with_many_seeds(|seed| {
            // Most of the test surface is covered by the GMP implementation,
            // MLD specific part is mostly passthrough. This test case is here
            // because MLD allows a router to ask for report immediately, by
            // specifying the `MaxRespDelay` to be 0. If this is the case, the
            // host should send the report immediately instead of setting a
            // timer.
            let mut rng = new_rng(seed);
            let cfg = MldConfig::default();
            let (mut s, _actions) =
                gmp::v1::GmpStateMachine::join_group(&mut rng, FakeInstant::default(), false, &cfg);
            assert_eq!(
                s.query_received(&mut rng, Duration::from_secs(0), FakeInstant::default(), &cfg),
                gmp::v1::QueryReceivedActions {
                    generic: Some(gmp::v1::QueryReceivedGenericAction::StopTimerAndSendReport),
                    protocol_specific: None
                }
            );
        });
    }

    const MY_IP: SpecifiedAddr<Ipv6Addr> = unsafe {
        SpecifiedAddr::new_unchecked(Ipv6Addr::from_bytes([
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 192, 168, 0, 3,
        ]))
    };
    const MY_MAC: Mac = Mac::new([1, 2, 3, 4, 5, 6]);
    const ROUTER_MAC: Mac = Mac::new([6, 5, 4, 3, 2, 1]);
    const GROUP_ADDR: MulticastAddr<Ipv6Addr> = <Ipv6 as gmp::testutil::TestIpExt>::GROUP_ADDR1;
    const TIMER_ID: MldTimerId<FakeWeakDeviceId<FakeDeviceId>> = MldTimerId(GmpTimerId {
        device: FakeWeakDeviceId(FakeDeviceId),
        _marker: IpVersionMarker::new(),
    });

    fn receive_mld_query(
        core_ctx: &mut FakeCoreCtxImpl,
        bindings_ctx: &mut FakeBindingsCtxImpl,
        resp_time: Duration,
        group_addr: MulticastAddr<Ipv6Addr>,
    ) {
        let router_addr: Ipv6Addr = ROUTER_MAC.to_ipv6_link_local().addr().get();
        let mut buffer = Mldv1MessageBuilder::<MulticastListenerQuery>::new_with_max_resp_delay(
            group_addr.get(),
            resp_time.try_into().unwrap(),
        )
        .into_serializer()
        .encapsulate(IcmpPacketBuilder::<_, _>::new(
            router_addr,
            MY_IP,
            IcmpUnusedCode,
            MulticastListenerQuery,
        ))
        .serialize_vec_outer()
        .unwrap();
        match buffer
            .parse_with::<_, Icmpv6Packet<_>>(IcmpParseArgs::new(router_addr, MY_IP))
            .unwrap()
        {
            Icmpv6Packet::Mld(packet) => core_ctx.receive_mld_packet(
                bindings_ctx,
                &FakeDeviceId,
                router_addr.try_into().unwrap(),
                MY_IP,
                packet,
            ),
            _ => panic!("serialized icmpv6 message is not an mld message"),
        }
    }

    fn receive_mld_report(
        core_ctx: &mut FakeCoreCtxImpl,
        bindings_ctx: &mut FakeBindingsCtxImpl,
        group_addr: MulticastAddr<Ipv6Addr>,
    ) {
        let router_addr: Ipv6Addr = ROUTER_MAC.to_ipv6_link_local().addr().get();
        let mut buffer = Mldv1MessageBuilder::<MulticastListenerReport>::new(group_addr)
            .into_serializer()
            .encapsulate(IcmpPacketBuilder::<_, _>::new(
                router_addr,
                MY_IP,
                IcmpUnusedCode,
                MulticastListenerReport,
            ))
            .serialize_vec_outer()
            .unwrap()
            .unwrap_b();
        match buffer
            .parse_with::<_, Icmpv6Packet<_>>(IcmpParseArgs::new(router_addr, MY_IP))
            .unwrap()
        {
            Icmpv6Packet::Mld(packet) => core_ctx.receive_mld_packet(
                bindings_ctx,
                &FakeDeviceId,
                router_addr.try_into().unwrap(),
                MY_IP,
                packet,
            ),
            _ => panic!("serialized icmpv6 message is not an mld message"),
        }
    }

    // Ensure the ttl is 1.
    fn ensure_ttl(frame: &[u8]) {
        assert_eq!(frame[7], 1);
    }

    fn ensure_slice_addr(frame: &[u8], start: usize, end: usize, ip: Ipv6Addr) {
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&frame[start..end]);
        assert_eq!(Ipv6Addr::from_bytes(bytes), ip);
    }

    // Ensure the destination address field in the ICMPv6 packet is correct.
    fn ensure_dst_addr(frame: &[u8], ip: Ipv6Addr) {
        ensure_slice_addr(frame, 24, 40, ip);
    }

    // Ensure the multicast address field in the MLD packet is correct.
    fn ensure_multicast_addr(frame: &[u8], ip: Ipv6Addr) {
        ensure_slice_addr(frame, 56, 72, ip);
    }

    // Ensure a sent frame meets the requirement.
    fn ensure_frame(
        frame: &[u8],
        op: u8,
        dst: MulticastAddr<Ipv6Addr>,
        multicast: MulticastAddr<Ipv6Addr>,
    ) {
        ensure_ttl(frame);
        assert_eq!(frame[48], op);
        // Ensure the length our payload is 32 = 8 (hbh_ext_hdr) + 24 (mld)
        assert_eq!(frame[5], 32);
        // Ensure the next header is our HopByHop Extension Header.
        assert_eq!(frame[6], 0);
        // Ensure there is a RouterAlert HopByHopOption in our sent frame
        assert_eq!(&frame[40..48], &[58, 0, 5, 2, 0, 0, 1, 0]);
        ensure_ttl(&frame[..]);
        ensure_dst_addr(&frame[..], dst.get());
        ensure_multicast_addr(&frame[..], multicast.get());
    }

    #[test]
    fn test_mld_simple_integration() {
        run_with_many_seeds(|seed| {
            let FakeCtxImpl { mut core_ctx, mut bindings_ctx } = new_context();
            bindings_ctx.seed_rng(seed);

            assert_eq!(
                core_ctx.gmp_join_group(&mut bindings_ctx, &FakeDeviceId, GROUP_ADDR),
                GroupJoinResult::Joined(())
            );

            receive_mld_query(
                &mut core_ctx,
                &mut bindings_ctx,
                Duration::from_secs(10),
                GROUP_ADDR,
            );
            core_ctx
                .state
                .gmp_state()
                .timers
                .assert_top(&gmp::v1::DelayedReportTimerId(GROUP_ADDR).into(), &());
            assert_eq!(bindings_ctx.trigger_next_timer(&mut core_ctx), Some(TIMER_ID));

            // We should get two MLD reports - one for the unsolicited one for
            // the host to turn into Delay Member state and the other one for
            // the timer being fired.
            assert_eq!(core_ctx.frames().len(), 2);
            // The frames are all reports.
            for (_, frame) in core_ctx.frames() {
                ensure_frame(&frame, 131, GROUP_ADDR, GROUP_ADDR);
                ensure_slice_addr(&frame, 8, 24, Ipv6::UNSPECIFIED_ADDRESS);
            }
        });
    }

    #[test]
    fn test_mld_immediate_query() {
        run_with_many_seeds(|seed| {
            let FakeCtxImpl { mut core_ctx, mut bindings_ctx } = new_context();
            bindings_ctx.seed_rng(seed);

            assert_eq!(
                core_ctx.gmp_join_group(&mut bindings_ctx, &FakeDeviceId, GROUP_ADDR),
                GroupJoinResult::Joined(())
            );
            assert_eq!(core_ctx.frames().len(), 1);

            receive_mld_query(&mut core_ctx, &mut bindings_ctx, Duration::from_secs(0), GROUP_ADDR);
            // The query says that it wants to hear from us immediately.
            assert_eq!(core_ctx.frames().len(), 2);
            // There should be no timers set.
            assert_eq!(bindings_ctx.trigger_next_timer(&mut core_ctx), None);
            // The frames are all reports.
            for (_, frame) in core_ctx.frames() {
                ensure_frame(&frame, 131, GROUP_ADDR, GROUP_ADDR);
                ensure_slice_addr(&frame, 8, 24, Ipv6::UNSPECIFIED_ADDRESS);
            }
        });
    }

    #[test]
    fn test_mld_integration_fallback_from_idle() {
        run_with_many_seeds(|seed| {
            let FakeCtxImpl { mut core_ctx, mut bindings_ctx } = new_context();
            bindings_ctx.seed_rng(seed);

            assert_eq!(
                core_ctx.gmp_join_group(&mut bindings_ctx, &FakeDeviceId, GROUP_ADDR),
                GroupJoinResult::Joined(())
            );
            assert_eq!(core_ctx.frames().len(), 1);

            core_ctx
                .state
                .gmp_state()
                .timers
                .assert_top(&gmp::v1::DelayedReportTimerId(GROUP_ADDR).into(), &());
            assert_eq!(bindings_ctx.trigger_next_timer(&mut core_ctx), Some(TIMER_ID));
            assert_eq!(core_ctx.frames().len(), 2);

            receive_mld_query(
                &mut core_ctx,
                &mut bindings_ctx,
                Duration::from_secs(10),
                GROUP_ADDR,
            );

            // We have received a query, hence we are falling back to Delay
            // Member state.
            let group_state = core_ctx.state.groups().get(&GROUP_ADDR).unwrap();
            match group_state.v1().get_inner() {
                gmp::v1::MemberState::Delaying(_) => {}
                _ => panic!("Wrong State!"),
            }

            core_ctx
                .state
                .gmp_state()
                .timers
                .assert_top(&gmp::v1::DelayedReportTimerId(GROUP_ADDR).into(), &());
            assert_eq!(bindings_ctx.trigger_next_timer(&mut core_ctx), Some(TIMER_ID));
            assert_eq!(core_ctx.frames().len(), 3);
            // The frames are all reports.
            for (_, frame) in core_ctx.frames() {
                ensure_frame(&frame, 131, GROUP_ADDR, GROUP_ADDR);
                ensure_slice_addr(&frame, 8, 24, Ipv6::UNSPECIFIED_ADDRESS);
            }
        });
    }

    #[test]
    fn test_mld_integration_immediate_query_wont_fallback() {
        run_with_many_seeds(|seed| {
            let FakeCtxImpl { mut core_ctx, mut bindings_ctx } = new_context();
            bindings_ctx.seed_rng(seed);

            assert_eq!(
                core_ctx.gmp_join_group(&mut bindings_ctx, &FakeDeviceId, GROUP_ADDR),
                GroupJoinResult::Joined(())
            );
            assert_eq!(core_ctx.frames().len(), 1);

            core_ctx
                .state
                .gmp_state()
                .timers
                .assert_top(&gmp::v1::DelayedReportTimerId(GROUP_ADDR).into(), &());
            assert_eq!(bindings_ctx.trigger_next_timer(&mut core_ctx), Some(TIMER_ID));
            assert_eq!(core_ctx.frames().len(), 2);

            receive_mld_query(&mut core_ctx, &mut bindings_ctx, Duration::from_secs(0), GROUP_ADDR);

            // Since it is an immediate query, we will send a report immediately
            // and turn into Idle state again.
            let group_state = core_ctx.state.groups().get(&GROUP_ADDR).unwrap();
            match group_state.v1().get_inner() {
                gmp::v1::MemberState::Idle(_) => {}
                _ => panic!("Wrong State!"),
            }

            // No timers!
            assert_eq!(bindings_ctx.trigger_next_timer(&mut core_ctx), None);
            assert_eq!(core_ctx.frames().len(), 3);
            // The frames are all reports.
            for (_, frame) in core_ctx.frames() {
                ensure_frame(&frame, 131, GROUP_ADDR, GROUP_ADDR);
                ensure_slice_addr(&frame, 8, 24, Ipv6::UNSPECIFIED_ADDRESS);
            }
        });
    }

    #[test]
    fn test_mld_integration_delay_reset_timer() {
        let FakeCtxImpl { mut core_ctx, mut bindings_ctx } = new_context();
        // This seed was carefully chosen to produce a substantial duration
        // value below.
        bindings_ctx.seed_rng(123456);
        assert_eq!(
            core_ctx.gmp_join_group(&mut bindings_ctx, &FakeDeviceId, GROUP_ADDR),
            GroupJoinResult::Joined(())
        );

        core_ctx.state.gmp_state().timers.assert_timers([(
            gmp::v1::DelayedReportTimerId(GROUP_ADDR).into(),
            (),
            FakeInstant::from(Duration::from_micros(590_354)),
        )]);
        let instant1 = bindings_ctx.timers.timers()[0].0.clone();
        let start = bindings_ctx.now();
        let duration = instant1 - start;

        receive_mld_query(&mut core_ctx, &mut bindings_ctx, duration, GROUP_ADDR);
        assert_eq!(core_ctx.frames().len(), 1);
        core_ctx.state.gmp_state().timers.assert_timers([(
            gmp::v1::DelayedReportTimerId(GROUP_ADDR).into(),
            (),
            FakeInstant::from(Duration::from_micros(34_751)),
        )]);
        let instant2 = bindings_ctx.timers.timers()[0].0.clone();
        // This new timer should be sooner.
        assert!(instant2 <= instant1);
        assert_eq!(bindings_ctx.trigger_next_timer(&mut core_ctx), Some(TIMER_ID));
        assert!(bindings_ctx.now() - start <= duration);
        assert_eq!(core_ctx.frames().len(), 2);
        // The frames are all reports.
        for (_, frame) in core_ctx.frames() {
            ensure_frame(&frame, 131, GROUP_ADDR, GROUP_ADDR);
            ensure_slice_addr(&frame, 8, 24, Ipv6::UNSPECIFIED_ADDRESS);
        }
    }

    #[test]
    fn test_mld_integration_last_send_leave() {
        run_with_many_seeds(|seed| {
            let FakeCtxImpl { mut core_ctx, mut bindings_ctx } = new_context();
            bindings_ctx.seed_rng(seed);

            assert_eq!(
                core_ctx.gmp_join_group(&mut bindings_ctx, &FakeDeviceId, GROUP_ADDR),
                GroupJoinResult::Joined(())
            );
            let now = bindings_ctx.now();

            core_ctx.state.gmp_state().timers.assert_range([(
                &gmp::v1::DelayedReportTimerId(GROUP_ADDR).into(),
                now..=(now + MLD_DEFAULT_UNSOLICITED_REPORT_INTERVAL),
            )]);
            // The initial unsolicited report.
            assert_eq!(core_ctx.frames().len(), 1);
            assert_eq!(bindings_ctx.trigger_next_timer(&mut core_ctx), Some(TIMER_ID));
            // The report after the delay.
            assert_eq!(core_ctx.frames().len(), 2);
            assert_eq!(
                core_ctx.gmp_leave_group(&mut bindings_ctx, &FakeDeviceId, GROUP_ADDR),
                GroupLeaveResult::Left(())
            );
            // Our leave message.
            assert_eq!(core_ctx.frames().len(), 3);
            // The first two messages should be reports.
            ensure_frame(&core_ctx.frames()[0].1, 131, GROUP_ADDR, GROUP_ADDR);
            ensure_slice_addr(&core_ctx.frames()[0].1, 8, 24, Ipv6::UNSPECIFIED_ADDRESS);
            ensure_frame(&core_ctx.frames()[1].1, 131, GROUP_ADDR, GROUP_ADDR);
            ensure_slice_addr(&core_ctx.frames()[1].1, 8, 24, Ipv6::UNSPECIFIED_ADDRESS);
            // The last one should be the done message whose destination is all
            // routers.
            ensure_frame(
                &core_ctx.frames()[2].1,
                132,
                Ipv6::ALL_ROUTERS_LINK_LOCAL_MULTICAST_ADDRESS,
                GROUP_ADDR,
            );
            ensure_slice_addr(&core_ctx.frames()[2].1, 8, 24, Ipv6::UNSPECIFIED_ADDRESS);
        });
    }

    #[test]
    fn test_mld_integration_not_last_does_not_send_leave() {
        run_with_many_seeds(|seed| {
            let FakeCtxImpl { mut core_ctx, mut bindings_ctx } = new_context();
            bindings_ctx.seed_rng(seed);

            assert_eq!(
                core_ctx.gmp_join_group(&mut bindings_ctx, &FakeDeviceId, GROUP_ADDR),
                GroupJoinResult::Joined(())
            );
            let now = bindings_ctx.now();
            core_ctx.state.gmp_state().timers.assert_range([(
                &gmp::v1::DelayedReportTimerId(GROUP_ADDR).into(),
                now..=(now + MLD_DEFAULT_UNSOLICITED_REPORT_INTERVAL),
            )]);
            assert_eq!(core_ctx.frames().len(), 1);
            receive_mld_report(&mut core_ctx, &mut bindings_ctx, GROUP_ADDR);
            bindings_ctx.timers.assert_no_timers_installed();
            // The report should be discarded because we have received from someone
            // else.
            assert_eq!(core_ctx.frames().len(), 1);
            assert_eq!(
                core_ctx.gmp_leave_group(&mut bindings_ctx, &FakeDeviceId, GROUP_ADDR),
                GroupLeaveResult::Left(())
            );
            // A leave message is not sent.
            assert_eq!(core_ctx.frames().len(), 1);
            // The frames are all reports.
            for (_, frame) in core_ctx.frames() {
                ensure_frame(&frame, 131, GROUP_ADDR, GROUP_ADDR);
                ensure_slice_addr(&frame, 8, 24, Ipv6::UNSPECIFIED_ADDRESS);
            }
        });
    }

    #[test]
    fn test_mld_with_link_local() {
        run_with_many_seeds(|seed| {
            let FakeCtxImpl { mut core_ctx, mut bindings_ctx } = new_context();
            bindings_ctx.seed_rng(seed);

            core_ctx.state.ipv6_link_local = Some(MY_MAC.to_ipv6_link_local().addr());
            assert_eq!(
                core_ctx.gmp_join_group(&mut bindings_ctx, &FakeDeviceId, GROUP_ADDR),
                GroupJoinResult::Joined(())
            );
            core_ctx
                .state
                .gmp_state()
                .timers
                .assert_top(&gmp::v1::DelayedReportTimerId(GROUP_ADDR).into(), &());
            assert_eq!(bindings_ctx.trigger_next_timer(&mut core_ctx), Some(TIMER_ID));
            for (_, frame) in core_ctx.frames() {
                ensure_frame(&frame, 131, GROUP_ADDR, GROUP_ADDR);
                ensure_slice_addr(&frame, 8, 24, MY_MAC.to_ipv6_link_local().addr().get());
            }
        });
    }

    #[test]
    fn test_skip_mld() {
        run_with_many_seeds(|seed| {
            // Test that we do not perform MLD for addresses that we're supposed
            // to skip or when MLD is disabled.
            let test = |FakeCtxImpl { mut core_ctx, mut bindings_ctx }, group| {
                core_ctx.state.ipv6_link_local = Some(MY_MAC.to_ipv6_link_local().addr());

                // Assert that no observable effects have taken place.
                let assert_no_effect =
                    |core_ctx: &FakeCoreCtxImpl, bindings_ctx: &FakeBindingsCtxImpl| {
                        bindings_ctx.timers.assert_no_timers_installed();
                        assert_empty(core_ctx.frames());
                    };

                assert_eq!(
                    core_ctx.gmp_join_group(&mut bindings_ctx, &FakeDeviceId, group),
                    GroupJoinResult::Joined(())
                );
                // We should join the group but left in the GMP's non-member
                // state.
                assert_gmp_state!(core_ctx, &group, NonMember);
                assert_no_effect(&core_ctx, &bindings_ctx);

                receive_mld_report(&mut core_ctx, &mut bindings_ctx, group);
                // We should have done no state transitions/work.
                assert_gmp_state!(core_ctx, &group, NonMember);
                assert_no_effect(&core_ctx, &bindings_ctx);

                receive_mld_query(&mut core_ctx, &mut bindings_ctx, Duration::from_secs(10), group);
                // We should have done no state transitions/work.
                assert_gmp_state!(core_ctx, &group, NonMember);
                assert_no_effect(&core_ctx, &bindings_ctx);

                assert_eq!(
                    core_ctx.gmp_leave_group(&mut bindings_ctx, &FakeDeviceId, group),
                    GroupLeaveResult::Left(())
                );
                // We should have left the group but not executed any `Actions`.
                assert!(core_ctx.state.groups().get(&group).is_none());
                assert_no_effect(&core_ctx, &bindings_ctx);
            };

            let new_ctx = || {
                let mut ctx = new_context();
                ctx.bindings_ctx.seed_rng(seed);
                ctx
            };

            // Test that we skip executing `Actions` for addresses we're
            // supposed to skip.
            test(new_ctx(), Ipv6::ALL_NODES_LINK_LOCAL_MULTICAST_ADDRESS);
            let mut bytes = Ipv6::MULTICAST_SUBNET.network().ipv6_bytes();
            // Manually set the "scope" field to 0.
            bytes[1] = bytes[1] & 0xF0;
            let reserved0 = MulticastAddr::new(Ipv6Addr::from_bytes(bytes)).unwrap();
            // Manually set the "scope" field to 1 (interface-local).
            bytes[1] = (bytes[1] & 0xF0) | 1;
            let iface_local = MulticastAddr::new(Ipv6Addr::from_bytes(bytes)).unwrap();
            test(new_ctx(), reserved0);
            test(new_ctx(), iface_local);

            // Test that we skip executing `Actions` when MLD is disabled on the
            // device.
            let mut ctx = new_ctx();
            ctx.core_ctx.state.mld_enabled = false;
            test(ctx, GROUP_ADDR);
        });
    }

    #[test]
    fn test_mld_integration_with_local_join_leave() {
        run_with_many_seeds(|seed| {
            // Simple MLD integration test to check that when we call top-level
            // multicast join and leave functions, MLD is performed.
            let FakeCtxImpl { mut core_ctx, mut bindings_ctx } = new_context();
            bindings_ctx.seed_rng(seed);

            assert_eq!(
                core_ctx.gmp_join_group(&mut bindings_ctx, &FakeDeviceId, GROUP_ADDR),
                GroupJoinResult::Joined(())
            );
            assert_gmp_state!(core_ctx, &GROUP_ADDR, Delaying);
            assert_eq!(core_ctx.frames().len(), 1);
            let now = bindings_ctx.now();
            let range = now..=(now + MLD_DEFAULT_UNSOLICITED_REPORT_INTERVAL);

            core_ctx
                .state
                .gmp_state()
                .timers
                .assert_range([(&gmp::v1::DelayedReportTimerId(GROUP_ADDR).into(), range.clone())]);
            let frame = &core_ctx.frames().last().unwrap().1;
            ensure_frame(frame, 131, GROUP_ADDR, GROUP_ADDR);
            ensure_slice_addr(frame, 8, 24, Ipv6::UNSPECIFIED_ADDRESS);

            assert_eq!(
                core_ctx.gmp_join_group(&mut bindings_ctx, &FakeDeviceId, GROUP_ADDR),
                GroupJoinResult::AlreadyMember
            );
            assert_gmp_state!(core_ctx, &GROUP_ADDR, Delaying);
            assert_eq!(core_ctx.frames().len(), 1);
            core_ctx
                .state
                .gmp_state()
                .timers
                .assert_range([(&gmp::v1::DelayedReportTimerId(GROUP_ADDR).into(), range.clone())]);

            assert_eq!(
                core_ctx.gmp_leave_group(&mut bindings_ctx, &FakeDeviceId, GROUP_ADDR),
                GroupLeaveResult::StillMember
            );
            assert_gmp_state!(core_ctx, &GROUP_ADDR, Delaying);
            assert_eq!(core_ctx.frames().len(), 1);

            core_ctx
                .state
                .gmp_state()
                .timers
                .assert_range([(&gmp::v1::DelayedReportTimerId(GROUP_ADDR).into(), range)]);

            assert_eq!(
                core_ctx.gmp_leave_group(&mut bindings_ctx, &FakeDeviceId, GROUP_ADDR),
                GroupLeaveResult::Left(())
            );
            assert_eq!(core_ctx.frames().len(), 2);
            bindings_ctx.timers.assert_no_timers_installed();
            let frame = &core_ctx.frames().last().unwrap().1;
            ensure_frame(frame, 132, Ipv6::ALL_ROUTERS_LINK_LOCAL_MULTICAST_ADDRESS, GROUP_ADDR);
            ensure_slice_addr(frame, 8, 24, Ipv6::UNSPECIFIED_ADDRESS);
        });
    }

    #[test]
    fn test_mld_enable_disable() {
        run_with_many_seeds(|seed| {
            let FakeCtxImpl { mut core_ctx, mut bindings_ctx } = new_context();
            bindings_ctx.seed_rng(seed);
            assert_eq!(core_ctx.take_frames(), []);

            // Should not perform MLD for the all-nodes address.
            //
            // As per RFC 3810 Section 6,
            //
            //   No MLD messages are ever sent regarding neither the link-scope,
            //   all-nodes multicast address, nor any multicast address of scope
            //   0 (reserved) or 1 (node-local).
            assert_eq!(
                core_ctx.gmp_join_group(
                    &mut bindings_ctx,
                    &FakeDeviceId,
                    Ipv6::ALL_NODES_LINK_LOCAL_MULTICAST_ADDRESS
                ),
                GroupJoinResult::Joined(())
            );
            assert_gmp_state!(core_ctx, &Ipv6::ALL_NODES_LINK_LOCAL_MULTICAST_ADDRESS, NonMember);
            assert_eq!(
                core_ctx.gmp_join_group(&mut bindings_ctx, &FakeDeviceId, GROUP_ADDR),
                GroupJoinResult::Joined(())
            );
            assert_gmp_state!(core_ctx, &GROUP_ADDR, Delaying);
            {
                let frames = core_ctx.take_frames();
                let (MldFrameMetadata { device: FakeDeviceId, dst_ip }, frame) =
                    assert_matches!(&frames[..], [x] => x);
                assert_eq!(dst_ip, &GROUP_ADDR);
                ensure_frame(
                    frame,
                    Icmpv6MessageType::MulticastListenerReport.into(),
                    GROUP_ADDR,
                    GROUP_ADDR,
                );
                ensure_slice_addr(frame, 8, 24, Ipv6::UNSPECIFIED_ADDRESS);
            }

            // Should do nothing.
            core_ctx.gmp_handle_maybe_enabled(&mut bindings_ctx, &FakeDeviceId);
            assert_gmp_state!(core_ctx, &Ipv6::ALL_NODES_LINK_LOCAL_MULTICAST_ADDRESS, NonMember);
            assert_gmp_state!(core_ctx, &GROUP_ADDR, Delaying);
            assert_eq!(core_ctx.take_frames(), []);

            // Should send done message.
            core_ctx.gmp_handle_disabled(&mut bindings_ctx, &FakeDeviceId);
            assert_gmp_state!(core_ctx, &Ipv6::ALL_NODES_LINK_LOCAL_MULTICAST_ADDRESS, NonMember);
            assert_gmp_state!(core_ctx, &GROUP_ADDR, NonMember);
            {
                let frames = core_ctx.take_frames();
                let (MldFrameMetadata { device: FakeDeviceId, dst_ip }, frame) =
                    assert_matches!(&frames[..], [x] => x);
                assert_eq!(dst_ip, &Ipv6::ALL_ROUTERS_LINK_LOCAL_MULTICAST_ADDRESS);
                ensure_frame(
                    frame,
                    Icmpv6MessageType::MulticastListenerDone.into(),
                    Ipv6::ALL_ROUTERS_LINK_LOCAL_MULTICAST_ADDRESS,
                    GROUP_ADDR,
                );
                ensure_slice_addr(frame, 8, 24, Ipv6::UNSPECIFIED_ADDRESS);
            }

            // Should do nothing.
            core_ctx.gmp_handle_disabled(&mut bindings_ctx, &FakeDeviceId);
            assert_gmp_state!(core_ctx, &Ipv6::ALL_NODES_LINK_LOCAL_MULTICAST_ADDRESS, NonMember);
            assert_gmp_state!(core_ctx, &GROUP_ADDR, NonMember);
            assert_eq!(core_ctx.take_frames(), []);

            // Should send report message.
            core_ctx.gmp_handle_maybe_enabled(&mut bindings_ctx, &FakeDeviceId);
            assert_gmp_state!(core_ctx, &Ipv6::ALL_NODES_LINK_LOCAL_MULTICAST_ADDRESS, NonMember);
            assert_gmp_state!(core_ctx, &GROUP_ADDR, Delaying);
            let frames = core_ctx.take_frames();
            let (MldFrameMetadata { device: FakeDeviceId, dst_ip }, frame) =
                assert_matches!(&frames[..], [x] => x);
            assert_eq!(dst_ip, &GROUP_ADDR);
            ensure_frame(
                frame,
                Icmpv6MessageType::MulticastListenerReport.into(),
                GROUP_ADDR,
                GROUP_ADDR,
            );
            ensure_slice_addr(frame, 8, 24, Ipv6::UNSPECIFIED_ADDRESS);
        });
    }
}
