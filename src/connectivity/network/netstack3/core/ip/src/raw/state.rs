// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Declares types and functionality related to the state of raw IP sockets.

use derivative::Derivative;
use lock_order::lock::{OrderedLockAccess, OrderedLockRef};
use net_types::ip::{Ip, IpVersionMarker};
use netstack3_base::{sync::RwLock, WeakDeviceIdentifier};
use packet_formats::ip::IpProtoExt;

use crate::internal::raw::{protocol::RawIpSocketProtocol, RawIpSocketsBindingsTypes};

/// State for a raw IP socket that can be modified, and is lock protected.
#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub struct RawIpSocketLockedState<I: Ip, D: WeakDeviceIdentifier> {
    /// The socket's bound device. When set, the socket will only be able to
    /// send/receive packets via this device.
    ///
    /// Held as a weak identifier, because binding a socket to a device should
    /// not obstruct removal of the device.
    pub(crate) bound_device: Option<D>,
    // TODO(https://fxbug.dev/42175797): Remove once IP fields are held.
    _delete_me: IpVersionMarker<I>,
}

/// State held by a raw IP socket.
pub struct RawIpSocketState<I: IpProtoExt, D: WeakDeviceIdentifier, BT: RawIpSocketsBindingsTypes> {
    /// The bindings state associated with this socket.
    external_state: BT::RawIpSocketState<I>,
    /// The IANA Internet Protocol of this socket.
    ///
    /// This field is specified at creation time and never changes.
    protocol: RawIpSocketProtocol<I>,
    // The locked socket state, accessible via the [`RawIpSocketStateContext`].
    locked_state: RwLock<RawIpSocketLockedState<I, D>>,
}

impl<I: IpProtoExt, D: WeakDeviceIdentifier, BT: RawIpSocketsBindingsTypes>
    RawIpSocketState<I, D, BT>
{
    pub(super) fn new(
        protocol: RawIpSocketProtocol<I>,
        external_state: BT::RawIpSocketState<I>,
    ) -> RawIpSocketState<I, D, BT> {
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
    /// Helper function to circumvent lock ordering, for tests.
    #[cfg(test)]
    pub(super) fn locked_state(&self) -> &RwLock<RawIpSocketLockedState<I, D>> {
        &self.locked_state
    }
}

impl<I: IpProtoExt, D: WeakDeviceIdentifier, BT: RawIpSocketsBindingsTypes>
    OrderedLockAccess<RawIpSocketLockedState<I, D>> for RawIpSocketState<I, D, BT>
{
    type Lock = RwLock<RawIpSocketLockedState<I, D>>;

    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.locked_state)
    }
}
