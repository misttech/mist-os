// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Netstack3 core TCP.
//!
//! This crate contains the TCP implementation for netstack3.

#![no_std]
#![warn(missing_docs, unreachable_patterns, clippy::useless_conversion, clippy::redundant_clone)]

extern crate fakealloc as alloc;

#[path = "."]
mod internal {
    pub(super) mod base;
    pub(super) mod buffer;
    pub(super) mod congestion;
    pub(super) mod rtt;
    pub(super) mod socket;
    pub(super) mod state;
    pub(super) mod uninstantiable;
}

pub use internal::base::{
    BufferSizes, ConnectionError, SocketOptions, TcpCounters, TcpCountersInner, TcpState,
    DEFAULT_FIN_WAIT2_TIMEOUT,
};
pub use internal::buffer::{Buffer, BufferLimits, IntoBuffers, ReceiveBuffer, SendBuffer};
pub use internal::socket::accept_queue::ListenerNotifier;
pub use internal::socket::isn::IsnGenerator;
pub use internal::socket::{
    AcceptError, BindError, BoundInfo, ConnectError, ConnectionInfo, DemuxState,
    DualStackDemuxIdConverter, DualStackIpExt, Ipv6Options, Ipv6SocketIdToIpv4DemuxIdConverter,
    ListenError, NoConnection, OriginalDestinationError, SetDeviceError, SetReuseAddrError,
    SocketAddr, SocketInfo, Sockets, TcpApi, TcpBindingsContext, TcpBindingsTypes, TcpContext,
    TcpDemuxContext, TcpDualStackContext, TcpIpTransportContext, TcpSocketId, TcpSocketSet,
    TcpSocketState, TcpTimerId, UnboundInfo, WeakTcpSocketId,
};

/// TCP test utilities.
#[cfg(any(test, feature = "testutils"))]
pub mod testutil {
    pub use crate::internal::buffer::testutil::{
        ClientBuffers, ProvidedBuffers, RingBuffer, TestSendBuffer, WriteBackClientBuffers,
    };
}
