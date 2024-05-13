// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Facilities backing raw IP sockets.

use net_types::ip::IpVersionMarker;

use crate::{
    ip::base::IpLayerIpExt,
    sync::{PrimaryRc, RwLock, StrongRc},
};

mod integration;

/// State for an IP socket that can be modified, and is lock protected.
struct RawIpSocketLockedState<I: IpLayerIpExt> {
    // TODO(https://fxbug.dev/42175797): Remove once IP fields are held.
    _delete_me: IpVersionMarker<I>,
}

/// State held by a raw IP socket.
struct RawIpSocketState<I: IpLayerIpExt> {
    // The locked socket state. accessible via the [`RawIpSocketStateContext`].
    locked_state: RwLock<RawIpSocketLockedState<I>>,
}

/// The owner of socket state.
// TODO(https://fxbug.dev/42175797): Use this type in the socket map.
#[allow(dead_code)]
struct PrimaryRawIpSocketId<I: IpLayerIpExt>(PrimaryRc<RawIpSocketState<I>>);

/// Reference to the state of a live socket.
struct RawIpSocketId<I: IpLayerIpExt>(StrongRc<RawIpSocketState<I>>);

/// Provides access to the [`RawIpSocketLockedState`] for a raw IP socket.
///
/// Implementations must ensure a proper lock ordering is adhered to.
// TODO(https://fxbug.dev/42175797): Use this trait to access socket state.
#[allow(dead_code)]
trait RawIpSocketStateContext<I: IpLayerIpExt> {
    fn with_locked_state<O, F: FnOnce(&RawIpSocketLockedState<I>) -> O>(
        &mut self,
        id: &RawIpSocketId<I>,
        cb: F,
    ) -> O;
    fn with_locked_state_mut<O, F: FnOnce(&mut RawIpSocketLockedState<I>) -> O>(
        &mut self,
        id: &RawIpSocketId<I>,
        cb: F,
    ) -> O;
}

/// The collection of all raw IP sockets installed in the system.
///
/// Implementations must ensure a proper lock ordering is adhered to.
#[derive(Default)]
pub struct RawIpSocketMap<I: IpLayerIpExt> {
    // TODO(https://fxbug.dev/42175797) Remove once IP specific fields are held.
    _delete_me: IpVersionMarker<I>,
}

/// Provides access to the `RawIpSocketMap` used by the system.
// TODO(https://fxbug.dev/42175797): Use this trait to access the socket map.
#[allow(dead_code)]
trait RawIpSocketMapContext<I: IpLayerIpExt> {
    fn with_socket_map<O, F: FnOnce(&RawIpSocketMap<I>) -> O>(&mut self, cb: F) -> O;
    fn with_socket_map_mut<O, F: FnOnce(&mut RawIpSocketMap<I>) -> O>(&mut self, cb: F) -> O;
}
