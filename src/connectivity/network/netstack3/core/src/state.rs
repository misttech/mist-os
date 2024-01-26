// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Structs containing the entire stack state.

use net_types::ip::{GenericOverIp, Ip, IpInvariant};

use crate::{
    api::CoreApi,
    context::{ContextProvider, CtxPair},
    device::{arp::ArpCounters, DeviceCounters, DeviceId, DeviceLayerState},
    ip::{
        self,
        device::slaac::SlaacCounters,
        icmp::{IcmpRxCounters, IcmpTxCounters, NdpCounters},
        IpCounters, Ipv4State, Ipv6State,
    },
    sync::RwLock,
    transport::{self, udp::UdpCounters, TransportLayerState},
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
    pub(crate) ipv4: Ipv4State<BT::Instant, DeviceId<BT>, BT::DeviceClass>,
    pub(crate) ipv6: Ipv6State<BT::Instant, DeviceId<BT>, BT::DeviceClass>,
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

    pub(crate) fn ip_counters<I: Ip>(&self) -> &IpCounters<I> {
        I::map_ip(
            IpInvariant(self),
            |IpInvariant(state)| state.ipv4.as_ref().counters(),
            |IpInvariant(state)| state.ipv6.as_ref().counters(),
        )
    }

    pub(crate) fn ipv4(&self) -> &Ipv4State<BT::Instant, DeviceId<BT>, BT::DeviceClass> {
        &self.ipv4
    }

    pub(crate) fn ipv6(&self) -> &Ipv6State<BT::Instant, DeviceId<BT>, BT::DeviceClass> {
        &self.ipv6
    }

    pub(crate) fn filter<I: packet_formats::ip::IpExt>(
        &self,
    ) -> &RwLock<crate::filter::State<I, BT::DeviceClass>> {
        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        struct Wrap<'a, I: packet_formats::ip::IpExt, DeviceClass>(
            &'a RwLock<crate::filter::State<I, DeviceClass>>,
        );
        let Wrap(state) = I::map_ip(
            IpInvariant(self),
            |IpInvariant(state)| Wrap(state.ipv4.filter()),
            |IpInvariant(state)| Wrap(state.ipv6.filter()),
        );
        state
    }

    pub(crate) fn icmp_tx_counters<I: Ip>(&self) -> &IcmpTxCounters<I> {
        I::map_ip(
            IpInvariant(self),
            |IpInvariant(state)| state.ipv4.icmp_tx_counters(),
            |IpInvariant(state)| state.ipv6.icmp_tx_counters(),
        )
    }

    pub(crate) fn icmp_rx_counters<I: Ip>(&self) -> &IcmpRxCounters<I> {
        I::map_ip(
            IpInvariant(self),
            |IpInvariant(state)| state.ipv4.icmp_rx_counters(),
            |IpInvariant(state)| state.ipv6.icmp_rx_counters(),
        )
    }

    pub(crate) fn ndp_counters(&self) -> &NdpCounters {
        &self.ipv6.ndp_counters()
    }

    pub(crate) fn device_counters(&self) -> &DeviceCounters {
        &self.device.counters()
    }

    pub(crate) fn arp_counters(&self) -> &ArpCounters {
        &self.device.arp_counters()
    }

    pub(crate) fn udp_counters<I: Ip>(&self) -> &UdpCounters<I> {
        &self.transport.udp_counters::<I>()
    }

    pub(crate) fn slaac_counters(&self) -> &SlaacCounters {
        &self.ipv6.slaac_counters()
    }
}
