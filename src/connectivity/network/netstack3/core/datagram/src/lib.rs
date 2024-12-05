// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Netstack3 core Datagram sockets.
//!
//! This crate contains the shared base implementation between UDP and ICMP Echo
//! sockets.

#![no_std]
#![warn(missing_docs, unreachable_patterns, clippy::useless_conversion, clippy::redundant_clone)]

extern crate fakealloc as alloc;
extern crate fakestd as std;

#[path = "."]
mod internal {
    pub(super) mod datagram;
    pub(super) mod spec_context;
    pub(super) mod uninstantiable;
}

pub use internal::datagram::{
    BoundSocketState, BoundSocketStateType, BoundSockets, ConnInfo, ConnState, ConnectError,
    DatagramApi, DatagramBoundStateContext, DatagramFlowId, DatagramIpSpecificSocketOptions,
    DatagramSocketMapSpec, DatagramSocketSet, DatagramSocketSpec, DatagramStateContext,
    DualStackConnState, DualStackConverter, DualStackDatagramBoundStateContext, DualStackIpExt,
    EitherIpSocket, ExpectedConnError, ExpectedUnboundError, InUseError, IpExt, IpOptions,
    ListenerInfo, MulticastInterfaceSelector, MulticastMembershipInterfaceSelector,
    NonDualStackConverter, NonDualStackDatagramBoundStateContext, ReferenceState, SendError,
    SendToError, SetMulticastMembershipError, SocketInfo, SocketState, StrongRc, WeakRc,
    WrapOtherStackIpOptions, WrapOtherStackIpOptionsMut,
};
pub use internal::spec_context::{
    DatagramSpecBoundStateContext, DatagramSpecStateContext,
    DualStackDatagramSpecBoundStateContext, NonDualStackDatagramSpecBoundStateContext,
};

/// Datagram socket test utilities.
#[cfg(any(test, feature = "testutils"))]
pub mod testutil {
    pub use crate::internal::datagram::create_primary_id;
    pub use crate::internal::datagram::testutil::setup_fake_ctx_with_dualstack_conn_addrs;
}
