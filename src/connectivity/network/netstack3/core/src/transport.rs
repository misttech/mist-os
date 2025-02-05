// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The transport layer.
//!
//! # Listeners and connections
//!
//! Some transport layer protocols (notably TCP and UDP) follow a common pattern
//! with respect to registering listeners and connections. There are some
//! subtleties here that are worth pointing out.
//!
//! ## Connections
//!
//! A connection has simpler semantics than a listener. It is bound to a single
//! local address and port and a single remote address and port. By virtue of
//! being bound to a local address, it is also bound to a local interface. This
//! means that, regardless of the entries in the forwarding table, all traffic
//! on that connection will always egress over the same interface. [^1] This
//! also means that, if the interface's address changes, any connections bound
//! to it are severed.
//!
//! ## Listeners
//!
//! A listener, on the other hand, can be bound to any number of local addresses
//! (although it is still always bound to a particular port). From the
//! perspective of this crate, there are two ways of registering a listener:
//! - By specifying one or more local addresses, the listener will be bound to
//!   each of those local addresses.
//! - By specifying zero local addresses, the listener will be bound to all
//!   addresses. These are referred to in our documentation as "wildcard
//!   listeners".
//!
//! The algorithm for figuring out what listener to deliver a packet to is as
//! follows: If there is any listener bound to the specific local address and
//! port addressed in the packet, deliver the packet to that listener.
//! Otherwise, if there is a wildcard listener bound the port addressed in the
//! packet, deliver the packet to that listener. This implies that if a listener
//! is removed which was bound to a particular local address, it can "uncover" a
//! wildcard listener bound to the same port, allowing traffic which would
//! previously have been delivered to the normal listener to now be delivered to
//! the wildcard listener.
//!
//! If desired, clients of this crate can implement a different mechanism for
//! registering listeners on all local addresses - enumerate every local
//! address, and then specify all of the local addresses when registering the
//! listener. This approach will not support shadowing, as a different listener
//! binding to the same port will explicitly conflict with the existing
//! listener, and will thus be rejected. In other words, from the perspective of
//! this crate's API, such listeners will appear like normal listeners that just
//! happen to bind all of the addresses, rather than appearing like wildcard
//! listeners.
//!
//! [^1]: It is an open design question as to whether incoming traffic on the
//!       connection will be accepted from a different interface. This is part
//!       of the "weak host model" vs "strong host model" discussion.

mod integration;

use derivative::Derivative;
use net_types::ip::{Ip, Ipv4, Ipv6};
use netstack3_base::{CoreTxMetadataContext, HandleableTimer, TimerHandler};
use netstack3_datagram as datagram;
use netstack3_device::WeakDeviceId;
use netstack3_icmp_echo::{IcmpSocketTxMetadata, IcmpSockets};
use netstack3_tcp::{self as tcp, TcpCounters, TcpState, TcpTimerId};
use netstack3_udp::{UdpCounters, UdpSocketTxMetadata, UdpState, UdpStateBuilder};

use crate::{BindingsContext, BindingsTypes, CoreCtx, IpExt};

/// A builder for transport layer state.
#[derive(Default, Clone)]
pub struct TransportStateBuilder {
    udp: UdpStateBuilder,
}

impl TransportStateBuilder {
    /// Get the builder for the UDP state.
    pub fn udp_builder(&mut self) -> &mut UdpStateBuilder {
        &mut self.udp
    }

    pub(crate) fn build_with_ctx<BC: BindingsContext>(
        self,
        bindings_ctx: &mut BC,
    ) -> TransportLayerState<BC> {
        let now = bindings_ctx.now();
        let mut rng = bindings_ctx.rng();
        TransportLayerState {
            udpv4: self.udp.clone().build(),
            udpv6: self.udp.build(),
            tcpv4: TcpState::new(now, &mut rng),
            tcpv6: TcpState::new(now, &mut rng),
            icmp_echo_v4: Default::default(),
            icmp_echo_v6: Default::default(),
        }
    }
}

/// The state associated with the transport layer.
pub struct TransportLayerState<BT: BindingsTypes> {
    udpv4: UdpState<Ipv4, WeakDeviceId<BT>, BT>,
    udpv6: UdpState<Ipv6, WeakDeviceId<BT>, BT>,
    tcpv4: TcpState<Ipv4, WeakDeviceId<BT>, BT>,
    tcpv6: TcpState<Ipv6, WeakDeviceId<BT>, BT>,
    icmp_echo_v4: IcmpSockets<Ipv4, WeakDeviceId<BT>, BT>,
    icmp_echo_v6: IcmpSockets<Ipv6, WeakDeviceId<BT>, BT>,
}

impl<BT: BindingsTypes> TransportLayerState<BT> {
    fn tcp_state<I: tcp::DualStackIpExt>(&self) -> &TcpState<I, WeakDeviceId<BT>, BT> {
        I::map_ip((), |()| &self.tcpv4, |()| &self.tcpv6)
    }

