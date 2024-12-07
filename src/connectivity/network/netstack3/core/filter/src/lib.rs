// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Packet filtering framework.

#![no_std]
#![warn(missing_docs)]

extern crate fakealloc as alloc;

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

pub use api::FilterApi;
pub use conntrack::{
    ConnectionDirection, Table, TransportProtocol, Tuple,
    WeakConnection as WeakConntrackConnection, WeakConnectionError,
};
pub use context::{
    FilterBindingsContext, FilterBindingsTypes, FilterContext, FilterIpContext, NatContext,
};
pub use logic::{
    FilterHandler, FilterImpl, FilterTimerId, IngressVerdict, ProofOfEgressCheck, Verdict,
};
pub use matchers::{
    AddressMatcher, AddressMatcherType, InterfaceMatcher, InterfaceProperties, PacketMatcher,
    PortMatcher, TransportProtocolMatcher,
};
pub use packets::{
    FilterIpExt, ForwardedPacket, IcmpMessage, IpPacket, MaybeTransportPacket,
    MaybeTransportPacketMut, RawIpBody, TransportPacketSerializer, TxPacket,
};
pub use state::validation::{ValidRoutines, ValidationError};
pub use state::{
    Action, FilterIpMetadata, Hook, IpRoutines, NatRoutines, Routine, Routines, Rule, State,
    TransparentProxy, UninstalledRoutine,
};

/// Testing-related utilities for use by other crates.
#[cfg(any(test, feature = "testutils"))]
pub mod testutil {
    pub use crate::logic::testutil::NoopImpl;

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
}
