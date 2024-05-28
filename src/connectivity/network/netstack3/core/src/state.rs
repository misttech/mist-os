// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Structs containing the entire stack state.

use net_types::ip::{Ip, IpInvariant, Ipv4, Ipv6};

use crate::{
    api::CoreApi,
    context::{BuildableCoreContext, ContextProvider, CoreTimerContext, CtxPair},
    device::{
        arp::ArpCounters, DeviceCounters, DeviceId, DeviceLayerState, EthernetDeviceCounters,
        PureIpDeviceCounters, WeakDeviceId,
    },
    ip::{
        self, icmp::IcmpState, nud::NudCounters, IpLayerIpExt, IpLayerTimerId, IpStateInner,
        Ipv4State, Ipv6State,
    },
    socket::datagram,
    time::TimerId,
    transport::{self, tcp::TcpCounters, udp::UdpCounters, TransportLayerState},
    BindingsContext, BindingsTypes, CoreCtx,
};

/// A builder for [`StackState`].
#[derive(Default, Clone)]
pub struct StackStateBuilder {
    transport: transport::TransportStateBuilder,
    ipv4: ip::Ipv4StateBuilder,
    ipv6: ip::Ipv6StateBuilder,
}

impl StackStateBuilder {
    /// Get the builder for the transport layer state.
    pub fn transport_builder(&mut self) -> &mut transport::TransportStateBuilder {
        &mut self.transport
    }

    /// Get the builder for the IPv4 state.
    pub fn ipv4_builder(&mut self) -> &mut ip::Ipv4StateBuilder {
        &mut self.ipv4
    }

    /// Consume this builder and produce a `StackState`.
    pub(crate) fn build_with_ctx<BC: BindingsContext>(
        self,
        bindings_ctx: &mut BC,
    ) -> StackState<BC> {
        StackState {
            transport: self.transport.build_with_ctx(bindings_ctx),
            ipv4: self.ipv4.build::<StackState<BC>, _, _>(bindings_ctx),
            ipv6: self.ipv6.build::<StackState<BC>, _, _>(bindings_ctx),
            device: DeviceLayerState::new(),
        }
    }
}

impl<BC: BindingsContext> BuildableCoreContext<BC> for StackState<BC> {
    type Builder = StackStateBuilder;
    fn build(bindings_ctx: &mut BC, builder: StackStateBuilder) -> Self {
        builder.build_with_ctx(bindings_ctx)
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

    pub(crate) fn nud_counters<I: Ip>(&self) -> &NudCounters<I> {
        I::map_ip(
            IpInvariant(self),
            |IpInvariant(state)| state.device.nud_counters::<Ipv4>(),
            |IpInvariant(state)| state.device.nud_counters::<Ipv6>(),
        )
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

    pub(crate) fn inner_ip_state<I: IpLayerIpExt>(&self) -> &IpStateInner<I, DeviceId<BT>, BT> {
        I::map_ip((), |()| &self.ipv4.inner, |()| &self.ipv6.inner)
    }

    pub(crate) fn inner_icmp_state<I: ip::IpExt + datagram::DualStackIpExt>(
        &self,
    ) -> &IcmpState<I, WeakDeviceId<BT>, BT> {
        I::map_ip((), |()| &self.ipv4.icmp.inner, |()| &self.ipv6.icmp.inner)
    }
}

// Stack state accessors for use in tests.
// We don't want bindings using this directly.
#[cfg(any(test, feature = "testutils"))]

impl<BT: BindingsTypes> StackState<BT> {
    /// Accessor for transport state.
    pub fn transport(&self) -> &TransportLayerState<BT> {
        &self.transport
    }
    /// Accessor for IPv4 state.
    pub fn ipv4(&self) -> &Ipv4State<DeviceId<BT>, BT> {
        &self.ipv4
    }
    /// Accessor for IPv6 state.
    pub fn ipv6(&self) -> &Ipv6State<DeviceId<BT>, BT> {
        &self.ipv6
    }
    /// Accessor for device state.
    pub fn device(&self) -> &DeviceLayerState<BT> {
        &self.device
    }
    /// Gets the core context.
    pub fn context(&self) -> crate::context::UnlockedCoreCtx<'_, BT> {
        crate::context::UnlockedCoreCtx::new(self)
    }
    /// Accessor for common IP state for `I`.
    pub fn common_ip<I: IpLayerIpExt>(&self) -> &IpStateInner<I, DeviceId<BT>, BT> {
        self.inner_ip_state::<I>()
    }
    /// Accessor for common ICMP state for `I`.
    pub fn common_icmp<I: ip::IpExt + datagram::DualStackIpExt>(
        &self,
    ) -> &IcmpState<I, WeakDeviceId<BT>, BT> {
        self.inner_icmp_state::<I>()
    }
}

impl<BC: BindingsContext> StackState<BC> {
    /// Create a new `StackState`.
    pub fn new(bindings_ctx: &mut BC) -> Self {
        StackStateBuilder::default().build_with_ctx(bindings_ctx)
    }
}

impl<BT: BindingsTypes> CoreTimerContext<IpLayerTimerId, BT> for StackState<BT> {
    fn convert_timer(timer: IpLayerTimerId) -> TimerId<BT> {
        timer.into()
    }
}