    fn udp_state<I: datagram::IpExt>(&self) -> &UdpState<I, WeakDeviceId<BT>, BT> {
        I::map_ip((), |()| &self.udpv4, |()| &self.udpv6)
    }

    pub(crate) fn icmp_echo_state<I: datagram::IpExt>(
        &self,
    ) -> &IcmpSockets<I, WeakDeviceId<BT>, BT> {
        I::map_ip((), |()| &self.icmp_echo_v4, |()| &self.icmp_echo_v6)
    }

    pub(crate) fn udp_counters<I: Ip>(&self) -> &UdpCounters<I> {
        I::map_ip((), |()| &self.udpv4.counters, |()| &self.udpv6.counters)
    }

    pub(crate) fn tcp_counters<I: Ip>(&self) -> &TcpCounters<I> {
        I::map_ip((), |()| &self.tcpv4.counters, |()| &self.tcpv6.counters)
    }
}

/// The identifier for timer events in the transport layer.
#[derive(Derivative)]
#[derivative(
    Clone(bound = ""),
    Eq(bound = ""),
    PartialEq(bound = ""),
    Hash(bound = ""),
    Debug(bound = "")
)]
pub(crate) enum TransportLayerTimerId<BT: BindingsTypes> {
    Tcp(TcpTimerId<WeakDeviceId<BT>, BT>),
}

impl<CC, BT> HandleableTimer<CC, BT> for TransportLayerTimerId<BT>
where
    BT: BindingsTypes,
    CC: TimerHandler<BT, TcpTimerId<WeakDeviceId<BT>, BT>>,
{
    fn handle(self, core_ctx: &mut CC, bindings_ctx: &mut BT, timer: BT::UniqueTimerId) {
        match self {
            TransportLayerTimerId::Tcp(id) => core_ctx.handle_timer(bindings_ctx, id, timer),
        }
    }
}

impl<BT: BindingsTypes> From<TcpTimerId<WeakDeviceId<BT>, BT>> for TransportLayerTimerId<BT> {
    fn from(id: TcpTimerId<WeakDeviceId<BT>, BT>) -> Self {
        TransportLayerTimerId::Tcp(id)
    }
}

/// The frame metadata type for frames traversing the stack.
#[derive(Derivative)]
#[derivative(Debug = "transparent", Debug(bound = ""), Default(bound = ""))]
#[cfg_attr(any(test, feature = "testutils"), derivative(PartialEq(bound = "")))]
pub struct TxMetadata<BT: BindingsTypes>(TxMetadataInner<BT>);

/// The internal metadata type.
///
/// This is split from [`TxMetadata`] so the outer type is opaque to bindings.
#[derive(Derivative)]
#[derivative(Debug(bound = ""), Default(bound = ""))]
#[cfg_attr(any(test, feature = "testutils"), derivative(PartialEq(bound = "")))]
enum TxMetadataInner<BT: BindingsTypes> {
    #[derivative(Default)]
    None,
    #[derivative(Debug = "transparent")]
    Udpv4(UdpSocketTxMetadata<Ipv4, WeakDeviceId<BT>, BT>),
    #[derivative(Debug = "transparent")]
    Udpv6(UdpSocketTxMetadata<Ipv6, WeakDeviceId<BT>, BT>),
    #[derivative(Debug = "transparent")]
    Icmpv4(IcmpSocketTxMetadata<Ipv4, WeakDeviceId<BT>, BT>),
    #[derivative(Debug = "transparent")]
    Icmpv6(IcmpSocketTxMetadata<Ipv6, WeakDeviceId<BT>, BT>),
}

impl<I: IpExt, L, BT: BindingsTypes>
    CoreTxMetadataContext<UdpSocketTxMetadata<I, WeakDeviceId<BT>, BT>, BT> for CoreCtx<'_, BT, L>
{
    fn convert_tx_meta(
        &self,
        tx_meta: UdpSocketTxMetadata<I, WeakDeviceId<BT>, BT>,
    ) -> TxMetadata<BT> {
        TxMetadata(I::map_ip_in(tx_meta, TxMetadataInner::Udpv4, TxMetadataInner::Udpv6))
    }
}

impl<I: IpExt, L, BT: BindingsTypes>
    CoreTxMetadataContext<IcmpSocketTxMetadata<I, WeakDeviceId<BT>, BT>, BT>
    for CoreCtx<'_, BT, L>
{
    fn convert_tx_meta(
        &self,
        tx_meta: IcmpSocketTxMetadata<I, WeakDeviceId<BT>, BT>,
    ) -> TxMetadata<BT> {
        TxMetadata(I::map_ip_in(tx_meta, TxMetadataInner::Icmpv4, TxMetadataInner::Icmpv6))
    }
}
