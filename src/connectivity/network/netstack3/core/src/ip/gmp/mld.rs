// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Multicast Listener Discovery (MLD).
//!
//! MLD is derived from version 2 of IPv4's Internet Group Management Protocol,
//! IGMPv2. One important difference to note is that MLD uses ICMPv6 (IP
//! Protocol 58) message types, rather than IGMP (IP Protocol 2) message types.

use core::{convert::Infallible as Never, time::Duration};

use net_types::{
    ip::{Ip, Ipv6, Ipv6Addr, Ipv6ReservedScope, Ipv6Scope, Ipv6SourceAddr},
    LinkLocalUnicastAddr, MulticastAddr, ScopeableAddress, SpecifiedAddr, Witness,
};
use packet::{serialize::Serializer, InnerPacketBuilder};
use packet_formats::{
    icmp::{
        mld::{
            IcmpMldv1MessageType, MldPacket, Mldv1Body, Mldv1MessageBuilder, MulticastListenerDone,
            MulticastListenerReport,
        },
        IcmpPacketBuilder, IcmpUnusedCode,
    },
    ip::Ipv6Proto,
    ipv6::{
        ext_hdrs::{ExtensionHeaderOptionAction, HopByHopOption, HopByHopOptionData},
        Ipv6PacketBuilder, Ipv6PacketBuilderWithHbhOptions,
    },
    utils::NonZeroDuration,
};
use thiserror::Error;
use tracing::{debug, error};
use zerocopy::ByteSlice;

use crate::{
    context::{RngContext, TimerContext, TimerHandler},
    device::{AnyDevice, DeviceIdContext},
    filter::MaybeTransportPacket,
    ip::{
        device::IpDeviceSendContext,
        gmp::{
            gmp_handle_timer, handle_query_message, handle_report_message, GmpContext,
            GmpDelayedReportTimerId, GmpMessage, GmpMessageType, GmpState, GmpStateContext,
            GmpStateMachine, GmpTypeLayout, IpExt, MulticastGroupSet, ProtocolSpecific,
            QueryTarget,
        },
        IpLayerHandler,
    },
    Instant,
};

/// The bindings execution context for MLD.
pub(crate) trait MldBindingsContext<DeviceId>:
    RngContext + TimerContext<MldDelayedReportTimerId<DeviceId>>
{
}
impl<DeviceId, BC: RngContext + TimerContext<MldDelayedReportTimerId<DeviceId>>>
    MldBindingsContext<DeviceId> for BC
{
}

