// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Structs containing the entire stack state.

use net_types::ip::{GenericOverIp, Ip, IpInvariant, Ipv4, Ipv6};

use crate::{
    api::CoreApi,
    context::{ContextProvider, CtxPair},
    device::{
        arp::ArpCounters, DeviceCounters, DeviceId, DeviceLayerState, EthernetDeviceCounters,
        PureIpDeviceCounters, WeakDeviceId,
    },
    ip::{
        self,
        device::nud::NudCounters,
        device::slaac::SlaacCounters,
        icmp::{IcmpState, NdpCounters},
        IpCounters, IpLayerIpExt, IpStateInner, Ipv4State, Ipv6State,
    },
    socket::datagram,
    sync::RwLock,
    transport::{self, tcp::TcpCounters, udp::UdpCounters, TransportLayerState},
    BindingsContext, BindingsTypes, CoreCtx,
};

/// A builder for [`StackState`].
#[derive(Default, Clone)]
pub(crate) struct StackStateBuilder {
    transport: transport::TransportStateBuilder,
    ipv4: ip::Ipv4StateBuilder,
    ipv6: ip::Ipv6StateBuilder,
}

impl StackStateBuilder {
    #[cfg(test)]
    /// Get the builder for the transport layer state.
    pub(crate) fn transport_builder(&mut self) -> &mut transport::TransportStateBuilder {
        &mut self.transport
    }

    #[cfg(test)]
    /// Get the builder for the IPv4 state.
    pub(crate) fn ipv4_builder(&mut self) -> &mut ip::Ipv4StateBuilder {
        &mut self.ipv4
    }

    /// Consume this builder and produce a `StackState`.
    pub(crate) fn build_with_ctx<BC: BindingsContext>(
        self,
        bindings_ctx: &mut BC,
    ) -> StackState<BC> {
        StackState {
            transport: self.transport.build_with_ctx(bindings_ctx),
            ipv4: self.ipv4.build(),
            ipv6: self.ipv6.build(),
            device: DeviceLayerState::new(),
        }
    }
}

/// The state associated with the network stack.
pub struct StackState<BT: BindingsTypes> {
    pub(crate) transport: TransportLayerState<BT>,
    pub(crate) ipv4: Ipv4State<DeviceId<BT>, BT>,
    pub(crate) ipv6: Ipv6State<DeviceId<BT>, BT>,
    pub(crate) device: DeviceLayerState<BT>,
}

impl<BT: BindingsTypes> StackState<BT> {
    /// Gets access to the API from a mutable reference to the bindings context.
    pub fn api<'a, BP: ContextProvider<Context = BT>>(
        &'a self,
        bindings_ctx: BP,
    ) -> CoreApi<'a, BP> {
        CoreApi::new(CtxPair { core_ctx: CoreCtx::new(self), bindings_ctx })
    }

    #[cfg(any(test, feature = "testutils"))]
    pub(crate) fn context(&self) -> crate::context::UnlockedCoreCtx<'_, BT> {
        crate::context::UnlockedCoreCtx::new(self)
    }

    pub(crate) fn ip_counters<I: IpLayerIpExt>(&self) -> &IpCounters<I> {
        I::map_ip(
            IpInvariant(self),
            |IpInvariant(state)| state.ipv4.as_ref().counters(),
            |IpInvariant(state)| state.ipv6.as_ref().counters(),
        )
    }

    pub(crate) fn filter<I: packet_formats::ip::IpExt>(
        &self,
    ) -> &RwLock<crate::filter::ValidState<I, BT::DeviceClass>> {
        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        struct Wrap<'a, I: packet_formats::ip::IpExt, DeviceClass>(
            &'a RwLock<crate::filter::ValidState<I, DeviceClass>>,
        );
        let Wrap(state) = I::map_ip(
            IpInvariant(self),
            |IpInvariant(state)| Wrap(state.ipv4.filter()),
            |IpInvariant(state)| Wrap(state.ipv6.filter()),
        );
        state
    }

    pub(crate) fn nud_counters<I: Ip>(&self) -> &NudCounters<I> {
        I::map_ip(
            IpInvariant(self),
            |IpInvariant(state)| state.device.nud_counters::<Ipv4>(),
            |IpInvariant(state)| state.device.nud_counters::<Ipv6>(),
        )
    }

    pub(crate) fn ndp_counters(&self) -> &NdpCounters {
        &self.ipv6.icmp().ndp_counters
    }

    pub(crate) fn device_counters(&self) -> &DeviceCounters {
        &self.device.counters()
    }

    pub(crate) fn ethernet_device_counters(&self) -> &EthernetDeviceCounters {
        &self.device.ethernet_counters()
    }

    pub(crate) fn pure_ip_device_counters(&self) -> &PureIpDeviceCounters {
        &self.device.pure_ip_counters()
    }

    pub(crate) fn arp_counters(&self) -> &ArpCounters {
        &self.device.arp_counters()
    }

    pub(crate) fn udp_counters<I: Ip>(&self) -> &UdpCounters<I> {
        &self.transport.udp_counters::<I>()
    }

    pub(crate) fn tcp_counters<I: Ip>(&self) -> &TcpCounters<I> {
        &self.transport.tcp_counters::<I>()
    }

    pub(crate) fn slaac_counters(&self) -> &SlaacCounters {
        &self.ipv6.slaac_counters()
    }

    pub(crate) fn inner_ip_state<I: IpLayerIpExt>(
        &self,
    ) -> &IpStateInner<I, BT::Instant, DeviceId<BT>> {
        I::map_ip((), |()| self.ipv4.inner(), |()| self.ipv6.inner())
    }

    pub(crate) fn inner_icmp_state<I: ip::IpExt + datagram::DualStackIpExt>(
        &self,
    ) -> &IcmpState<I, WeakDeviceId<BT>, BT> {
        I::map_ip((), |()| &self.ipv4.icmp().inner, |()| &self.ipv6.icmp().inner)
    }
}

impl<BC: BindingsContext> StackState<BC> {
    /// Create a new `StackState`.
    pub fn new(bindings_ctx: &mut BC) -> Self {
        StackStateBuilder::default().build_with_ctx(bindings_ctx)
    }
}
