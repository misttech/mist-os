// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Packet filtering framework.

#![no_std]
#![warn(
    missing_docs,
    unreachable_patterns,
    clippy::useless_conversion,
    clippy::redundant_clone,
    clippy::precedence
)]

extern crate alloc;

mod actions;
mod api;
mod conntrack;
mod context;
mod logic;
mod matchers;
mod packets;
mod state;

use logic::nat::NatConfig;

/// A connection as tracked by conntrack.
pub type ConntrackConnection<I, A, BT> = conntrack::Connection<I, NatConfig<I, A>, BT>;

pub use actions::MarkAction;
pub use api::FilterApi;
pub use conntrack::{
    ConnectionDirection, Table, TransportProtocol, Tuple,
    WeakConnection as WeakConntrackConnection, WeakConnectionError,
};
pub use context::{
    FilterBindingsContext, FilterBindingsTypes, FilterContext, FilterIpContext, NatContext,
    SocketEgressFilterResult, SocketIngressFilterResult, SocketOpsFilter,
    SocketOpsFilterBindingContext,
};
pub use logic::{
    FilterHandler, FilterImpl, FilterTimerId, IngressVerdict, ProofOfEgressCheck, Verdict,
};
pub use matchers::{
    AddressMatcher, AddressMatcherType, InterfaceMatcher, InterfaceProperties, PacketMatcher,
    PortMatcher, TransportProtocolMatcher,
};
pub use packets::{
    DynTransportSerializer, DynamicTransportSerializer, FilterIpExt, ForwardedPacket, IcmpMessage,
    IpPacket, MaybeTransportPacket, MaybeTransportPacketMut, RawIpBody, TransportPacketSerializer,
    TxPacket,
};
pub use state::validation::{ValidRoutines, ValidationError};
pub use state::{
    Action, FilterIpMetadata, FilterMarkMetadata, Hook, IpRoutines, NatRoutines, Routine, Routines,
    Rule, State, TransparentProxy, UninstalledRoutine,
};

/// Testing-related utilities for use by other crates.
#[cfg(any(test, feature = "testutils"))]
pub mod testutil {
    pub use crate::logic::testutil::NoopImpl;
    pub use crate::packets::testutil::new_filter_egress_ip_packet;
    use net_types::ip::IpVersion;
    use packet::FragmentedByteSlice;

    use crate::{
        FilterIpExt, IpPacket, SocketEgressFilterResult, SocketIngressFilterResult, SocketOpsFilter,
    };
    use netstack3_base::socket::SocketCookie;
    use netstack3_base::{Marks, StrongDeviceIdentifier};

    #[cfg(test)]
    pub(crate) trait TestIpExt:
        crate::context::testutil::TestIpExt + crate::packets::testutil::internal::TestIpExt
    {
    }

    #[cfg(test)]
    impl<I> TestIpExt for I where
        I: crate::context::testutil::TestIpExt + crate::packets::testutil::internal::TestIpExt
    {
    }

    /// No-op implementation of `SocketOpsFilter`.
    pub struct NoOpSocketOpsFilter;

    impl<D: StrongDeviceIdentifier> SocketOpsFilter<D> for NoOpSocketOpsFilter {
        fn on_egress<I: FilterIpExt, P: IpPacket<I>>(
            &self,
            _packet: &P,
            _device: &D,
            _cookie: SocketCookie,
            _marks: &Marks,
        ) -> SocketEgressFilterResult {
            SocketEgressFilterResult::Pass { congestion: false }
        }

        fn on_ingress(
            &self,
            _ip_version: IpVersion,
            _packet: FragmentedByteSlice<'_, &[u8]>,
            _device: &D,
            _cookie: SocketCookie,
            _marks: &Marks,
        ) -> SocketIngressFilterResult {
            SocketIngressFilterResult::Accept
        }
    }
}
