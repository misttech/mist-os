// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Declares types and functionality related to the state of raw IP sockets.

use lock_order::lock::{OrderedLockAccess, OrderedLockRef};
use net_types::ip::{Ip, IpVersionMarker};
use packet_formats::ip::IpProtoExt;

use crate::{
    ip::raw::{protocol::RawIpSocketProtocol, RawIpSocketsBindingsTypes},
    sync::RwLock,
};

/// State for a raw IP socket that can be modified, and is lock protected.
#[derive(Default)]
pub(super) struct RawIpSocketLockedState<I: Ip> {
    // TODO(https://fxbug.dev/42175797): Remove once IP fields are held.
    _delete_me: IpVersionMarker<I>,
}

/// State held by a raw IP socket.
pub(super) struct RawIpSocketState<I: IpProtoExt, BT: RawIpSocketsBindingsTypes> {
    /// The bindings state associated with this socket.
    external_state: BT::RawIpSocketState<I>,
    /// The IANA Internet Protocol of this socket.
    ///
    /// This field is specified at creation time and never changes.
    protocol: RawIpSocketProtocol<I>,
    // The locked socket state, accessible via the [`RawIpSocketStateContext`].
    locked_state: RwLock<RawIpSocketLockedState<I>>,
}

impl<I: IpProtoExt, BT: RawIpSocketsBindingsTypes> RawIpSocketState<I, BT> {
    pub(super) fn new(
        protocol: RawIpSocketProtocol<I>,
        external_state: BT::RawIpSocketState<I>,
    ) -> RawIpSocketState<I, BT> {
        RawIpSocketState { external_state, protocol, locked_state: Default::default() }
    }
    pub(super) fn protocol(&self) -> &RawIpSocketProtocol<I> {
        &self.protocol
    }
    pub(super) fn external_state(&self) -> &BT::RawIpSocketState<I> {
        &self.external_state
    }
    pub(super) fn into_external_state(self) -> BT::RawIpSocketState<I> {
        let RawIpSocketState { protocol: _, locked_state: _, external_state } = self;
        external_state
    }
}

impl<I: IpProtoExt, BT: RawIpSocketsBindingsTypes> OrderedLockAccess<RawIpSocketLockedState<I>>
    for RawIpSocketState<I, BT>
{
    type Lock = RwLock<RawIpSocketLockedState<I>>;

    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.locked_state)
    }
}
