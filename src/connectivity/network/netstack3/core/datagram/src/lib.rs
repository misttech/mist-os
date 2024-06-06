// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Netstack3 core Datagram sockets.
//!
//! This crate contains the shared base implementation between UDP and ICMP Echo
//! sockets.

#![no_std]
#![deny(missing_docs, unreachable_patterns, clippy::useless_conversion, clippy::redundant_clone)]

extern crate fakealloc as alloc;
extern crate fakestd as std;

#[path = "."]
mod internal {
    pub(super) mod datagram;
    pub(super) mod spec_context;
    pub(super) mod uninstantiable;
}

pub use internal::datagram::{
    close, collect_all_sockets, connect, create, disconnect_connected, get_bound_device, get_info,
    get_ip_hop_limits, get_ip_transparent, get_multicast_interface, get_multicast_loop,
    get_options_device, get_sharing, get_shutdown_connected, listen, send_conn, send_to,
    set_device, set_ip_transparent, set_multicast_interface, set_multicast_loop,
    set_multicast_membership, shutdown_connected, update_ip_hop_limit, update_sharing,
    with_other_stack_ip_options, with_other_stack_ip_options_and_default_hop_limits,
    with_other_stack_ip_options_mut, with_other_stack_ip_options_mut_if_unbound, BoundSocketState,
    BoundSocketStateType, BoundSockets, ConnInfo, ConnState, ConnectError,
    DatagramBoundStateContext, DatagramFlowId, DatagramSocketMapSpec, DatagramSocketOptions,
    DatagramSocketSet, DatagramSocketSpec, DatagramStateContext, DualStackConnState,
    DualStackConverter, DualStackDatagramBoundStateContext, DualStackIpExt, EitherIpSocket,
    ExpectedConnError, ExpectedUnboundError, InUseError, IpExt, IpOptions, ListenerInfo,
    MulticastInterfaceSelector, MulticastMembershipInterfaceSelector, NonDualStackConverter,
    NonDualStackDatagramBoundStateContext, ReferenceState, SendError, SendToError,
    SetMulticastMembershipError, SocketInfo, SocketState, StrongRc, WeakRc,
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