/// Provides immutable access to MLD state.
pub(crate) trait MldStateContext<BC: MldBindingsContext<Self::DeviceId>>:
    DeviceIdContext<AnyDevice>
{
    /// Calls the function with an immutable reference to the device's MLD
    /// state.
    fn with_mld_state<O, F: FnOnce(&MulticastGroupSet<Ipv6Addr, MldGroupState<BC::Instant>>) -> O>(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> O;
}

/// The execution context for the Multicast Listener Discovery (MLD) protocol.
pub(crate) trait MldContext<BC: MldBindingsContext<Self::DeviceId>>:
    DeviceIdContext<AnyDevice> + IpDeviceSendContext<Ipv6, BC> + IpLayerHandler<Ipv6, BC>
{
    /// Calls the function with a mutable reference to the device's MLD state
    /// and whether or not MLD is enabled for the `device`.
    fn with_mld_state_mut<O, F: FnOnce(GmpState<'_, Ipv6Addr, MldGroupState<BC::Instant>>) -> O>(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> O;

    /// Gets the IPv6 link local address on `device`.
    fn get_ipv6_link_local_addr(
        &mut self,
        device: &Self::DeviceId,
    ) -> Option<LinkLocalUnicastAddr<Ipv6Addr>>;
}

/// A handler for incoming MLD packets.
///
/// A blanket implementation is provided for all `C: MldContext`.
pub trait MldPacketHandler<BC, DeviceId> {
    /// Receive an MLD packet.
    fn receive_mld_packet<B: ByteSlice>(
        &mut self,
        bindings_ctx: &mut BC,
        device: &DeviceId,
        src_ip: Ipv6SourceAddr,
        dst_ip: SpecifiedAddr<Ipv6Addr>,
        packet: MldPacket<B>,
    );
}

impl<BC: MldBindingsContext<CC::DeviceId>, CC: MldContext<BC>> MldPacketHandler<BC, CC::DeviceId>
    for CC
{
    fn receive_mld_packet<B: ByteSlice>(
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
                let addr = body.group_addr();
                SpecifiedAddr::new(addr)
                    .map_or(Some(QueryTarget::Unspecified), |addr| {
                        MulticastAddr::new(addr.get()).map(QueryTarget::Specified)
                    })
                    .map_or(Err(MldError::NotAMember { addr }), |group_addr| {
                        handle_query_message(
                            self,
                            bindings_ctx,
                            device,
                            group_addr,
                            body.max_response_delay(),
                        )
                    })
            }
            MldPacket::MulticastListenerReport(msg) => {
                let addr = msg.body().group_addr();
                MulticastAddr::new(msg.body().group_addr())
                    .map_or(Err(MldError::NotAMember { addr }), |group_addr| {
                        handle_report_message(self, bindings_ctx, device, group_addr)
                    })
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
            error!("Error occurred when handling MLD message: {}", e);
        }
    }
}

impl<B: ByteSlice> GmpMessage<Ipv6> for Mldv1Body<B> {
    fn group_addr(&self) -> Ipv6Addr {
        self.group_addr
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

impl<BC: MldBindingsContext<CC::DeviceId>, CC: DeviceIdContext<AnyDevice>> GmpTypeLayout<Ipv6, BC>
    for CC
{
    type ProtocolSpecific = MldProtocolSpecific;
    type GroupState = MldGroupState<BC::Instant>;
}

impl<BC: MldBindingsContext<CC::DeviceId>, CC: MldStateContext<BC>> GmpStateContext<Ipv6, BC>
    for CC
{
    fn with_gmp_state<
        O,
        F: FnOnce(&MulticastGroupSet<Ipv6Addr, MldGroupState<BC::Instant>>) -> O,
    >(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> O {
        self.with_mld_state(device, cb)
    }
}

impl<BC: MldBindingsContext<CC::DeviceId>, CC: MldContext<BC>> GmpContext<Ipv6, BC> for CC {
    type Err = MldError;

    fn with_gmp_state_mut<O, F: FnOnce(GmpState<'_, Ipv6Addr, MldGroupState<BC::Instant>>) -> O>(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> O {
        self.with_mld_state_mut(device, cb)
    }

    fn send_message(
        &mut self,
        bindings_ctx: &mut BC,
        device: &Self::DeviceId,
        group_addr: MulticastAddr<Ipv6Addr>,
        msg_type: GmpMessageType<MldProtocolSpecific>,
    ) {
        let result = match msg_type {
            GmpMessageType::Report(MldProtocolSpecific) => send_mld_packet::<_, _, _>(
                self,
                bindings_ctx,
                device,
                group_addr,
                MulticastListenerReport,
                group_addr,
                (),
            ),
            GmpMessageType::Leave => send_mld_packet::<_, _, _>(
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

    fn run_actions(&mut self, _bindings_ctx: &mut BC, device: &CC::DeviceId, actions: Never) {
        unreachable!("actions ({actions:?} should not be constructable; device = {device:?}")
    }

    fn not_a_member_err(addr: Ipv6Addr) -> Self::Err {
        Self::Err::NotAMember { addr }
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

pub(crate) type MldResult<T> = Result<T, MldError>;

#[derive(PartialEq, Eq, Clone, Copy, Default, Debug)]
pub(crate) struct MldProtocolSpecific;

#[derive(Debug)]
pub(crate) struct MldConfig {
    unsolicited_report_interval: Duration,
    send_leave_anyway: bool,
}

/// The default value for `unsolicited_report_interval` [RFC 2710 Section 7.10]
///
/// [RFC 2710 Section 7.10]: https://tools.ietf.org/html/rfc2710#section-7.10
const DEFAULT_UNSOLICITED_REPORT_INTERVAL: Duration = Duration::from_secs(10);

impl Default for MldConfig {
    fn default() -> Self {
        MldConfig {
            unsolicited_report_interval: DEFAULT_UNSOLICITED_REPORT_INTERVAL,
            send_leave_anyway: false,
        }
    }
}

impl ProtocolSpecific for MldProtocolSpecific {
    type Actions = Never;
    type Config = MldConfig;

    fn cfg_unsolicited_report_interval(cfg: &Self::Config) -> Duration {
        cfg.unsolicited_report_interval
    }

    fn cfg_send_leave_anyway(cfg: &Self::Config) -> bool {
        cfg.send_leave_anyway
    }

    fn get_max_resp_time(resp_time: Duration) -> Option<NonZeroDuration> {
        NonZeroDuration::new(resp_time)
    }

    fn do_query_received_specific(
        _cfg: &Self::Config,
        _max_resp_time: Duration,
        old: Self,
    ) -> (Self, Option<Never>) {
        (old, None)
    }
}

/// The state on a multicast address.
#[cfg_attr(test, derive(Debug))]
pub struct MldGroupState<I: Instant>(GmpStateMachine<I, MldProtocolSpecific>);

impl<I: Instant> From<GmpStateMachine<I, MldProtocolSpecific>> for MldGroupState<I> {
    fn from(state: GmpStateMachine<I, MldProtocolSpecific>) -> MldGroupState<I> {
        MldGroupState(state)
    }
}

impl<I: Instant> From<MldGroupState<I>> for GmpStateMachine<I, MldProtocolSpecific> {
    fn from(MldGroupState(state): MldGroupState<I>) -> GmpStateMachine<I, MldProtocolSpecific> {
        state
    }
}

impl<I: Instant> AsMut<GmpStateMachine<I, MldProtocolSpecific>> for MldGroupState<I> {
    fn as_mut(&mut self) -> &mut GmpStateMachine<I, MldProtocolSpecific> {
        let Self(s) = self;
        s
    }
}

/// An MLD timer to delay the sending of a report.
#[derive(PartialEq, Eq, Clone, Copy, Debug, Hash)]
pub struct MldDelayedReportTimerId<DeviceId>(
    pub(crate) GmpDelayedReportTimerId<Ipv6Addr, DeviceId>,
);

impl<DeviceId> MldDelayedReportTimerId<DeviceId> {
    pub(crate) fn device_id(&self) -> &DeviceId {
        let Self(this) = self;
        this.device_id()
    }
}

impl<DeviceId> From<GmpDelayedReportTimerId<Ipv6Addr, DeviceId>>
    for MldDelayedReportTimerId<DeviceId>
{
    fn from(id: GmpDelayedReportTimerId<Ipv6Addr, DeviceId>) -> MldDelayedReportTimerId<DeviceId> {
        MldDelayedReportTimerId(id)
    }
}

impl_timer_context!(
    DeviceId,
    MldDelayedReportTimerId<DeviceId>,
    GmpDelayedReportTimerId::<Ipv6Addr, DeviceId>,
    MldDelayedReportTimerId(id),
    id
);

impl<BC: MldBindingsContext<CC::DeviceId>, CC: MldContext<BC>>
    TimerHandler<BC, MldDelayedReportTimerId<CC::DeviceId>> for CC
{
    fn handle_timer(
        &mut self,
        bindings_ctx: &mut BC,
        timer: MldDelayedReportTimerId<CC::DeviceId>,
    ) {
        let MldDelayedReportTimerId(id) = timer;
        gmp_handle_timer(self, bindings_ctx, id);
    }
}

/// Send an MLD packet.
///
/// The MLD packet being sent should have its `hop_limit` to be 1 and a
/// `RouterAlert` option in its Hop-by-Hop Options extensions header.
fn send_mld_packet<
    BC: MldBindingsContext<CC::DeviceId>,
    CC: MldContext<BC>,
    M: IcmpMldv1MessageType + MaybeTransportPacket,
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

    crate::ip::IpLayerHandler::send_ip_frame(
        core_ctx,
        bindings_ctx,
        &device,
        dst_ip.into_specified(),
        body,
        None,
    )
    .map_err(|_| MldError::SendFailure { addr: group_addr.into() })
}

#[cfg(test)]
mod tests {

    use assert_matches::assert_matches;
    use net_types::{ethernet::Mac, ip::Ip as _};
    use packet::{BufferMut, ParseBuffer};
    use packet_formats::{
        ethernet::EthernetFrameLengthCheck,
        icmp::{mld::MulticastListenerQuery, IcmpParseArgs, Icmpv6MessageType, Icmpv6Packet},
        testutil::parse_icmp_packet_in_ip_packet_in_ethernet_frame,
    };

    use super::*;
    use crate::{
        context::{
            testutil::{FakeInstant, FakeTimerCtxExt},
            InstantContext as _, SendFrameContext,
        },
        device::{
            ethernet::{EthernetCreationProperties, EthernetLinkDevice},
            testutil::FakeDeviceId,
            DeviceId,
        },
        filter::ProofOfEgressCheck,
        ip::{
            device::{
                config::{IpDeviceConfigurationUpdate, Ipv6DeviceConfigurationUpdate},
                slaac::SlaacConfiguration,
                Ipv6DeviceTimerId,
            },
            gmp::{
                GmpHandler as _, GroupJoinResult, GroupLeaveResult, MemberState,
                QueryReceivedActions, QueryReceivedGenericAction,
            },
            testutil::FakeIpDeviceIdCtx,
            types::IpTypesIpExt,
            IpLayerPacketMetadata,
        },
        state::StackStateBuilder,
        testutil::{
            assert_empty, new_rng, run_with_many_seeds, FakeEventDispatcherConfig, TestIpExt as _,
            DEFAULT_INTERFACE_METRIC, IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
        },
        time::TimerIdInner,
        TimerId,
    };

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
        groups: MulticastGroupSet<Ipv6Addr, MldGroupState<FakeInstant>>,
        mld_enabled: bool,
        ipv6_link_local: Option<LinkLocalUnicastAddr<Ipv6Addr>>,
        ip_device_id_ctx: FakeIpDeviceIdCtx<FakeDeviceId>,
    }

    impl Default for FakeMldCtx {
        fn default() -> FakeMldCtx {
            FakeMldCtx {
                groups: MulticastGroupSet::default(),
                mld_enabled: true,
                ipv6_link_local: None,
                ip_device_id_ctx: Default::default(),
            }
        }
    }

    impl AsRef<FakeIpDeviceIdCtx<FakeDeviceId>> for FakeMldCtx {
        fn as_ref(&self) -> &FakeIpDeviceIdCtx<FakeDeviceId> {
            &self.ip_device_id_ctx
        }
    }

    type FakeCtxImpl = crate::context::testutil::FakeCtx<
        FakeMldCtx,
        MldDelayedReportTimerId<FakeDeviceId>,
        MldFrameMetadata<FakeDeviceId>,
        (),
        FakeDeviceId,
        (),
    >;
    type FakeCoreCtxImpl = crate::context::testutil::FakeCoreCtx<
        FakeMldCtx,
        MldFrameMetadata<FakeDeviceId>,
        FakeDeviceId,
    >;
    type FakeBindingsCtxImpl = crate::context::testutil::FakeBindingsCtx<
        MldDelayedReportTimerId<FakeDeviceId>,
        (),
        (),
        (),
    >;

    impl MldStateContext<FakeBindingsCtxImpl> for FakeCoreCtxImpl {
        fn with_mld_state<
            O,
            F: FnOnce(&MulticastGroupSet<Ipv6Addr, MldGroupState<FakeInstant>>) -> O,
        >(
            &mut self,
            &FakeDeviceId: &FakeDeviceId,
            cb: F,
        ) -> O {
            let FakeMldCtx { groups, mld_enabled: _, ipv6_link_local: _, ip_device_id_ctx: _ } =
                self.get_ref();
            cb(groups)
        }
    }

    impl MldContext<FakeBindingsCtxImpl> for FakeCoreCtxImpl {
        fn with_mld_state_mut<
            O,
            F: FnOnce(GmpState<'_, Ipv6Addr, MldGroupState<FakeInstant>>) -> O,
        >(
            &mut self,
            &FakeDeviceId: &FakeDeviceId,
            cb: F,
        ) -> O {
            let FakeMldCtx { groups, mld_enabled, ipv6_link_local: _, ip_device_id_ctx: _ } =
                self.get_mut();
            cb(GmpState { enabled: *mld_enabled, groups })
        }

        fn get_ipv6_link_local_addr(
            &mut self,
            _device: &FakeDeviceId,
        ) -> Option<LinkLocalUnicastAddr<Ipv6Addr>> {
            self.get_ref().ipv6_link_local
        }
    }

    impl IpLayerHandler<Ipv6, FakeBindingsCtxImpl> for FakeCoreCtxImpl {
        fn send_ip_packet_from_device<S>(
            &mut self,
            _bindings_ctx: &mut FakeBindingsCtxImpl,
            _meta: crate::ip::SendIpPacketMeta<
                Ipv6,
                &Self::DeviceId,
                Option<SpecifiedAddr<<Ipv6 as Ip>::Addr>>,
            >,
            _body: S,
        ) -> Result<(), S>
        where
            S: Serializer + MaybeTransportPacket,
            S::Buffer: BufferMut,
        {
            unimplemented!();
        }

        fn send_ip_frame<S>(
            &mut self,
            bindings_ctx: &mut FakeBindingsCtxImpl,
            device: &Self::DeviceId,
            next_hop: SpecifiedAddr<<Ipv6 as Ip>::Addr>,
            body: S,
            broadcast: Option<<Ipv6 as IpTypesIpExt>::BroadcastMarker>,
        ) -> Result<(), S>
        where
            S: Serializer + netstack3_filter::IpPacket<Ipv6>,
            S::Buffer: BufferMut,
        {
            crate::ip::send_ip_frame(
                self,
                bindings_ctx,
                device,
                next_hop,
                body,
                broadcast,
                IpLayerPacketMetadata::<Ipv6>::default(),
            )
        }
    }

    impl IpDeviceSendContext<Ipv6, FakeBindingsCtxImpl> for FakeCoreCtxImpl {
        fn send_ip_frame<S>(
            &mut self,
            bindings_ctx: &mut FakeBindingsCtxImpl,
            device_id: &Self::DeviceId,
            local_addr: SpecifiedAddr<Ipv6Addr>,
            body: S,
            _broadcast: Option<Never>,
            ProofOfEgressCheck { .. }: ProofOfEgressCheck,
        ) -> Result<(), S>
        where
            S: Serializer,
            S::Buffer: BufferMut,
        {
            self.send_frame(
                bindings_ctx,
                MldFrameMetadata::new(
                    device_id.clone(),
                    MulticastAddr::new(local_addr.get()).expect("addr should be multicast"),
                ),
                body,
            )
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
            let (mut s, _actions) = GmpStateMachine::<_, MldProtocolSpecific>::join_group(
                &mut rng,
                FakeInstant::default(),
                false,
            );
            assert_eq!(
                s.query_received(&mut rng, Duration::from_secs(0), FakeInstant::default()),
                QueryReceivedActions {
                    generic: Some(QueryReceivedGenericAction::StopTimerAndSendReport(
                        MldProtocolSpecific
                    )),
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
    const GROUP_ADDR: MulticastAddr<Ipv6Addr> =
        unsafe { MulticastAddr::new_unchecked(Ipv6Addr::new([0xff02, 0, 0, 0, 0, 0, 0, 3])) };
    const TIMER_ID: MldDelayedReportTimerId<FakeDeviceId> =
        MldDelayedReportTimerId(GmpDelayedReportTimerId {
            device: FakeDeviceId,
            group_addr: GROUP_ADDR,
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
            let FakeCtxImpl { mut core_ctx, mut bindings_ctx } =
                FakeCtxImpl::with_core_ctx(FakeCoreCtxImpl::default());
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
            let FakeCtxImpl { mut core_ctx, mut bindings_ctx } =
                FakeCtxImpl::with_core_ctx(FakeCoreCtxImpl::default());
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
            let FakeCtxImpl { mut core_ctx, mut bindings_ctx } =
                FakeCtxImpl::with_core_ctx(FakeCoreCtxImpl::default());
            bindings_ctx.seed_rng(seed);

            assert_eq!(
                core_ctx.gmp_join_group(&mut bindings_ctx, &FakeDeviceId, GROUP_ADDR),
                GroupJoinResult::Joined(())
            );
            assert_eq!(core_ctx.frames().len(), 1);

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
            let MldGroupState(group_state) = core_ctx.get_ref().groups.get(&GROUP_ADDR).unwrap();
            match group_state.get_inner() {
                MemberState::Delaying(_) => {}
                _ => panic!("Wrong State!"),
            }

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
            let FakeCtxImpl { mut core_ctx, mut bindings_ctx } =
                FakeCtxImpl::with_core_ctx(FakeCoreCtxImpl::default());
            bindings_ctx.seed_rng(seed);

            assert_eq!(
                core_ctx.gmp_join_group(&mut bindings_ctx, &FakeDeviceId, GROUP_ADDR),
                GroupJoinResult::Joined(())
            );
            assert_eq!(core_ctx.frames().len(), 1);

            assert_eq!(bindings_ctx.trigger_next_timer(&mut core_ctx), Some(TIMER_ID));
            assert_eq!(core_ctx.frames().len(), 2);

            receive_mld_query(&mut core_ctx, &mut bindings_ctx, Duration::from_secs(0), GROUP_ADDR);

            // Since it is an immediate query, we will send a report immediately
            // and turn into Idle state again.
            let MldGroupState(group_state) = core_ctx.get_ref().groups.get(&GROUP_ADDR).unwrap();
            match group_state.get_inner() {
                MemberState::Idle(_) => {}
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
        let FakeCtxImpl { mut core_ctx, mut bindings_ctx } =
            FakeCtxImpl::with_core_ctx(FakeCoreCtxImpl::default());
        // This seed was carefully chosen to produce a substantial duration
        // value below.
        bindings_ctx.seed_rng(123456);
        assert_eq!(
            core_ctx.gmp_join_group(&mut bindings_ctx, &FakeDeviceId, GROUP_ADDR),
            GroupJoinResult::Joined(())
        );
        bindings_ctx.timer_ctx().assert_timers_installed([(
            TIMER_ID,
            FakeInstant::from(Duration::from_micros(590_354)),
        )]);
        let instant1 = bindings_ctx.timer_ctx().timers()[0].0.clone();
        let start = bindings_ctx.now();
        let duration = instant1 - start;

        receive_mld_query(&mut core_ctx, &mut bindings_ctx, duration, GROUP_ADDR);
        assert_eq!(core_ctx.frames().len(), 1);
        bindings_ctx.timer_ctx().assert_timers_installed([(
            TIMER_ID,
            FakeInstant::from(Duration::from_micros(34_751)),
        )]);
        let instant2 = bindings_ctx.timer_ctx().timers()[0].0.clone();
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
            let FakeCtxImpl { mut core_ctx, mut bindings_ctx } =
                FakeCtxImpl::with_core_ctx(FakeCoreCtxImpl::default());
            bindings_ctx.seed_rng(seed);

            assert_eq!(
                core_ctx.gmp_join_group(&mut bindings_ctx, &FakeDeviceId, GROUP_ADDR),
                GroupJoinResult::Joined(())
            );
            let now = bindings_ctx.now();
            bindings_ctx.timer_ctx().assert_timers_installed_range([(
                TIMER_ID,
                now..=(now + DEFAULT_UNSOLICITED_REPORT_INTERVAL),
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
            let FakeCtxImpl { mut core_ctx, mut bindings_ctx } =
                FakeCtxImpl::with_core_ctx(FakeCoreCtxImpl::default());
            bindings_ctx.seed_rng(seed);

            assert_eq!(
                core_ctx.gmp_join_group(&mut bindings_ctx, &FakeDeviceId, GROUP_ADDR),
                GroupJoinResult::Joined(())
            );
            let now = bindings_ctx.now();
            bindings_ctx.timer_ctx().assert_timers_installed_range([(
                TIMER_ID,
                now..=(now + DEFAULT_UNSOLICITED_REPORT_INTERVAL),
            )]);
            assert_eq!(core_ctx.frames().len(), 1);
            receive_mld_report(&mut core_ctx, &mut bindings_ctx, GROUP_ADDR);
            bindings_ctx.timer_ctx().assert_no_timers_installed();
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
            let FakeCtxImpl { mut core_ctx, mut bindings_ctx } =
                FakeCtxImpl::with_core_ctx(FakeCoreCtxImpl::default());
            bindings_ctx.seed_rng(seed);

            core_ctx.get_mut().ipv6_link_local = Some(MY_MAC.to_ipv6_link_local().addr());
            assert_eq!(
                core_ctx.gmp_join_group(&mut bindings_ctx, &FakeDeviceId, GROUP_ADDR),
                GroupJoinResult::Joined(())
            );
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
                core_ctx.get_mut().ipv6_link_local = Some(MY_MAC.to_ipv6_link_local().addr());

                // Assert that no observable effects have taken place.
                let assert_no_effect =
                    |core_ctx: &FakeCoreCtxImpl, bindings_ctx: &FakeBindingsCtxImpl| {
                        bindings_ctx.timer_ctx().assert_no_timers_installed();
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
                assert!(core_ctx.get_ref().groups.get(&group).is_none());
                assert_no_effect(&core_ctx, &bindings_ctx);
            };

            let new_ctx = || {
                let mut ctx = FakeCtxImpl::with_core_ctx(FakeCoreCtxImpl::default());
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
            ctx.core_ctx.get_mut().mld_enabled = false;
            test(ctx, GROUP_ADDR);
        });
    }

    #[test]
    fn test_mld_integration_with_local_join_leave() {
        run_with_many_seeds(|seed| {
            // Simple MLD integration test to check that when we call top-level
            // multicast join and leave functions, MLD is performed.
            let FakeCtxImpl { mut core_ctx, mut bindings_ctx } =
                FakeCtxImpl::with_core_ctx(FakeCoreCtxImpl::default());
            bindings_ctx.seed_rng(seed);

            assert_eq!(
                core_ctx.gmp_join_group(&mut bindings_ctx, &FakeDeviceId, GROUP_ADDR),
                GroupJoinResult::Joined(())
            );
            assert_gmp_state!(core_ctx, &GROUP_ADDR, Delaying);
            assert_eq!(core_ctx.frames().len(), 1);
            let now = bindings_ctx.now();
            let range = now..=(now + DEFAULT_UNSOLICITED_REPORT_INTERVAL);
            bindings_ctx.timer_ctx().assert_timers_installed_range([(TIMER_ID, range.clone())]);
            let frame = &core_ctx.frames().last().unwrap().1;
            ensure_frame(frame, 131, GROUP_ADDR, GROUP_ADDR);
            ensure_slice_addr(frame, 8, 24, Ipv6::UNSPECIFIED_ADDRESS);

            assert_eq!(
                core_ctx.gmp_join_group(&mut bindings_ctx, &FakeDeviceId, GROUP_ADDR),
                GroupJoinResult::AlreadyMember
            );
            assert_gmp_state!(core_ctx, &GROUP_ADDR, Delaying);
            assert_eq!(core_ctx.frames().len(), 1);
            bindings_ctx.timer_ctx().assert_timers_installed_range([(TIMER_ID, range.clone())]);

            assert_eq!(
                core_ctx.gmp_leave_group(&mut bindings_ctx, &FakeDeviceId, GROUP_ADDR),
                GroupLeaveResult::StillMember
            );
            assert_gmp_state!(core_ctx, &GROUP_ADDR, Delaying);
            assert_eq!(core_ctx.frames().len(), 1);
            bindings_ctx.timer_ctx().assert_timers_installed_range([(TIMER_ID, range)]);

            assert_eq!(
                core_ctx.gmp_leave_group(&mut bindings_ctx, &FakeDeviceId, GROUP_ADDR),
                GroupLeaveResult::Left(())
            );
            assert_eq!(core_ctx.frames().len(), 2);
            bindings_ctx.timer_ctx().assert_no_timers_installed();
            let frame = &core_ctx.frames().last().unwrap().1;
            ensure_frame(frame, 132, Ipv6::ALL_ROUTERS_LINK_LOCAL_MULTICAST_ADDRESS, GROUP_ADDR);
            ensure_slice_addr(frame, 8, 24, Ipv6::UNSPECIFIED_ADDRESS);
        });
    }

    #[test]
    fn test_mld_enable_disable() {
        run_with_many_seeds(|seed| {
            let FakeCtxImpl { mut core_ctx, mut bindings_ctx } =
                FakeCtxImpl::with_core_ctx(FakeCoreCtxImpl::default());
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

    #[test]
    fn test_mld_enable_disable_integration() {
        let FakeEventDispatcherConfig {
            local_mac,
            remote_mac: _,
            local_ip: _,
            remote_ip: _,
            subnet: _,
        } = Ipv6::FAKE_CONFIG;

        let mut ctx = crate::testutil::FakeCtx::new_with_builder(StackStateBuilder::default());

        let eth_device_id =
            ctx.core_api().device::<EthernetLinkDevice>().add_device_with_default_state(
                EthernetCreationProperties {
                    mac: local_mac,
                    max_frame_size: IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
                },
                DEFAULT_INTERFACE_METRIC,
            );
        let device_id: DeviceId<_> = eth_device_id.clone().into();

        let now = ctx.bindings_ctx.now();
        let ll_addr = local_mac.to_ipv6_link_local().addr();
        let snmc_addr = ll_addr.to_solicited_node_address();
        let snmc_timer_id = TimerId(TimerIdInner::Ipv6Device(
            Ipv6DeviceTimerId::Mld(MldDelayedReportTimerId(GmpDelayedReportTimerId {
                device: device_id.clone(),
                group_addr: snmc_addr,
            }))
            .into(),
        ));
        let range = now..=(now + DEFAULT_UNSOLICITED_REPORT_INTERVAL);
        struct TestConfig {
            ip_enabled: bool,
            gmp_enabled: bool,
        }
        let set_config = |ctx: &mut crate::testutil::FakeCtx,
                          TestConfig { ip_enabled, gmp_enabled }| {
            let _: Ipv6DeviceConfigurationUpdate = ctx
                .core_api()
                .device_ip::<Ipv6>()
                .update_configuration(
                    &device_id,
                    Ipv6DeviceConfigurationUpdate {
                        // TODO(https://fxbug.dev/42180878): Make sure that DAD resolving
                        // for a link-local address results in reports sent with a
                        // specified source address.
                        dad_transmits: Some(None),
                        max_router_solicitations: Some(None),
                        // Auto-generate a link-local address.
                        slaac_config: Some(SlaacConfiguration {
                            enable_stable_addresses: true,
                            ..Default::default()
                        }),
                        ip_config: IpDeviceConfigurationUpdate {
                            ip_enabled: Some(ip_enabled),
                            gmp_enabled: Some(gmp_enabled),
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                )
                .unwrap();
        };
        let check_sent_report = |bindings_ctx: &mut crate::testutil::FakeBindingsCtx,
                                 specified_source: bool| {
            let frames = bindings_ctx.take_ethernet_frames();
            let (egress_device, frame) = assert_matches!(&frames[..], [x] => x);
            assert_eq!(egress_device, &eth_device_id);
            let (src_mac, dst_mac, src_ip, dst_ip, ttl, _message, code) =
                parse_icmp_packet_in_ip_packet_in_ethernet_frame::<
                    Ipv6,
                    _,
                    MulticastListenerReport,
                    _,
                >(frame, EthernetFrameLengthCheck::NoCheck, |icmp| {
                    assert_eq!(icmp.body().group_addr, snmc_addr.get());
                })
                .unwrap();
            assert_eq!(src_mac, local_mac.get());
            assert_eq!(dst_mac, Mac::from(&snmc_addr));
            assert_eq!(
                src_ip,
                if specified_source { ll_addr.get() } else { Ipv6::UNSPECIFIED_ADDRESS }
            );
            assert_eq!(dst_ip, snmc_addr.get());
            assert_eq!(ttl, 1);
            assert_eq!(code, IcmpUnusedCode);
            assert_eq!(dst_ip, snmc_addr.get());
            assert_eq!(ttl, 1);
            assert_eq!(code, IcmpUnusedCode);
        };
        let check_sent_done = |bindings_ctx: &mut crate::testutil::FakeBindingsCtx,
                               specified_source: bool| {
            let frames = bindings_ctx.take_ethernet_frames();
            let (egress_device, frame) = assert_matches!(&frames[..], [x] => x);
            assert_eq!(egress_device, &eth_device_id);
            let (src_mac, dst_mac, src_ip, dst_ip, ttl, _message, code) =
                        parse_icmp_packet_in_ip_packet_in_ethernet_frame::<
                            Ipv6,
                            _,
                            MulticastListenerDone,
                            _,
                        >(frame, EthernetFrameLengthCheck::NoCheck, |icmp| {
                            assert_eq!(icmp.body().group_addr, snmc_addr.get());
                        }).unwrap();
            assert_eq!(src_mac, local_mac.get());
            assert_eq!(dst_mac, Mac::from(&Ipv6::ALL_ROUTERS_LINK_LOCAL_MULTICAST_ADDRESS));
            assert_eq!(
                src_ip,
                if specified_source { ll_addr.get() } else { Ipv6::UNSPECIFIED_ADDRESS }
            );
            assert_eq!(dst_ip, Ipv6::ALL_ROUTERS_LINK_LOCAL_MULTICAST_ADDRESS.get());
            assert_eq!(ttl, 1);
            assert_eq!(code, IcmpUnusedCode);
        };

        // Enable IPv6 and MLD.
        //
        // MLD should be performed for the auto-generated link-local address's
        // solicited-node multicast address.
        set_config(&mut ctx, TestConfig { ip_enabled: true, gmp_enabled: true });
        ctx.bindings_ctx
            .timer_ctx()
            .assert_timers_installed_range([(snmc_timer_id.clone(), range.clone())]);
        check_sent_report(&mut ctx.bindings_ctx, false);

        // Disable MLD.
        set_config(&mut ctx, TestConfig { ip_enabled: true, gmp_enabled: false });
        ctx.bindings_ctx.timer_ctx().assert_no_timers_installed();
        check_sent_done(&mut ctx.bindings_ctx, true);

        // Enable MLD but disable IPv6.
        //
        // Should do nothing.
        set_config(&mut ctx, TestConfig { ip_enabled: false, gmp_enabled: true });
        ctx.bindings_ctx.timer_ctx().assert_no_timers_installed();
        assert_matches!(ctx.bindings_ctx.take_ethernet_frames()[..], []);

        // Disable MLD but enable IPv6.
        //
        // Should do nothing.
        set_config(&mut ctx, TestConfig { ip_enabled: true, gmp_enabled: false });
        ctx.bindings_ctx.timer_ctx().assert_no_timers_installed();
        assert_matches!(ctx.bindings_ctx.take_ethernet_frames()[..], []);

        // Enable MLD.
        set_config(&mut ctx, TestConfig { ip_enabled: true, gmp_enabled: true });
        ctx.bindings_ctx
            .timer_ctx()
            .assert_timers_installed_range([(snmc_timer_id.clone(), range.clone())]);
        check_sent_report(&mut ctx.bindings_ctx, true);

        // Disable IPv6.
        set_config(&mut ctx, TestConfig { ip_enabled: false, gmp_enabled: true });
        ctx.bindings_ctx.timer_ctx().assert_no_timers_installed();
        check_sent_done(&mut ctx.bindings_ctx, false);

        // Enable IPv6.
        set_config(&mut ctx, TestConfig { ip_enabled: true, gmp_enabled: true });
        ctx.bindings_ctx.timer_ctx().assert_timers_installed_range([(snmc_timer_id, range)]);
        check_sent_report(&mut ctx.bindings_ctx, false);

        // Remove the device to cleanup all dangling references.
        core::mem::drop(device_id);
        ctx.core_api().device().remove_device(eth_device_id).into_removed();
    }
}
